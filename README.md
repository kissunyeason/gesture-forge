# GestureForge

GestureForge is an open-source, compositor-independent input automation platform for Linux.
Its central design rule is simple:

> **Input recognition, matching conditions, and output actions are separate modules.**

A gesture never has a hard-coded action. Backends publish normalized events, bindings match those events, condition providers decide whether a binding applies, and action providers execute user-selected behavior.

## Project status

`0.2.0` is the read-only observer release. It includes everything from the
foundation release plus:

- Linux evdev device discovery;
- touchpad/touchscreen/pointer candidate classification;
- a non-grabbing raw event monitor;
- JSON Lines output for future recording and replay tools.

It **does not grab the real touchpad**, create a virtual input device, or inject
input. That is intentional: this version can run alongside the current setup
while we identify the exact hardware event stream. Hardware proxying remains
the next milestone.

## Architecture

```text
physical input -> backend -> normalized event -> matcher -> conditions
                                                    |
                                                    v
                                            action providers
```

- `gesture-core`: event schema, configuration, validation, matching, provider APIs.
- `gesture-actions`: independently registered action providers.
- `gesture-device`: interfaces for evdev/libinput/uinput and test backends.
- `gesture-daemon`: configuration reload, event socket, dispatch.
- `gesture-cli`: validate configs and inject simulated events.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/CONFIGURATION.md](docs/CONFIGURATION.md), [docs/PLUGIN_API.md](docs/PLUGIN_API.md), [docs/PRIOR_ART.md](docs/PRIOR_ART.md), and [docs/ROADMAP.md](docs/ROADMAP.md).

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
```

The monitor never grabs the device. See [the observer documentation](docs/DEVICE_OBSERVER.md).
