# Changelog

## 0.5.0 - continuous drag recognition foundation

- add generic N-finger hold-then-drag rules;
- emit action-agnostic `touchpad.drag` begin, update, end, and cancel events;
- keep ordinary holds available until movement actually activates a drag;
- suppress end-of-session swipe and hold output after a drag claims the session;
- cancel active drags when finger count, coordinate membership, or required tracking changes;
- add an explicitly enabled, lazily created uinput drag pointer provider;
- release virtual buttons on end, cancel, emission failure, replacement begin, and provider drop;
- assign stable drag stream IDs so stale clients cannot release a newer drag;
- synthesize drag cancellation when a live client disconnects unexpectedly;
- rebuild security-sensitive action registries on configuration reload and apply permission reductions fail-closed;
- surface daemon action failures and mismatched replies to the live recognizer client;
- emit an explicit drag cancel when live or offline recognition abandons an active stream;
- cancel an unfinished drag before resetting on an unexpected new touch session.

## 0.4.1 - generic finger-count rules

- replace the fixed recognizer configuration with simultaneous swipe and hold rule lists;
- give every rule a stable ID, explicit finger count, and tracking-completeness policy;
- preserve v0.4 `three_finger_swipe` and `three_finger_hold` configuration compatibility;
- distinguish effective, tracked, reported, and completely tracked finger state;
- reset motion when the coordinate-bearing contact set changes;
- expose the matching rule ID without coupling recognition to actions;
- keep built-in calibrated thresholds limited to three-finger gestures.

## 0.4.0 - session gesture recognition

- add an independent `gesture-recognition` crate;
- classify completed three-finger sessions as cardinal swipes or stationary holds;
- combine distance, average velocity, duration, and axis deviation instead of relying on one metric;
- add configurable recognition thresholds with validation;
- add live `gestures` and offline `recognize` CLI commands;
- validate the candidate thresholds against 21 swipe and 15 negative development samples.

## 0.3.1 - exclusive recording hotfix

- add opt-in `record --exclusive` / `record --grab`;
- use Linux `EVIOCGRAB` so desktop and compositor gestures do not run while a sample is captured;
- release the grab automatically when the recorder exits;
- keep shared non-grabbing capture as the default.


## 0.3.0 - multitouch frame tracker

- add protocol-B slot and tracking-ID parsing;
- emit normalized begin/update/end touch frames;
- calculate contact count, centroid, displacement, frame interval, and velocity;
- add live `frames`, raw `record`, and offline `replay` commands;
- keep frame production independent from gesture recognition and actions.


## 0.2.0 - read-only device observer

- add evdev device discovery and touchpad candidate classification;
- add raw, non-grabbing event observation with JSON Lines output;
- add `devices` and `monitor` CLI commands;
- keep hardware observation separate from gesture recognition and actions.


## 0.1.0 - unreleased

- Initial Rust workspace.
- Generic event, binding, condition, and action models.
- Provider registries and matcher engine.
- Config validation and live reload.
- Unix-socket daemon and simulation CLI.
- Safe null device backend.
