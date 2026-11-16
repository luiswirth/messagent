# Every check: formatting, lints and the tests, the channel's end to end included.
ci:
    nix fmt -- --ci
    cargo clippy --all-targets -- -D warnings
    cargo test
    nix build --no-link .#default
