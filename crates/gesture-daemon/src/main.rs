use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use gesture_actions::{default_action_registry, default_condition_registry};
use gesture_core::{Config, Engine, InputEvent};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::RwLock,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about = "GestureForge event routing daemon")]
struct Args {
    /// Path to the versioned TOML configuration.
    #[arg(long, env = "GESTURE_FORGE_CONFIG", default_value_os_t = default_config_path())]
    config: PathBuf,

    /// Override the Unix socket path from the configuration.
    #[arg(long, env = "GESTURE_FORGE_SOCKET")]
    socket: Option<PathBuf>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

struct Runtime {
    engine: RwLock<Engine>,
    actions: gesture_core::ActionRegistry,
    conditions: gesture_core::ConditionRegistry,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(config.runtime.log_level.clone())),
        )
        .init();

    let actions = default_action_registry(config.security.allow_command_actions)?;
    let conditions = default_condition_registry()?;
    let engine = Engine::new(config.clone())?;
    engine.validate_providers(&actions, &conditions)?;

    let runtime = Arc::new(Runtime {
        engine: RwLock::new(engine),
        actions,
        conditions,
    });

    let socket_path = args
        .socket
        .or_else(|| nonempty_path(&config.runtime.socket_path))
        .unwrap_or_else(default_socket_path);

    prepare_socket(&socket_path).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind socket {}", socket_path.display()))?;
    set_private_socket_permissions(&socket_path)?;

    spawn_config_watcher(args.config.clone(), Arc::clone(&runtime));

    info!(socket = %socket_path.display(), config = %args.config.display(), "GestureForge daemon started");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result.context("failed to accept Unix socket connection")?;
                let runtime = Arc::clone(&runtime);
                tokio::spawn(async move {
                    if let Err(error) = handle_client(stream, runtime).await {
                        warn!(%error, "client connection failed");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                info!("received shutdown signal");
                break;
            }
        }
    }

    let _ = tokio::fs::remove_file(&socket_path).await;
    Ok(())
}

async fn handle_client(stream: UnixStream, runtime: Arc<Runtime>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<InputEvent>(&line) {
            Ok(event) => {
                let engine = runtime.engine.read().await;
                match engine
                    .dispatch(&event, &runtime.actions, &runtime.conditions)
                    .await
                {
                    Ok(report) => serde_json::to_vec(&report)?,
                    Err(error) => serde_json::to_vec(&ErrorResponse {
                        error: error.to_string(),
                    })?,
                }
            }
            Err(error) => serde_json::to_vec(&ErrorResponse {
                error: format!("invalid event JSON: {error}"),
            })?,
        };

        writer.write_all(&response).await?;
        writer.write_all(b"\n").await?;
    }

    Ok(())
}

fn spawn_config_watcher(path: PathBuf, runtime: Arc<Runtime>) {
    tokio::task::spawn_blocking(move || {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(sender) {
            Ok(watcher) => watcher,
            Err(error) => {
                error!(%error, "failed to create configuration watcher");
                return;
            }
        };

        let watch_target = path.parent().unwrap_or_else(|| Path::new("."));
        if let Err(error) = watcher.watch(watch_target, RecursiveMode::NonRecursive) {
            error!(%error, path = %watch_target.display(), "failed to watch configuration directory");
            return;
        }

        while let Ok(result) = receiver.recv() {
            let event = match result {
                Ok(event) => event,
                Err(error) => {
                    warn!(%error, "configuration watcher error");
                    continue;
                }
            };

            if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
                || !event.paths.iter().any(|changed| changed == &path)
            {
                continue;
            }

            std::thread::sleep(Duration::from_millis(100));
            match Config::load(&path)
                .and_then(Engine::new)
                .and_then(|engine| {
                    engine.validate_providers(&runtime.actions, &runtime.conditions)?;
                    Ok(engine)
                })
            {
                Ok(engine) => {
                    *runtime.engine.blocking_write() = engine;
                    info!(path = %path.display(), "configuration reloaded");
                }
                Err(error) => {
                    warn!(%error, path = %path.display(), "configuration reload rejected; keeping previous configuration");
                }
            }
        }
    });
}

async fn prepare_socket(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("failed to remove stale socket {}", path.display())),
    }
    Ok(())
}

fn default_config_path() -> PathBuf {
    config_home().join("gesture-forge/config.toml")
}

fn default_socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let user = env::var("USER").unwrap_or_else(|_| "user".to_owned());
            env::temp_dir().join(format!("gesture-forge-{user}"))
        })
        .join("gesture-forge.sock")
}

fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    (!value.trim().is_empty()).then(|| PathBuf::from(value))
}


#[cfg(unix)]
fn set_private_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure socket {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_socket_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
