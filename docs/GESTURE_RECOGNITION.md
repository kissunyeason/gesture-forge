# Gesture recognition

GestureForge 0.4 adds an action-agnostic recognizer between normalized touch
frames and the core event matcher.

```text
evdev raw events -> touch frames -> gesture recognizer -> InputEvent
```

The recognizer does not know about GNOME workspaces, mouse buttons, commands,
or any other action. It emits the same stable `InputEvent` schema used by the
daemon and simulation CLI.

## v0.4 events

A completed three-finger swipe emits:

```json
{
  "family": "touchpad.swipe",
  "phase": "end",
  "fingers": 3,
  "direction": "up",
  "values": {
    "distance": 321.0,
    "path_length": 322.0,
    "duration_ms": 298.0,
    "average_velocity": 1080.0,
    "axis_deviation_degrees": 0.7,
    "dx": 4.0,
    "dy": -321.0,
    "straightness": 0.997,
    "sample_points": 36.0
  }
}
```

A stationary three-finger hold emits `touchpad.hold` with phase `end`.
Classification happens when the entire touch session ends, so a partial motion
does not trigger an irreversible action early.

## Recognition rules

The swipe classifier combines all of these conditions:

- minimum net distance;
- minimum average path velocity;
- maximum stable-finger duration;
- maximum deviation from the nearest cardinal axis.

Distance alone cannot distinguish a slow move from a swipe. Velocity alone
cannot distinguish a short, fast adjustment from a swipe. Combining the
metrics separated all 21 swipe samples from 15 negative samples in the v0.4
development dataset. That is in-sample validation, not a universal accuracy
claim.

Finger-count transitions are excluded from metrics. If a session contains more
than one stable segment with the configured finger count, the longest segment
is classified.

## Configuration

Copy the example file if hardware-specific tuning is needed:

```bash
mkdir -p ~/.config/gesture-forge
cp configs/recognizer.example.toml \
  ~/.config/gesture-forge/recognizer.toml
```

Use it during offline recognition:

```bash
cargo run -p gesture-cli -- recognize \
  --input sample.jsonl \
  --recognizer-config ~/.config/gesture-forge/recognizer.toml \
  --json
```

Or inspect live recognition:

```bash
cargo run -p gesture-cli -- gestures \
  --device /dev/input/event8 \
  --exclusive \
  --recognizer-config ~/.config/gesture-forge/recognizer.toml
```

`gestures` is shared by default. `--exclusive` temporarily blocks other clients
from receiving touchpad events and is intended for controlled testing until a
fail-open uinput proxy exists.
