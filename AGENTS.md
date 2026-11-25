---
description: "Message and call your agent in a messaging app, like a friend: a way for one agent to talk to its person, linked to the agent's inbox and outbox like a subscriber. Use when working on messagent, or on how I reach the lead by Telegram."
---

# messagent

It is a piece of Luis's agentics (`~/dev/agentics/AGENTS.md`), independent of Vibe Together, and depends only on MindIO (`~/dev/agentics/mindio`), the agent's inbox and outbox, through its crate, as the senses (`~/dev/agentics/senses`) do.

- `design.md`: what is decided, and the questions still open.
- `telegram.md`: the page for agents, what arrives and how to answer.
- `src/`: the one binary, `messagent`, one bot for several agents, each in a topic of its own; `tests/channel.rs` runs it end to end between a fake Telegram and scratch agents' IO.
- `just ci`: every check.
- It runs on lwirth-server for the lead, as a user service of Luis's, from dothome's `nixos/messagent.nix`.
- The history with Luis's words and the research is in the first issue; the Vibe Together bridge it was before is in this repository's history.

Work on messagent is paired: Luis guides it deliberately, and its core design is his.
Read the design before working on anything.
Record every explicit decision in it as it is made.
