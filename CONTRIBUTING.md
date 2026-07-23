# Contributing

Thank you for contributing to GestureForge.

## Design constraints

1. Do not hard-code desktop actions inside gesture recognizers.
2. New input sources emit normalized events.
3. New actions implement an action provider.
4. New context checks implement a condition provider.
5. Configuration changes must remain versioned and migratable.
6. Hardware-facing code must have replayable tests wherever possible.

## Development workflow

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Please include tests for configuration validation, matching rules, and event replay behavior.
