# Evdev observer, exclusive recorder, and touchpad proxy

GestureForge provides shared device discovery and raw event observation.
GestureForge 0.3.1 also adds an explicit exclusive mode for sample recording.

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

## Experimental selective passthrough

Live recognition can exclusively grab the physical touchpad while preserving
normal one- and two-finger desktop input through a uinput clone:

```bash
cargo run -p gesture-cli -- gestures \
  --device /dev/input/event8 \
  --exclusive \
  --passthrough \
  --exclusive-timeout 120
```

The proxy buffers each physical frame until `SYN_REPORT`. Frames with at most
two fingers are replayed to `GestureForge Virtual Touchpad`. When a third finger
appears, any virtual contacts already exposed to the desktop are explicitly
ended and the remainder of that physical session is consumed until all fingers
are lifted. This prevents the compositor from receiving a three-finger stream
while keeping pointer movement and two-finger scrolling available.

The virtual device copies the selected device's key set, absolute-axis metadata,
input ID, and input properties. `/dev/uinput` permission is therefore required.
The implementation is intentionally experimental and currently targets
protocol-B clickpads with `ABS_X/Y`, MT slots, MT positions, tracking IDs,
`INPUT_PROP_POINTER`, and `INPUT_PROP_BUTTONPAD`.

## Architectural role

Raw evdev events are diagnostic input, not configurable actions and not yet
normalized gestures. Future recognizers will convert frames into namespaced
`InputEvent` values. Action providers remain unaware of evdev details.

## Build normalized frames

```bash
cargo run -p gesture-cli -- frames \
  --device /dev/input/event8 \
  --json
```

For deterministic diagnostics, use `record` to create raw JSON Lines and
`replay` to run the same data through the frame tracker. When desktop gestures
would interfere with sampling, use:

```bash
cargo run -p gesture-cli -- record \
  --device /dev/input/event8 \
  --output sample.jsonl \
  --exclusive \
  --exclusive-timeout 120
```

The `--exclusive` option is deliberately opt-in. It calls `EVIOCGRAB` only for
the guarded lifetime of the recorder process. GestureForge explicitly ungrabs
before flushing or dispatch cleanup, listens for `SIGINT`, `SIGTERM`, and
`SIGHUP`, watches the launching terminal process, and enforces a total exclusive
timeout. The default is 120 seconds and accepted values are 1 through 3600.

If a development process must be stopped from another terminal, use:

```bash
pkill -TERM -x gesture-forge
```

See [TOUCH_FRAMES.md](TOUCH_FRAMES.md).
