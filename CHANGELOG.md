# Changelog

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
