# Normalized multitouch frames

GestureForge converts Linux multitouch protocol-B events into contact frames
before any gesture is recognized.

The tracker consumes:

- `ABS_MT_SLOT` to select a contact slot;
- `ABS_MT_TRACKING_ID` to begin and end a contact;
- `ABS_MT_POSITION_X` and `ABS_MT_POSITION_Y` for contact positions;
- `BTN_TOOL_*TAP` as a fallback finger count;
- `SYN_REPORT` as the frame boundary.

Each frame contains:

- begin, update, or end phase;
- effective and hardware-reported finger counts;
- sorted active contacts and tracking IDs;
- centroid of contacts with complete coordinates;
- centroid displacement and velocity when the contact count is stable;
- source timestamp and frame interval.

Finger-count transitions intentionally reset displacement and velocity so that
adding or removing a finger does not create a false motion spike.

## Live frames

```bash
cargo run -p gesture-cli -- frames \
  --device /dev/input/event8 \
  --json
```

This is read-only and does not call `EVIOCGRAB`.

## Recording and replay

```bash
cargo run -p gesture-cli -- record \
  --device /dev/input/event8 \
  --output touchpad.jsonl \
  --exclusive \
  --idle-timeout 15

cargo run -p gesture-cli -- replay \
  --input touchpad.jsonl \
  --json
```

A recording contains one serialized `RawInputEvent` per line. Replay uses the
same tracker as live input, allowing deterministic tests without hardware.

`--exclusive` (also accepted as `--grab`) requests Linux `EVIOCGRAB` for the
duration of the recording. Other clients, including the desktop compositor, do
not receive those events, so workspace and overview gestures are not triggered.
The grab is tied to the open device file and is released when the recorder exits.
Without this option, recording remains shared and desktop gestures may run.

Touch frames are not gestures and do not contain actions. The independent
`gesture-recognition` crate now consumes them for v0.4 swipe and hold events.
Drag, pinch, rotation, and tap remain future recognizers.
