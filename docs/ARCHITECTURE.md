# Architecture

## Stable boundaries

GestureForge treats input automation as a pipeline with stable contracts:

1. **Backend** reads a device or compositor protocol.
2. **Recognizer** publishes normalized events.
3. **Matcher** compares events with declarative trigger patterns.
4. **Condition providers** evaluate runtime context.
5. **Action providers** execute independent effects.

No layer is allowed to assume a specific action for a gesture.

## Normalized event

An event has a string `family`, lifecycle `phase`, optional finger count and direction, numeric values, string labels, and contextual fields. String namespaces allow future additions without breaking the core enum ABI.

Examples:

- `touchpad.swipe`
- `touchpad.drag`
- `touchpad.pinch`
- `mouse.stroke`
- `keyboard.chord`
- `touchscreen.edge-swipe`

## Provider model

Actions are addressed as `<provider>.<action>`, for example:

- `core.log`
- `process.run`
- `uinput.drag`
- `uinput.key-chord`
- future `gnome.show-overview`
- future `dbus.call`

Conditions use the same model, for example `core.app-id` or future `gnome.workspace`.

## Hardware backend roadmap

The hardware proxy will:

- discover real touchpads through evdev/libinput;
- create a synthetic clone through uinput;
- exclusively grab only devices selected by policy;
- forward unmatched traffic losslessly;
- publish recognizer events without embedding actions;
- fail open, releasing the physical device when the daemon exits.

Record/replay fixtures will make recognizer changes testable without hardware.

## Recognizer boundary

`gesture-recognition` consumes only `TouchFrame` values and publishes only
`InputEvent` values. It does not import action providers or desktop adapters.
Swipe and hold rules are generic over finger count and may be enabled
simultaneously. Each completed event carries the matching rule ID as a label.
Recognition uses effective, tracked, reported, and completely tracked finger
state without treating a device-reported count as additional coordinates.

Continuous drag rules publish `touchpad.drag` begin, update, end, and cancel
events after a stable hold followed by intentional movement. They still do not
press buttons, move pointers, or manipulate windows. Those effects belong to
independent action providers. The optional `uinput.drag` action converts the
lifecycle into virtual button and relative-pointer events, while
`uinput.key-chord` emits bounded keyboard chords. Both require explicit
security opt-in and fail-safe release handling. A stable stream ID prevents
late events from an older client from mutating a newer active drag. The daemon
tracks drag ownership per socket and synthesizes cancellation on disconnect.
Security-sensitive provider registries are rebuilt as one runtime state during
configuration reload. Permission reductions are applied fail-closed even when a
stale binding still names the provider that was just disabled. Failed reloads
never grant new action permissions.
