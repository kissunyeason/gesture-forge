# Gesture recognition

GestureForge provides an action-agnostic recognizer between normalized touch
frames and the core event matcher. Version 0.4.1 allows multiple swipe and hold
rules with arbitrary finger counts to run simultaneously. Version 0.5.0 adds an
opt-in continuous hold-then-drag lifecycle.

```text
evdev raw events -> touch frames -> gesture recognizer -> InputEvent
```

The recognizer does not know about GNOME workspaces, mouse buttons, commands,
or any other action. It emits the same stable `InputEvent` schema used by the
daemon and simulation CLI.

## Events

A completed swipe emits:

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

Events retain their effective finger count and include the stable matching rule
identity in `labels["recognition.rule_id"]`. Recognition remains independent
from bindings and actions.

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

Finger-count and coordinate-contact transitions are excluded from metrics. If a
session contains more than one stable segment with the configured finger count,
the longest segment is classified. A rule with
`require_complete_tracking = true` rejects frames where the device reports more
fingers than it supplies complete coordinates for.

All enabled swipe rules are evaluated first to preserve the v0.4 swipe-before-
hold behavior. If multiple rules match, the longest stable segment wins,
followed by sample count and declaration order. A completed touch session emits
at most one event.

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
  --exclusive-timeout 120 \
  --recognizer-config ~/.config/gesture-forge/recognizer.toml
```

`gestures` is shared by default. `--exclusive` temporarily blocks other clients
from receiving touchpad events and is intended for controlled testing. While it
is active, ordinary one- and two-finger desktop input is also blocked because
GestureForge does not yet proxy unmatched hardware events. A guarded exclusive
session ends on `SIGINT`, `SIGTERM`, terminal hangup, launching-process exit, or
the total `--exclusive-timeout` (120 seconds by default).

Add `--dispatch` (and optionally `--socket PATH`) to forward each recognized
event to a running daemon. The daemon remains responsible for configuration,
security checks, and action execution. The CLI treats unsuccessful action
outcomes as errors instead of accepting any syntactically valid daemon reply.
For pointer-drag testing, combine `--dispatch` with `--exclusive` so the desktop
does not process the physical gesture at the same time.

The physical grab is released before socket-based drag cancellation is sent.
Daemon acknowledgements also have a short timeout, so an unresponsive daemon
cannot keep the physical touchpad grabbed indefinitely.

The generic rule syntax is:

```toml
[[recognition.swipes]]
id = "three-finger-swipe"
enabled = true
fingers = 3
min_distance = 200.0
min_average_velocity = 400.0
max_duration_ms = 900.0
max_axis_deviation_degrees = 30.0
require_complete_tracking = true

[[recognition.holds]]
id = "three-finger-hold"
enabled = true
fingers = 3
min_duration_ms = 650.0
max_net_distance = 30.0
require_complete_tracking = false
```

Rule IDs must be unique across swipe and hold rules. New rules require explicit
thresholds and a tracking policy, so adding a four- or five-finger rule cannot
silently inherit the calibrated three-finger values.

The v0.4 tables remain accepted and are migrated in memory:

```toml
[recognition.three_finger_swipe]
min_distance = 250.0

[recognition.three_finger_hold]
enabled = false
```

A legacy table cannot be combined with the new list for the same gesture type.
Legacy rules retain the v0.4 partial-coordinate behavior. The example and fresh
built-in swipe rule require complete tracking. Only three-finger thresholds
have real-sample calibration.

## Continuous drag rules

A drag rule first observes a stable hold. Reaching the hold duration arms the
rule without consuming the session. Movement beyond `min_drag_distance` then
activates the drag and emits:

```text
touchpad.drag phase=begin
touchpad.drag phase=update
touchpad.drag phase=update
touchpad.drag phase=end
```

If finger count, coordinate membership, or required complete tracking changes
after activation, the lifecycle ends with `phase=cancel`. Once activated, a
drag owns that touch session and suppresses swipe/hold classification. If it is
only armed and the user releases without moving, the ordinary hold rule remains
eligible.

```toml
[[recognition.drags]]
id = "three-finger-drag"
enabled = false
fingers = 3
min_hold_duration_ms = 350.0
max_hold_distance = 20.0
min_drag_distance = 8.0
require_complete_tracking = true
```

`begin` and `update` events expose `dx`, `dy`, `total_dx`, `total_dy`,
`distance`, `path_length`, `duration_ms`, and `hold_duration_ms`. Drag rules are
disabled by default and their example thresholds are not calibrated. The
recognizer does not inject pointer movement or mouse buttons; consumers must
also treat both `end` and `cancel` as mandatory release signals.

The live and offline CLI commands explicitly cancel an active drag before
returning because of Ctrl+C, idle timeout, event limit, input EOF, or another
processing error. Every drag lifecycle carries a stable
`recognition.stream_id`; action providers use it to reject stale events. The
daemon additionally synthesizes a matching cancel if a dispatch client exits or
loses its socket before completing the lifecycle.
