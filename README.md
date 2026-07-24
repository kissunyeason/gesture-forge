# GestureForge

GestureForge is an open-source, compositor-independent input automation platform for Linux.
Its central design rule is simple:

> **Input recognition, matching conditions, and output actions are separate modules.**

A gesture never has a hard-coded action. Backends publish normalized events, bindings match those events, condition providers decide whether a binding applies, and action providers execute user-selected behavior.

## Project status

`0.5.0` extends the action-agnostic recognizer with generic finger-count rules
and an experimental continuous drag lifecycle. It includes the read-only
observer, multitouch frames, and:

- Linux evdev device discovery and non-grabbing observation;
- protocol-B slot and tracking-ID parsing;
- normalized contact frames with finger count, centroid, displacement, and velocity;
- live frame inspection plus raw JSON Lines recording and offline replay;
- simultaneous configurable N-finger swipe and hold rules at touch-session end;
- opt-in N-finger hold-then-drag recognition with begin/update/end/cancel events;
- an opt-in uinput action provider for virtual pointer drags and keyboard chords;
- an experimental exclusive virtual-touchpad proxy that forwards one/two fingers and consumes three-or-more;
- live and offline recognition commands that emit standard `InputEvent` objects;
- optional live forwarding from the recognizer to the daemon socket.

Only the built-in three-finger swipe and hold thresholds have been calibrated
against real samples. Four- and five-finger rules and drag thresholds require
explicit testing. Recognition itself remains action-agnostic.

Shared observation remains non-grabbing. Recording and live recognition can opt
into a temporary `EVIOCGRAB` so GNOME and other clients do not receive test
gestures. Exclusive CLI sessions now explicitly ungrab before shutdown cleanup,
watch `SIGINT`, `SIGTERM`, and terminal hangup, stop when their launching
terminal process disappears, and enforce a 120-second total grab limit by
default. The optional uinput provider creates virtual pointer and keyboard
devices only when explicitly enabled and first executed. `gestures --exclusive --passthrough`
clones the selected touchpad into uinput, forwards complete one- and two-finger
frames, and withholds three-or-more-finger sessions from the desktop. This proxy
is experimental; generic hardware support, tap, pinch, and rotation remain
later milestones.

## Architecture

```text
physical input -> backend -> normalized event -> matcher -> conditions
                                                    |
                                                    v
                                            action providers
```

- `gesture-core`: event schema, configuration, validation, matching, provider APIs.
- `gesture-actions`: independently registered action providers.
- `gesture-device`: evdev discovery, raw observation, protocol-B frame tracking, exclusive touchpad proxying, and backend interfaces.
- `gesture-recognition`: configurable frame-to-event recognition without actions.
- `gesture-daemon`: configuration reload, event socket, dispatch.
- `gesture-cli`: validate configs, inspect devices, record/replay input, and inject simulations.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/TOUCH_FRAMES.md](docs/TOUCH_FRAMES.md), [docs/GESTURE_RECOGNITION.md](docs/GESTURE_RECOGNITION.md), [docs/CONFIGURATION.md](docs/CONFIGURATION.md), [docs/PLUGIN_API.md](docs/PLUGIN_API.md), [docs/PRIOR_ART.md](docs/PRIOR_ART.md), and [docs/ROADMAP.md](docs/ROADMAP.md).

## Build

```bash
cargo build --workspace
cargo test --workspace
```

## Safe first run

```bash
mkdir -p ~/.config/gesture-forge
cp configs/config.example.toml ~/.config/gesture-forge/config.toml

cargo run -p gesture-daemon -- \
  --config ~/.config/gesture-forge/config.toml
```

In a second terminal:

```bash
cargo run -p gesture-cli -- simulate \
  --family touchpad.swipe \
  --phase end \
  --fingers 3 \
  --direction up
```

The example binding only writes a log entry. No keyboard, mouse, window, or shell action is bound by default.

## Configuration principles

Each binding contains four independent parts:

1. **trigger** — what event shape to match;
2. **conditions** — when it is allowed to run;
3. **actions** — which providers and action names to invoke;
4. **routing** — priority and whether lower-priority bindings are consumed.

Actions use generic provider/action identifiers and JSON-like TOML parameters. Adding a new action does not require changing the gesture recognizer.

## Security

Command execution is disabled by default. To register `process.run`, set:

```toml
[security]
allow_command_actions = true
```

The process provider executes an explicit program and argument array without invoking a shell.

Virtual input injection is separately disabled by default:

```toml
[security]
allow_uinput_actions = true
```

Enabling the provider does not open `/dev/uinput` during validation. The device
is created lazily on the first matching action. Production installation should
use the narrow rule in `packaging/udev/60-gesture-forge-uinput.rules` and a
dedicated group rather than running the daemon as root. Configuration reloads
rebuild the provider registry. Disabling a security flag immediately drops the
old provider and attempts to release any pressed virtual button or key, even
when a stale binding still references that now-disabled provider; such a binding fails
closed until the configuration is corrected. A failed reload never grants a
new action permission; increases take effect only after full validation.


The same provider exposes `uinput.key-chord`. Its `keys` parameter accepts up to
eight Linux `KEY_*` names below the button-code range. Keys are pressed in the
configured order, released in reverse order, and released again during provider
cleanup if an emission fails.

Each continuous drag carries a stable `recognition.stream_id`. The provider
uses it to reject stale updates and stale cancellation from older clients. The
daemon also synthesizes a cancellation if a live recognizer disconnects before
sending `end` or `cancel`.

## License

GPL-3.0-or-later. Contributions remain open source.


## Device discovery and read-only observation

```bash
cargo run -p gesture-cli -- devices --touchpads-only
cargo run -p gesture-cli -- monitor --device /dev/input/event8 --idle-timeout 10
cargo run -p gesture-cli -- frames --device /dev/input/event8 --json
cargo run -p gesture-cli -- record \
  --device /dev/input/event8 \
  --output sample.jsonl \
  --exclusive \
  --exclusive-timeout 120
cargo run -p gesture-cli -- replay --input sample.jsonl --json
cargo run -p gesture-cli -- recognize --input sample.jsonl --json
cargo run -p gesture-cli -- gestures \
  --device /dev/input/event8 \
  --exclusive \
  --passthrough \
  --exclusive-timeout 120
# With a running daemon, append --dispatch to execute configured actions.
```

`monitor` and `frames` never grab the device. `record` is shared by default;
add `--exclusive` (alias `--grab`) when collecting gesture samples so desktop
gestures are temporarily blocked. The grab ends on normal return, `Ctrl+C`,
`SIGTERM`, terminal hangup, launching-process exit, the exclusive timeout, or
process drop. `--passthrough` requires `--exclusive`: it exposes one- and
two-finger frames through `GestureForge Virtual Touchpad`, terminates that
virtual contact stream when a third finger appears, and suppresses the physical
session until every finger is lifted. If a test terminal is lost,
`pkill -TERM -x gesture-forge` remains the manual recovery command. See
[the observer documentation](docs/DEVICE_OBSERVER.md).
