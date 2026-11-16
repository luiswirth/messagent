# messagent

Message your agent in a messaging app, like a friend.
Everything here is Luis's decision, except what is marked as open.
How each decision came about is in this file's Git history, in Vibe Together's from 2026-11-03 to 2026-11-11 (`bridges/messagent`), and in this repository's first issue.

## What it is

- "messagent is just a way for one agent to talk to the user", Luis (2026-11-16).
  It is independent of Vibe Together: a peer that delivers into the agent's inbox, and part of Ag², which he called "more reasonable".
- "the common building block is truly the inbox": messagent depends only on the inbox's file convention (`~/dev/vibetogether/inbox/AGENTS.md`), as the senses do; "yes let's lead with the inbox".
- It is no bridge to a Vibe Together session, Luis (2026-11-16):
  mirroring a session is "kind of hacky", since his own messages come back from the bot, and in Vibe Together a Telegram message can't be told apart from a web one;
  and a client of the session is the wrong shape too, "because then the telegram chat and the room are fully out of sync".
  Group chats stay Vibe Together's job.
- The agent replies through its outbox, the inbox's dual, where it drops a file, Luis (2026-11-16): "outbox is a great idea".
  messagent links a messenger to the agent's inbox and outbox, "like a subscriber. i think that's a sensible shape."
  Telegram is one channel of it.
- One bot per agent, Luis (2026-11-16), who asked: "can ag^2 host a bot creator for telegram, such that it's seamless for the user?"
- It is anyone's to run for anyone; an allowlist is a deployment's own, Luis's decision (2026-11-16).
- The UI is the messenger: no UI of our own.
- It embraces how people already use each messenger, and is agnostic to which one:
  each connects with the capabilities it has.
  The capabilities are traits: `Messages`, finished content in what the messenger carries, and `Calls`, live audio both ways.
  Reacting is an optional capability of its own.
- Telegram first; Discord comes into messagent later.
  WhatsApp is not pursued for now: its setup and its rules on AI make it a hassle.
- Images are first-class, both ways.
- The agent writes in each messenger's own format, passed through raw.
- No typing indicator.
- In Rust.

## Open

- Creating the bot: Telegram's managed bots (Bot API 9.6) let a manager bot of Ag²'s create one once the user confirms, but the manager can always fetch or replace its token; or BotFather and a pasted token, with no one in between. Proposed: offer both, the manager's trust stated plainly.
- The outbox's convention, proposed to live beside the inbox's.
- Whether a message the agent has taken in gets 👀, and reactions, as in the bridge.
- Calls: Telegram bots can't call. A call would use voice's core.
- Voice notes, through voice's speech engines.
- Handing over the phone: messagent puts another agent on the chat, which answers through the same bot until it hands back.
