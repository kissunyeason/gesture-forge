use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use gesture_actions::{default_action_registry_with_security, default_condition_registry};
use gesture_core::{Config, DispatchReport, Engine, InputEvent, SecurityConfig};
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
    state: RwLock<RuntimeState>,
}

struct RuntimeState {
    engine: Engine,
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

    let runtime = Arc::new(Runtime {
        state: RwLock::new(build_runtime_state(config.clone())?),
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
    let mut active_drag = None;

    let connection_result: Result<()> = async {
        while let Some(line) = lines.next_line().await? {
            let response = match serde_json::from_str::<InputEvent>(&line) {
                Ok(event) => match dispatch_event(&event, &runtime).await {
                    Ok(report) => {
                        track_client_drag(&mut active_drag, &event);
                        serde_json::to_vec(&report)?
                    }
                    Err(error) => serde_json::to_vec(&ErrorResponse {
                        error: error.to_string(),
                    })?,
                },
                Err(error) => serde_json::to_vec(&ErrorResponse {
                    error: format!("invalid event JSON: {error}"),
                })?,
            };

            writer.write_all(&response).await?;
            writer.write_all(b"\n").await?;
        }
        Ok(())
    }
    .await;

    let cleanup_result = cancel_client_drag(active_drag, &runtime).await;
    match (connection_result, cleanup_result) {
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "client connection failed; drag cleanup also failed: {cleanup_error}"
        )),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("failed to cancel drag after client disconnect"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn dispatch_event(event: &InputEvent, runtime: &Runtime) -> Result<DispatchReport> {
    let state = runtime.state.read().await;
    state
        .engine
        .dispatch(event, &state.actions, &state.conditions)
        .await
}

fn track_client_drag(active: &mut Option<InputEvent>, event: &InputEvent) {
    if event.family != "touchpad.drag" || drag_stream_id(event).is_none() {
        return;
    }

    match event.phase.as_str() {
        "begin" => *active = Some(event.clone()),
        "update" => {
            if active.as_ref().and_then(drag_stream_id) == drag_stream_id(event) {
                *active = Some(event.clone());
            }
        }
        "end" | "cancel" if active.as_ref().and_then(drag_stream_id) == drag_stream_id(event) => {
            *active = None;
        }
        _ => {}
    }
}

async fn cancel_client_drag(active: Option<InputEvent>, runtime: &Runtime) -> Result<()> {
    let Some(active) = active else {
        return Ok(());
    };
    let cancel = drag_cancel_event(&active);
    let report = dispatch_event(&cancel, runtime)
        .await
        .context("daemon rejected disconnect cleanup")?;
    if let Some(failure) = dispatch_failure(&report) {
        anyhow::bail!("disconnect cleanup action failed: {failure}");
    }
    Ok(())
}

fn dispatch_failure(report: &DispatchReport) -> Option<String> {
    report.bindings.iter().find_map(|binding| {
        binding.outcomes.iter().find_map(|outcome| {
            (!outcome.success).then(|| {
                format!(
                    "binding {:?}, action {}.{}: {}",
                    binding.id,
                    outcome.provider,
                    outcome.action,
                    outcome.message.as_deref().unwrap_or("unknown failure")
                )
            })
        })
    })
}

fn drag_cancel_event(active: &InputEvent) -> InputEvent {
    let mut cancel = InputEvent::new("touchpad.drag", "cancel");
    cancel.fingers = active.fingers;
    cancel.direction = active.direction.clone();
    cancel.values = active.values.clone();
    cancel.values.insert("dx".to_owned(), 0.0);
    cancel.values.insert("dy".to_owned(), 0.0);
    cancel.labels = active.labels.clone();
    cancel.context = active.context.clone();
    cancel
}

fn drag_stream_id(event: &InputEvent) -> Option<&str> {
    event
        .labels
        .get("recognition.stream_id")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn build_runtime_state(config: Config) -> Result<RuntimeState> {
    let state = build_runtime_state_unvalidated(config)?;
    state
        .engine
        .validate_providers(&state.actions, &state.conditions)?;
    Ok(state)
}

fn build_runtime_state_unvalidated(config: Config) -> Result<RuntimeState> {
    let actions = default_action_registry_with_security(
        config.security.allow_command_actions,
        config.security.allow_uinput_actions,
    )?;
    let conditions = default_condition_registry()?;
    let engine = Engine::new(config)?;
    Ok(RuntimeState {
        engine,
        actions,
        conditions,
    })
}

fn security_became_more_restrictive(previous: &SecurityConfig, next: &SecurityConfig) -> bool {
    (previous.allow_command_actions && !next.allow_command_actions)
        || (previous.allow_uinput_actions && !next.allow_uinput_actions)
}

fn fail_closed_security(previous: &SecurityConfig, next: &SecurityConfig) -> SecurityConfig {
    SecurityConfig {
        allow_command_actions: previous.allow_command_actions && next.allow_command_actions,
        allow_uinput_actions: previous.allow_uinput_actions && next.allow_uinput_actions,
    }
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
            let config = match Config::load(&path) {
                Ok(config) => config,
                Err(error) => {
                    warn!(%error, path = %path.display(), "configuration reload rejected; keeping previous configuration");
                    continue;
                }
            };
            let previous_security = {
                let state = runtime.state.blocking_read();
                state.engine.config().security.clone()
            };

            match build_runtime_state(config.clone()) {
                Ok(state) => {
                    *runtime.state.blocking_write() = state;
                    info!(path = %path.display(), "configuration reloaded");
                }
                Err(error)
                    if security_became_more_restrictive(&previous_security, &config.security) =>
                {
                    let mut fail_closed_config = config;
                    fail_closed_config.security =
                        fail_closed_security(&previous_security, &fail_closed_config.security);
                    match build_runtime_state_unvalidated(fail_closed_config) {
                        Ok(state) => {
                            *runtime.state.blocking_write() = state;
                            warn!(
                                %error,
                                path = %path.display(),
                                "configuration provider validation failed, but stricter security settings were applied fail-closed"
                            );
                        }
                        Err(fallback_error) => {
                            warn!(
                                %error,
                                %fallback_error,
                                path = %path.display(),
                                "configuration reload rejected; keeping previous configuration"
                            );
                        }
                    }
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
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to remove stale socket {}", path.display()))
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn drag_event(stream_id: &str, phase: &str) -> InputEvent {
        let mut event = InputEvent::new("touchpad.drag", phase);
        event.fingers = Some(3);
        event.values.insert("dx".to_owned(), 4.0);
        event.values.insert("dy".to_owned(), -2.0);
        event
            .labels
            .insert("recognition.stream_id".to_owned(), stream_id.to_owned());
        event
    }

    #[test]
    fn tracks_only_the_matching_client_drag_stream() {
        let mut active = None;
        track_client_drag(&mut active, &drag_event("stream-a", "begin"));
        track_client_drag(&mut active, &drag_event("stream-b", "update"));
        assert_eq!(active.as_ref().and_then(drag_stream_id), Some("stream-a"));
        track_client_drag(&mut active, &drag_event("stream-b", "cancel"));
        assert!(active.is_some());
        track_client_drag(&mut active, &drag_event("stream-a", "end"));
        assert!(active.is_none());
    }

    #[test]
    fn detects_security_restrictions_that_must_apply_fail_closed() {
        let previous = SecurityConfig {
            allow_command_actions: true,
            allow_uinput_actions: true,
        };
        let commands_disabled = SecurityConfig {
            allow_command_actions: false,
            allow_uinput_actions: true,
        };
        let uinput_disabled = SecurityConfig {
            allow_command_actions: true,
            allow_uinput_actions: false,
        };
        let unchanged = previous.clone();
        let mixed_change = SecurityConfig {
            allow_command_actions: false,
            allow_uinput_actions: true,
        };
        let mixed_previous = SecurityConfig {
            allow_command_actions: true,
            allow_uinput_actions: false,
        };

        assert!(security_became_more_restrictive(
            &previous,
            &commands_disabled
        ));
        assert!(security_became_more_restrictive(
            &previous,
            &uinput_disabled
        ));
        assert!(!security_became_more_restrictive(&previous, &unchanged));

        let effective = fail_closed_security(&mixed_previous, &mixed_change);
        assert!(!effective.allow_command_actions);
        assert!(!effective.allow_uinput_actions);
    }

    #[test]
    fn disconnect_cancel_preserves_stream_and_clears_motion_delta() {
        let active = drag_event("stream-a", "update");
        let cancel = drag_cancel_event(&active);
        assert_eq!(cancel.phase, "cancel");
        assert_eq!(drag_stream_id(&cancel), Some("stream-a"));
        assert_eq!(cancel.values.get("dx"), Some(&0.0));
        assert_eq!(cancel.values.get("dy"), Some(&0.0));
    }
}
