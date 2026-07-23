#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/gesture-forge"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$CONFIG_DIR" "$BIN_DIR" "$UNIT_DIR"
install -m755 "$ROOT/target/release/gesture-forged" "$BIN_DIR/gesture-forged"
install -m755 "$ROOT/target/release/gesture-forge" "$BIN_DIR/gesture-forge"

if [[ ! -e "$CONFIG_DIR/config.toml" ]]; then
    install -m644 "$ROOT/configs/config.example.toml" "$CONFIG_DIR/config.toml"
else
    echo "Keeping existing $CONFIG_DIR/config.toml"
fi

install -m644 "$ROOT/packaging/systemd/gesture-forge.service" \
    "$UNIT_DIR/gesture-forge.service"

systemctl --user daemon-reload

echo
echo "Installed the safe 0.1 foundation."
echo "The service was not enabled or started automatically."
echo "Validate: gesture-forge validate"
echo "Start manually: systemctl --user start gesture-forge.service"
