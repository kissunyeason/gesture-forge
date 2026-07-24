use std::{
    collections::BTreeMap,
    env, fmt,
    fs::File as StdFile,
    io::{BufRead, BufReader as StdBufReader, IsTerminal},
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use gesture_actions::{default_action_registry_with_security, default_condition_registry};
use gesture_core::{Config, DispatchReport, Engine, InputEvent};
use gesture_device::{
    enumerate_devices, DeviceInfo, EvdevObserver, RawInputEvent, TouchFrame, TouchFrameTracker,
    TouchpadPassthrough,
};
use gesture_recognition::{GestureRecognizer, RecognizerConfig};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    signal::unix::{signal, Signal, SignalKind},
    time::{Instant, Interval, MissedTickBehavior},
};

const DEFAULT_EXCLUSIVE_TIMEOUT_SECONDS: u64 = 120;
const PARENT_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Maximum total seconds to hold an exclusive grab.
    #[arg(long, default_value_t = DEFAULT_EXCLUSIVE_TIMEOUT_SECONDS)]
    exclusive_timeout: u64,
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
    /// Maximum total seconds to hold an exclusive grab.
    #[arg(long, default_value_t = DEFAULT_EXCLUSIVE_TIMEOUT_SECONDS)]
    exclusive_timeout: u64,
    /// Replay one- and two-finger input through a virtual touchpad while consuming three-or-more-finger sessions.
    #[arg(long, requires = "exclusive")]
    passthrough: bool,
    /// Optional recognizer TOML. Built-in compatible defaults are used when omitted.
    #[arg(long, env = "GESTURE_FORGE_RECOGNIZER_CONFIG")]
    recognizer_config: Option<PathBuf>,
    /// Emit one JSON object per recognized gesture.
    #[arg(long)]
    json: bool,
    /// Forward recognized events to the GestureForge daemon.
    #[arg(long)]
    dispatch: bool,
    #[arg(long, env = "GESTURE_FORGE_SOCKET", default_value_os_t = default_socket_path())]
    socket: PathBuf,
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
    /// Optional recognizer TOML. Built-in compatible defaults are used when omitted.
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
    let actions = default_action_registry_with_security(
        config.security.allow_command_actions,
        config.security.allow_uinput_actions,
    )?;
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
    let mut shutdown = ShutdownMonitor::new(false, 0)?;
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

        let Some(event) = next_raw_event(&mut observer, args.idle_timeout, &mut shutdown).await?
        else {
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
    let mut shutdown = ShutdownMonitor::new(false, 0)?;
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

        let Some(event) = next_raw_event(&mut observer, args.idle_timeout, &mut shutdown).await?
        else {
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
    validate_exclusive_timeout(args.exclusive, args.exclusive_timeout)?;
    let mut output = tokio::fs::File::create(&args.output)
        .await
        .with_context(|| format!("failed to create recording {}", args.output.display()))?;
    let mut shutdown = ShutdownMonitor::new(args.exclusive, args.exclusive_timeout)?;
    let mut observer = if args.exclusive {
        EvdevObserver::open_exclusive(&args.device)?
    } else {
        EvdevObserver::open(&args.device)?
    };
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
    print_exclusive_safety(&observer, args.exclusive_timeout);

    let run_result: Result<()> = async {
        loop {
            if args.limit > 0 && count >= args.limit {
                break;
            }

            let Some(event) =
                next_raw_event(&mut observer, args.idle_timeout, &mut shutdown).await?
            else {
                break;
            };

            output.write_all(&serde_json::to_vec(&event)?).await?;
            output.write_all(b"\n").await?;
            count += 1;
        }
        Ok(())
    }
    .await;

    let release_result = observer.release_grab();
    let finalize_result: Result<()> = async {
        output.flush().await?;
        eprintln!("recorded {count} raw events to {}", args.output.display());
        Ok(())
    }
    .await;

    let result = finish_with_secondary(
        run_result,
        release_result,
        "raw event recording",
        "exclusive grab release",
    );
    finish_with_secondary(
        result,
        finalize_result,
        "raw event recording",
        "output finalization",
    )
}

async fn gestures(args: GesturesArgs) -> Result<()> {
    validate_exclusive_timeout(args.exclusive, args.exclusive_timeout)?;
    let config = load_recognizer_config(args.recognizer_config.as_deref())?;
    let mut tracker = TouchFrameTracker::new();
    let mut recognizer = GestureRecognizer::new(config)?;
    let mut dispatcher = if args.dispatch {
        Some(DispatchClient::connect(&args.socket).await?)
    } else {
        None
    };
    let mut shutdown = ShutdownMonitor::new(args.exclusive, args.exclusive_timeout)?;
    let mut passthrough = if args.passthrough {
        Some(TouchpadPassthrough::open(&args.device)?)
    } else {
        None
    };
    let mut observer = if args.exclusive {
        EvdevObserver::open_exclusive(&args.device)?
    } else {
        EvdevObserver::open(&args.device)?
    };
    let mut count = 0usize;

    let delivery_mode = if passthrough.is_some() {
        "exclusive proxy mode; one/two fingers are forwarded and three-or-more are consumed"
    } else if observer.is_exclusive() {
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
    print_exclusive_safety(&observer, args.exclusive_timeout);

    let run_result: Result<()> = async {
        loop {
            if args.limit > 0 && count >= args.limit {
                break;
            }

            let Some(raw_event) =
                next_raw_event(&mut observer, args.idle_timeout, &mut shutdown).await?
            else {
                break;
            };

            if let Some(passthrough) = passthrough.as_mut() {
                passthrough.push(&raw_event)?;
            }

            let Some(frame) = tracker.push(&raw_event) else {
                continue;
            };

            for event in recognizer.push(&frame) {
                deliver_gesture(&event, args.json, dispatcher.as_mut()).await?;
                count += 1;
                if args.limit > 0 && count >= args.limit {
                    break;
                }
            }
        }
        Ok(())
    }
    .await;

    // End any virtual contacts before restoring the physical touchpad. Both
    // operations are local and must complete before socket-based drag cleanup.
    let passthrough_release_result = passthrough
        .as_mut()
        .map(TouchpadPassthrough::release_all)
        .unwrap_or(Ok(()));
    let grab_release_result = observer.release_grab();
    let release_result = finish_with_secondary(
        passthrough_release_result,
        grab_release_result,
        "live gesture recognition",
        "physical grab release",
    );

    let cleanup_result: Result<()> = async {
        for event in recognizer.cancel() {
            deliver_gesture(&event, args.json, dispatcher.as_mut()).await?;
        }
        Ok(())
    }
    .await;

    let result = finish_with_secondary(
        run_result,
        release_result,
        "live gesture recognition",
        "exclusive grab release",
    );
    finish_with_secondary(
        result,
        cleanup_result,
        "live gesture recognition",
        "drag cleanup",
    )
}

async fn deliver_gesture(
    event: &InputEvent,
    json: bool,
    dispatcher: Option<&mut DispatchClient>,
) -> Result<()> {
    if let Some(dispatcher) = dispatcher {
        dispatcher.send(event).await?;
    }
    print_gesture(event, json)
}

struct DispatchClient {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}

impl DispatchClient {
    async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("failed to connect to daemon socket {}", path.display()))?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            writer,
            reader: BufReader::new(reader),
        })
    }

    async fn send(&mut self, event: &InputEvent) -> Result<()> {
        tokio::time::timeout(DISPATCH_TIMEOUT, self.send_inner(event))
            .await
            .with_context(|| {
                format!(
                    "daemon did not acknowledge {} {} within {} seconds",
                    event.family,
                    event.phase,
                    DISPATCH_TIMEOUT.as_secs()
                )
            })?
    }

    async fn send_inner(&mut self, event: &InputEvent) -> Result<()> {
        self.writer.write_all(&serde_json::to_vec(event)?).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        let mut response = String::new();
        self.reader.read_line(&mut response).await?;
        if response.trim().is_empty() {
            anyhow::bail!("daemon closed the dispatch socket without a response");
        }

        let value: serde_json::Value =
            serde_json::from_str(&response).context("daemon returned invalid response JSON")?;
        if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
            anyhow::bail!(
                "daemon rejected {} {}: {}",
                event.family,
                event.phase,
                error
            );
        }

        let report: DispatchReport =
            serde_json::from_value(value).context("daemon response was not a dispatch report")?;
        validate_dispatch_report(event, &report)
    }
}

fn validate_dispatch_report(event: &InputEvent, report: &DispatchReport) -> Result<()> {
    if report.event_id != event.id {
        anyhow::bail!(
            "daemon response event id {} does not match dispatched event {}",
            report.event_id,
            event.id
        );
    }
    if let Some(failure) = dispatch_failure(report) {
        anyhow::bail!(
            "daemon action failed for {} {}: {}",
            event.family,
            event.phase,
            failure
        );
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

fn finish_with_secondary(
    primary_result: Result<()>,
    secondary_result: Result<()>,
    operation: &str,
    secondary_name: &str,
) -> Result<()> {
    match (primary_result, secondary_result) {
        (Err(error), Err(secondary_error)) => Err(error).context(format!(
            "{operation} failed; {secondary_name} also failed: {secondary_error}"
        )),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context(format!("{operation} {secondary_name} failed")),
        (Ok(()), Ok(())) => Ok(()),
    }
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

    let run_result = (|| -> Result<()> {
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
    })();

    let cleanup_result = (|| -> Result<()> {
        for event in recognizer.cancel() {
            print_gesture(&event, args.json)?;
        }
        Ok(())
    })();

    finish_with_secondary(
        run_result,
        cleanup_result,
        "offline gesture recognition",
        "drag cleanup",
    )
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
    let rule_id = event
        .labels
        .get("recognition.rule_id")
        .map(String::as_str)
        .unwrap_or("-");

    if event.family == "touchpad.drag" {
        let dx = event.values.get("dx").copied().unwrap_or_default();
        let dy = event.values.get("dy").copied().unwrap_or_default();
        let total_dx = event.values.get("total_dx").copied().unwrap_or_default();
        let total_dy = event.values.get("total_dy").copied().unwrap_or_default();
        println!(
            "{} phase={} rule={} fingers={} delta=({:+.1},{:+.1}) total=({:+.1},{:+.1}) duration={:.1}ms",
            event.family,
            event.phase,
            rule_id,
            fingers,
            dx,
            dy,
            total_dx,
            total_dy,
            duration
        );
        return Ok(());
    }

    println!(
        "{} phase={} rule={} fingers={} direction={} distance={:.1} velocity={:.1}/s duration={:.1}ms",
        event.family, event.phase, rule_id, fingers, direction, distance, velocity, duration
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownReason {
    Interrupt,
    Terminate,
    Hangup,
    ParentExited,
    ParentUnavailable,
    ExclusiveTimeout,
}

impl fmt::Display for ShutdownReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Interrupt => "SIGINT received",
            Self::Terminate => "SIGTERM received",
            Self::Hangup => "terminal hangup received",
            Self::ParentExited => "launching process exited",
            Self::ParentUnavailable => "launching process could no longer be inspected",
            Self::ExclusiveTimeout => "exclusive-grab safety timeout reached",
        })
    }
}

struct ShutdownMonitor {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
    parent_pid: Option<u32>,
    parent_check: Interval,
    exclusive_deadline: Option<Instant>,
}

impl ShutdownMonitor {
    fn new(exclusive: bool, exclusive_timeout: u64) -> Result<Self> {
        let interrupt = signal(SignalKind::interrupt()).context("failed to watch SIGINT")?;
        let terminate = signal(SignalKind::terminate()).context("failed to watch SIGTERM")?;
        let hangup = signal(SignalKind::hangup()).context("failed to watch SIGHUP")?;
        let parent_pid = if exclusive && stdio_is_terminal() {
            Some(read_parent_pid()?)
        } else {
            None
        };
        let mut parent_check = tokio::time::interval(PARENT_CHECK_INTERVAL);
        parent_check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let exclusive_deadline =
            exclusive.then(|| Instant::now() + Duration::from_secs(exclusive_timeout));

        Ok(Self {
            interrupt,
            terminate,
            hangup,
            parent_pid,
            parent_check,
            exclusive_deadline,
        })
    }

    async fn wait(&mut self) -> ShutdownReason {
        let expected_parent = self.parent_pid;
        let has_deadline = self.exclusive_deadline.is_some();
        let deadline = self
            .exclusive_deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(24 * 60 * 60));

        loop {
            tokio::select! {
                biased;
                _ = self.interrupt.recv() => return ShutdownReason::Interrupt,
                _ = self.terminate.recv() => return ShutdownReason::Terminate,
                _ = self.hangup.recv() => return ShutdownReason::Hangup,
                _ = tokio::time::sleep_until(deadline), if has_deadline => {
                    return ShutdownReason::ExclusiveTimeout;
                }
                _ = self.parent_check.tick(), if expected_parent.is_some() => {
                    match read_parent_pid() {
                        Ok(current_parent) if Some(current_parent) != expected_parent => {
                            return ShutdownReason::ParentExited;
                        }
                        Err(_) => return ShutdownReason::ParentUnavailable,
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

fn validate_exclusive_timeout(exclusive: bool, timeout: u64) -> Result<()> {
    if exclusive && !(1..=3600).contains(&timeout) {
        anyhow::bail!("--exclusive-timeout must be between 1 and 3600 seconds");
    }
    Ok(())
}

fn print_exclusive_safety(observer: &EvdevObserver, timeout: u64) {
    if !observer.is_exclusive() {
        return;
    }

    eprintln!(
        "exclusive safety: pid={} timeout={}s; SIGINT, SIGTERM, SIGHUP, parent exit, timeout, and Drop all release the grab",
        process::id(),
        timeout
    );
}

fn stdio_is_terminal() -> bool {
    std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::io::stderr().is_terminal()
}

fn read_parent_pid() -> Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status")
        .context("failed to inspect /proc/self/status for exclusive-grab watchdog")?;
    parse_parent_pid(&status).context("/proc/self/status did not contain a valid PPid entry")
}

fn parse_parent_pid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .and_then(|value| value.trim().parse().ok())
            .filter(|pid| *pid > 0)
    })
}

async fn next_raw_event(
    observer: &mut EvdevObserver,
    idle_timeout: u64,
    shutdown: &mut ShutdownMonitor,
) -> Result<Option<RawInputEvent>> {
    if idle_timeout == 0 {
        tokio::select! {
            biased;
            reason = shutdown.wait() => {
                eprintln!("stopping input capture: {reason}");
                Ok(None)
            }
            result = observer.next_event() => result.map(Some),
        }
    } else {
        tokio::select! {
            biased;
            reason = shutdown.wait() => {
                eprintln!("stopping input capture: {reason}");
                Ok(None)
            }
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
        "{:>5} {:?} fingers={} tracked={} reported={:?} complete={} centroid={} delta={} velocity/s={}",
        frame.sequence,
        frame.phase,
        frame.fingers,
        frame.tracked_contacts,
        frame.reported_fingers,
        frame.tracking_complete,
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

#[cfg(test)]
mod tests {
    use super::*;
    use gesture_core::{ActionOutcome, BindingReport};

    #[test]
    fn rejects_failed_daemon_actions() {
        let event = InputEvent::new("test", "end");
        let report = DispatchReport {
            event_id: event.id,
            bindings: vec![BindingReport {
                id: "drag".to_owned(),
                outcomes: vec![ActionOutcome {
                    provider: "uinput".to_owned(),
                    action: "drag".to_owned(),
                    success: false,
                    message: Some("permission denied".to_owned()),
                }],
            }],
        };
        let error = validate_dispatch_report(&event, &report).unwrap_err();
        assert!(error.to_string().contains("permission denied"));
    }

    #[test]
    fn rejects_a_response_for_a_different_event() {
        let event = InputEvent::new("test", "end");
        let report = DispatchReport {
            event_id: InputEvent::new("other", "end").id,
            bindings: Vec::new(),
        };
        let error = validate_dispatch_report(&event, &report).unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn parses_parent_pid_from_proc_status() {
        let status = "Name:\tgesture-forge\nState:\tS (sleeping)\nPPid:\t4321\n";
        assert_eq!(parse_parent_pid(status), Some(4321));
        assert_eq!(parse_parent_pid("PPid:\t0\n"), None);
        assert_eq!(parse_parent_pid("Name:\ttest\n"), None);
    }

    #[test]
    fn validates_exclusive_grab_timeout() {
        validate_exclusive_timeout(false, 0).unwrap();
        validate_exclusive_timeout(true, 1).unwrap();
        validate_exclusive_timeout(true, 3600).unwrap();
        assert!(validate_exclusive_timeout(true, 0).is_err());
        assert!(validate_exclusive_timeout(true, 3601).is_err());
    }
}
