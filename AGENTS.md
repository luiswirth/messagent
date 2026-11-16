# messagent

Message and call your agent in a messaging app, like a friend: Vibe Together's bridge to messengers, which mirrors a chat and a session onto each other.

It is a piece of Ag² (`~/dev/agentics/AGENTS.md`), in its own repository, built on Vibe Together (`PotentialHedge/vibetogether`, `~/dev/vibetogether`), Luis's decision (2026-11-16): "own repos, built on".
It calls Vibe Together only through its client library, `vibetogether-client`, pinned to a commit of its main in `Cargo.toml`; `just bump` moves the pin.
Vibe Together's own framing is its `AGENTS.md` and the collective's `projects/vibetogether/design.md`.

- `design.md`: what is decided, and the questions still open.
- `telegram.md` and `reactions.md`: the extensions for Telegram's own messages and for reacting, for agents in a bridged session.
- `src/`: the one binary, `messagent`, the Telegram bot; `tests/bridge.rs` runs it end to end between a fake Telegram and a Vibe Together backend on loopback.
- `just ci`: every check.
- It runs on lwirth-server as a user service of Luis's, from dothome's `nixos/messagent.nix`, since it speaks as him.
- The history with Luis's words and the research is in the first issue.

Work on messagent is paired: Luis guides it deliberately, and its core design is his.
Read the design before working on anything.
Record every explicit decision in it as it is made.
Prose that records what Luis said needs no review.
