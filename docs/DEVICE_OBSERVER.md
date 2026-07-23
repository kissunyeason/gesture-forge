# Read-only evdev observer

GestureForge 0.2 adds device discovery and raw event observation without
claiming the device.

## List readable devices

```bash
cargo run -p gesture-cli -- devices
cargo run -p gesture-cli -- devices --touchpads-only
cargo run -p gesture-cli -- devices --json
```

Only nodes that the current process can open are shown. On Fedora this usually
requires membership of the `input` group or access delegated by the session.

## Observe one device

```bash
cargo run -p gesture-cli -- monitor \
  --device /dev/input/event8 \
  --idle-timeout 10
```

The observer does not call `EVIOCGRAB` and does not emit any synthetic input.
Press `Ctrl+C` to stop. `--limit 100` records a bounded sample and `--json`
produces JSON Lines suitable for later recorder/replayer work.

An existing program that has exclusively grabbed the physical touchpad can
prevent the observer from receiving events. Stop that program temporarily or
observe its virtual touchpad instead. Discovery itself remains safe.

## Architectural role

Raw evdev events are diagnostic input, not configurable actions and not yet
normalized gestures. Future recognizers will convert frames into namespaced
`InputEvent` values. Action providers remain unaware of evdev details.
