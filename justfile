# Warnings are errors here, so that a local build stays warning-tolerant.
export RUSTFLAGS := "-D warnings"
# Doc comments carry the mathematics, so a broken intra-doc link is an error.
export RUSTDOCFLAGS := "-D warnings"

# Everything CI runs.
ci:
    nix fmt -- --ci
    cargo clippy --all-targets
    cargo test
    cargo doc --no-deps
    just bridge

# The bridge end to end, between a fake Telegram and a relay of its own.
bridge:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet
    POLYLOGUE_ADDRESS=127.0.0.1:8792 polylogue-relay &
    trap 'kill $!' EXIT
    tests/bridge.sh http://localhost:8792 "${CARGO_TARGET_DIR:-target}/debug/messagent"
