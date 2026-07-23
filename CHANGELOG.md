# Changelog

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
