# Plugin direction

The Rust traits in `gesture-core::provider` are the in-process provider API for first-party crates.

A later stable plugin protocol will be out-of-process and newline-delimited JSON over a Unix socket. This avoids unstable Rust ABI coupling and lets plugins use any language.

Planned protocol operations:

- `hello`: negotiate protocol and provider versions;
- `describe`: list actions, conditions, parameter schemas, and capabilities;
- `validate`: validate a provider-specific configuration object;
- `execute`: run an action against a normalized event;
- `evaluate`: evaluate a condition against a normalized event;
- `cancel`: stop a continuous action;
- `health`: report plugin status.

Plugins will never receive raw input devices unless explicitly granted a hardware capability. Normal action plugins only receive normalized events and context.
