# Every check: formatting, lints and the tests, the bridge's end to end included.
ci:
    nix fmt -- --ci
    cargo clippy --all-targets -- -D warnings
    cargo test
    nix build --no-link .#default

# Moves the pin of Vibe Together's crates to a commit of its main, the latest by default.
bump rev=`git ls-remote https://github.com/PotentialHedge/vibetogether refs/heads/main | cut -f1`:
    sed -i 's/rev = "[0-9a-f]*"/rev = "{{ rev }}"/' Cargo.toml
    cargo update -p vibetogether-client -p vibetogether-backend
