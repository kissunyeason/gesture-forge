use std::{
    collections::BTreeMap,
    env,
    fs::File as StdFile,
    io::{BufRead, BufReader as StdBufReader},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use gesture_actions::{default_action_registry, default_condition_registry};
use gesture_core::{Config, Engine, InputEvent};
use gesture_device::{
    enumerate_devices, DeviceInfo, EvdevObserver, RawInputEvent, TouchFrame, TouchFrameTracker,
};
use gesture_recognition::{GestureRecognizer, RecognizerConfig};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "GestureForge input, recognition, and configuration tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse the configuration and validate all registered providers.
    Validate(ConfigArgs),
    /// Print a summary of configured bindings.
    Inspect(ConfigArgs),
    /// Send a normalized event to a running GestureForge daemon.
    Simulate(SimulateArgs),
    /// List readable Linux evdev input devices.
    Devices(DevicesArgs),
    /// Observe raw events from one device without grabbing it.
    Monitor(MonitorArgs),
    /// Convert live protocol-B events into normalized touch frames.
    Frames(FramesArgs),
    /// Record raw evdev events as replayable JSON Lines.
    Record(RecordArgs),
    /// Replay a JSON Lines recording through the touch-frame tracker.
    Replay(ReplayArgs),
    /// Recognize live touchpad gestures from normalized frames.
    Gestures(GesturesArgs),
    /// Recognize gestures from a raw JSON Lines recording.
    Recognize(RecognizeArgs),
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[arg(long, env = "GESTURE_FORGE_CONFIG", default_value_os_t = default_config_path())]
    config: PathBuf,
}

#[derive(Debug, Args)]
struct SimulateArgs {
    #[arg(long, env = "GESTURE_FORGE_SOCKET", default_value_os_t = default_socket_path())]
    socket: PathBuf,
    #[arg(long)]
    family: String,
    #[arg(long, default_value = "end")]
    phase: String,
    #[arg(long)]
    fingers: Option<u8>,
    #[arg(long)]
    direction: Option<String>,
    /// Numeric metric in key=value form. May be repeated.
    #[arg(long = "value", value_parser = parse_metric)]
    values: Vec<(String, f64)>,
    /// String label in key=value form. May be repeated.
    #[arg(long = "label", value_parser = parse_label)]
    labels: Vec<(String, String)>,
    #[arg(long)]
    app_id: Option<String>,
    #[arg(long)]
    window_title: Option<String>,
}

#[derive(Debug, Args)]
struct DevicesArgs {
    /// Emit a JSON array instead of the human-readable table.
    #[arg(long)]
    json: bool,
    /// Show only devices inferred to be touchpads.
    #[arg(long)]
    touchpads_only: bool,
}

#[derive(Debug, Args)]
struct MonitorArgs {
    /// Input event node, for example /dev/input/event8.
    #[arg(long)]
    device: PathBuf,
    /// Emit one JSON object per raw event.
    #[arg(long)]
    json: bool,
    /// Stop after this many events. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Stop when no event arrives for this many seconds. Zero disables it.
    #[arg(long, default_value_t = 10)]
    idle_timeout: u64,
}

#[derive(Debug, Args)]
struct FramesArgs {
    /// Input event node, for example /dev/input/event8.
    #[arg(long)]
    device: PathBuf,
    /// Emit one JSON object per normalized frame.
    #[arg(long)]
    json: bool,
    /// Stop after this many normalized frames. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Stop when no raw event arrives for this many seconds. Zero disables it.
    #[arg(long, default_value_t = 10)]
    idle_timeout: u64,
}

#[derive(Debug, Args)]
struct RecordArgs {
    /// Input event node, for example /dev/input/event8.
    #[arg(long)]
    device: PathBuf,
    /// Temporarily grab the device so desktop gestures do not run while recording.
    #[arg(long, visible_alias = "grab")]
    exclusive: bool,
    /// Destination JSON Lines file.
    #[arg(long)]
    output: PathBuf,
    /// Stop after this many raw events. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Stop when no event arrives for this many seconds. Zero disables it.
    #[arg(long, default_value_t = 10)]
    idle_timeout: u64,
}

#[derive(Debug, Args)]
struct ReplayArgs {
    /// JSON Lines file created by `gesture-forge record` or `monitor --json`.
    #[arg(long)]
    input: PathBuf,
    /// Emit one JSON object per normalized frame.
    #[arg(long)]
    json: bool,
    /// Stop after this many normalized frames. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
}

#[derive(Debug, Args)]
struct GesturesArgs {
    /// Input event node, for example /dev/input/event8.
    #[arg(long)]
    device: PathBuf,
    /// Temporarily grab the device while recognizing gestures.
    #[arg(long, visible_alias = "grab")]
    exclusive: bool,
    /// Optional recognizer TOML. Built-in v0.4 defaults are used when omitted.
    #[arg(long, env = "GESTURE_FORGE_RECOGNIZER_CONFIG")]
    recognizer_config: Option<PathBuf>,
    /// Emit one JSON object per recognized gesture.
    #[arg(long)]
    json: bool,
    /// Stop after this many recognized gestures. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Stop when no raw event arrives for this many seconds. Zero disables it.
    #[arg(long, default_value_t = 10)]
    idle_timeout: u64,
}

#[derive(Debug, Args)]
struct RecognizeArgs {
    /// JSON Lines file created by `gesture-forge record` or `monitor --json`.
    #[arg(long)]
    input: PathBuf,
    /// Optional recognizer TOML. Built-in v0.4 defaults are used when omitted.
    #[arg(long, env = "GESTURE_FORGE_RECOGNIZER_CONFIG")]
    recognizer_config: Option<PathBuf>,
    /// Emit one JSON object per recognized gesture.
    #[arg(long)]
    json: bool,
    /// Stop after this many recognized gestures. Zero means unlimited.
    #[arg(long, default_value_t = 0)]
    limit: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Validate(args) => validate(args),
        Command::Inspect(args) => inspect(args),
        Command::Simulate(args) => simulate(args).await,
        Command::Devices(args) => devices(args),
        Command::Monitor(args) => monitor(args).await,
        Command::Frames(args) => frames(args).await,
        Command::Record(args) => record(args).await,
        Command::Replay(args) => replay(args),
        Command::Gestures(args) => gestures(args).await,
        Command::Recognize(args) => recognize(args),
    }
}

fn validate(args: ConfigArgs) -> Result<()> {
    let config = Config::load(&args.config)?;
    let actions = default_action_registry(config.security.allow_command_actions)?;
    let conditions = default_condition_registry()?;
    let engine = Engine::new(config)?;
    engine.validate_providers(&actions, &conditions)?;
    println!("configuration is valid: {}", args.config.display());
    Ok(())
}

fn inspect(args: ConfigArgs) -> Result<()> {
    let config = Config::load(&args.config)?;
    println!("version: {}", config.version);
    println!("bindings: {}", config.bindings.len());
    for binding in config.bindings {
        println!(
            "- {} [{}] priority={} consume={} trigger={} actions={}",
            binding.id,
            if binding.enabled {
                "enabled"
            } else {
                "disabled"
            },
            binding.priority,
            binding.consume,
            binding.trigger.family,
            binding
                .actions
                .iter()
                .map(|action| format!("{}.{}", action.provider, action.action))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

async fn simulate(args: SimulateArgs) -> Result<()> {
    let mut event = InputEvent::new(args.family, args.phase);
    event.fingers = args.fingers;
    event.direction = args.direction;
    event.values = args.values.into_iter().collect::<BTreeMap<_, _>>();
    event.labels = args.labels.into_iter().collect::<BTreeMap<_, _>>();
    event.context.app_id = args.app_id;
    event.context.window_title = args.window_title;

    let mut stream = UnixStream::connect(&args.socket)
        .await
        .with_context(|| format!("failed to connect to {}", args.socket.display()))?;
    let payload = serde_json::to_vec(&event)?;
    stream.write_all(&payload).await?;
    stream.write_all(b"\n").await?;

    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).await?;
    print!("{response}");
    Ok(())
}

fn devices(args: DevicesArgs) -> Result<()> {
    let devices: Vec<DeviceInfo> = enumerate_devices()
        .into_iter()
        .filter(|device| !args.touchpads_only || device.is_touchpad_candidate())
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&devices)?);
        return Ok(());
    }

    if devices.is_empty() {
        println!("no readable input devices found");
        println!("check membership of the input group and /dev/input permissions");
        return Ok(());
    }

    for device in devices {
        println!(
            "{}\t{:?}\t{}",
            device.path.display(),
            device.class,
            device.name
        );
        println!(
            "  multitouch={} pointer={} direct={} buttonpad={} abs={} rel={}",
            device.capabilities.multitouch_positions,
            device.capabilities.pointer_property,
            device.capabilities.direct_property,
            device.capabilities.buttonpad_property,
            device.capabilities.absolute_axes,
            device.capabilities.relative_axes,
        );
    }
    Ok(())
}

async fn monitor(args: MonitorArgs) -> Result<()> {
    let mut observer = EvdevObserver::open(&args.device)?;
    eprintln!(
        "observing {} ({}) in read-only mode; the device is not grabbed",
        observer.info().path.display(),
        observer.info().name
    );

    let mut count = 0usize;
    loop {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let Some(event) = next_raw_event(&mut observer, args.idle_timeout).await? else {
            break;
        };
        count += 1;

        if args.json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            println!(
                "{:>5} {:<24} code={:<4} value={:<8} {}",
                count, event.event_type, event.code, event.value, event.summary
            );
        }
    }

    Ok(())
}

async fn frames(args: FramesArgs) -> Result<()> {
    let mut observer = EvdevObserver::open(&args.device)?;
    let mut tracker = TouchFrameTracker::new();
    let mut count = 0usize;

    eprintln!(
        "building normalized frames from {} ({}); the device is not grabbed",
        observer.info().path.display(),
        observer.info().name
    );

    loop {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let Some(event) = next_raw_event(&mut observer, args.idle_timeout).await? else {
            break;
        };

        if let Some(frame) = tracker.push(&event) {
            count += 1;
            print_frame(&frame, args.json)?;
        }
    }

    Ok(())
}

async fn record(args: RecordArgs) -> Result<()> {
    let mut observer = if args.exclusive {
        EvdevObserver::open_exclusive(&args.device)?
    } else {
        EvdevObserver::open(&args.device)?
    };
    let mut output = tokio::fs::File::create(&args.output)
        .await
        .with_context(|| format!("failed to create recording {}", args.output.display()))?;
    let mut count = 0usize;

    let delivery_mode = if observer.is_exclusive() {
        "exclusive mode; desktop gestures are temporarily blocked"
    } else {
        "shared mode; desktop gestures may still run"
    };

    eprintln!(
        "recording raw events from {} ({}) to {} in {}",
        observer.info().path.display(),
        observer.info().name,
        args.output.display(),
        delivery_mode
    );

    loop {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let Some(event) = next_raw_event(&mut observer, args.idle_timeout).await? else {
            break;
        };

        output.write_all(&serde_json::to_vec(&event)?).await?;
        output.write_all(b"\n").await?;
        count += 1;
    }

    output.flush().await?;
    eprintln!("recorded {count} raw events to {}", args.output.display());
    Ok(())
}

async fn gestures(args: GesturesArgs) -> Result<()> {
    let mut observer = if args.exclusive {
        EvdevObserver::open_exclusive(&args.device)?
    } else {
        EvdevObserver::open(&args.device)?
    };
    let config = load_recognizer_config(args.recognizer_config.as_deref())?;
    let mut tracker = TouchFrameTracker::new();
    let mut recognizer = GestureRecognizer::new(config)?;
    let mut count = 0usize;

    let delivery_mode = if observer.is_exclusive() {
        "exclusive mode; desktop gestures are temporarily blocked"
    } else {
        "shared mode; desktop gestures may still run"
    };
    eprintln!(
        "recognizing gestures from {} ({}) in {}",
        observer.info().path.display(),
        observer.info().name,
        delivery_mode
    );

    loop {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let Some(raw_event) = next_raw_event(&mut observer, args.idle_timeout).await? else {
            break;
        };

        let Some(frame) = tracker.push(&raw_event) else {
            continue;
        };

        for event in recognizer.push(&frame) {
            print_gesture(&event, args.json)?;
            count += 1;
            if args.limit > 0 && count >= args.limit {
                break;
            }
        }
    }

    Ok(())
}

fn replay(args: ReplayArgs) -> Result<()> {
    let input = StdFile::open(&args.input)
        .with_context(|| format!("failed to open recording {}", args.input.display()))?;
    let reader = StdBufReader::new(input);
    let mut tracker = TouchFrameTracker::new();
    let mut count = 0usize;

    for (index, line) in reader.lines().enumerate() {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let line = line.with_context(|| {
            format!(
                "failed to read line {} from {}",
                index + 1,
                args.input.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let event: RawInputEvent = serde_json::from_str(&line).with_context(|| {
            format!(
                "invalid raw event at {}:{}",
                args.input.display(),
                index + 1
            )
        })?;

        if let Some(frame) = tracker.push(&event) {
            count += 1;
            print_frame(&frame, args.json)?;
        }
    }

    Ok(())
}

fn recognize(args: RecognizeArgs) -> Result<()> {
    let input = StdFile::open(&args.input)
        .with_context(|| format!("failed to open recording {}", args.input.display()))?;
    let reader = StdBufReader::new(input);
    let config = load_recognizer_config(args.recognizer_config.as_deref())?;
    let mut tracker = TouchFrameTracker::new();
    let mut recognizer = GestureRecognizer::new(config)?;
    let mut count = 0usize;

    for (index, line) in reader.lines().enumerate() {
        if args.limit > 0 && count >= args.limit {
            break;
        }

        let line = line.with_context(|| {
            format!(
                "failed to read line {} from {}",
                index + 1,
                args.input.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let raw_event: RawInputEvent = serde_json::from_str(&line).with_context(|| {
            format!(
                "invalid raw event at {}:{}",
                args.input.display(),
                index + 1
            )
        })?;

        let Some(frame) = tracker.push(&raw_event) else {
            continue;
        };

        for event in recognizer.push(&frame) {
            print_gesture(&event, args.json)?;
            count += 1;
            if args.limit > 0 && count >= args.limit {
                break;
            }
        }
    }

    Ok(())
}

fn load_recognizer_config(path: Option<&Path>) -> Result<RecognizerConfig> {
    match path {
        Some(path) => RecognizerConfig::load(path),
        None => {
            let config = RecognizerConfig::default();
            config.validate()?;
            Ok(config)
        }
    }
}

fn print_gesture(event: &InputEvent, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(event)?);
        return Ok(());
    }

    let fingers = event
        .fingers
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned());
    let direction = event.direction.as_deref().unwrap_or("-");
    let distance = event.values.get("distance").copied().unwrap_or_default();
    let velocity = event
        .values
        .get("average_velocity")
        .copied()
        .unwrap_or_default();
    let duration = event.values.get("duration_ms").copied().unwrap_or_default();

    println!(
        "{} phase={} fingers={} direction={} distance={:.1} velocity={:.1}/s duration={:.1}ms",
        event.family, event.phase, fingers, direction, distance, velocity, duration
    );
    Ok(())
}

async fn next_raw_event(
    observer: &mut EvdevObserver,
    idle_timeout: u64,
) -> Result<Option<RawInputEvent>> {
    if idle_timeout == 0 {
        tokio::select! {
            result = observer.next_event() => result.map(Some),
            _ = tokio::signal::ctrl_c() => Ok(None),
        }
    } else {
        tokio::select! {
            result = tokio::time::timeout(
                Duration::from_secs(idle_timeout),
                observer.next_event(),
            ) => match result {
                Ok(event) => event.map(Some),
                Err(_) => {
                    eprintln!("idle timeout reached; no input event was received");
                    Ok(None)
                }
            },
            _ = tokio::signal::ctrl_c() => Ok(None),
        }
    }
}

fn print_frame(frame: &TouchFrame, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(frame)?);
        return Ok(());
    }

    let centroid = frame
        .centroid
        .map(|point| format!("({:.1},{:.1})", point.x, point.y))
        .unwrap_or_else(|| "-".to_owned());
    let delta = frame
        .delta
        .map(|point| format!("({:+.1},{:+.1})", point.x, point.y))
        .unwrap_or_else(|| "-".to_owned());
    let velocity = frame
        .velocity_per_second
        .map(|point| format!("({:+.1},{:+.1})", point.x, point.y))
        .unwrap_or_else(|| "-".to_owned());

    println!(
        "{:>5} {:?} fingers={} tracked={} reported={:?} centroid={} delta={} velocity/s={}",
        frame.sequence,
        frame.phase,
        frame.fingers,
        frame.tracked_contacts,
        frame.reported_fingers,
        centroid,
        delta,
        velocity
    );
    Ok(())
}

fn parse_metric(value: &str) -> std::result::Result<(String, f64), String> {
    let (key, value) = split_pair(value)?;
    let number = value
        .parse::<f64>()
        .map_err(|error| format!("invalid numeric value {value:?}: {error}"))?;
    Ok((key.to_owned(), number))
}

fn parse_label(value: &str) -> std::result::Result<(String, String), String> {
    let (key, value) = split_pair(value)?;
    Ok((key.to_owned(), value.to_owned()))
}

fn split_pair(value: &str) -> std::result::Result<(&str, &str), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| "expected key=value".to_owned())?;
    if key.trim().is_empty() {
        Err("key must not be empty".to_owned())
    } else {
        Ok((key, value))
    }
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
