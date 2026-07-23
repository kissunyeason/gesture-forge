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
- live and offline recognition commands that emit standard `InputEvent` objects.

Only the built-in three-finger swipe and hold thresholds have been calibrated
against real samples. Four- and five-finger rules and drag thresholds require
explicit testing. Drag recognition emits events only; it does not yet inject a
mouse button or pointer motion.

Shared observation remains non-grabbing. Recording and live recognition can opt
into a temporary `EVIOCGRAB` so GNOME and other clients do not receive test
gestures. GestureForge still does not create a virtual input device or inject
input. Hardware proxying, passthrough, drag, tap, pinch, and rotation remain
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
- `gesture-device`: evdev discovery, raw observation, protocol-B frame tracking, and backend interfaces.
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

Future evdev/uinput access will use narrow udev rules and a dedicated group. GestureForge will not require running its daemon as root.

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
  --exclusive
cargo run -p gesture-cli -- replay --input sample.jsonl --json
cargo run -p gesture-cli -- recognize --input sample.jsonl --json
cargo run -p gesture-cli -- gestures --device /dev/input/event8 --exclusive
```

`monitor` and `frames` never grab the device. `record` is shared by default;
add `--exclusive` (alias `--grab`) when collecting gesture samples so desktop
gestures are temporarily blocked. The grab ends when the recorder exits. See
[the observer documentation](docs/DEVICE_OBSERVER.md).
