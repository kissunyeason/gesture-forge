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
- future `uinput.key-sequence`
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
