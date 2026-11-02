# messagent

Message and call your agent in a messaging app, like a friend.
"messagent" is the name, Luis's decision.
The flagship for now, Luis's decision: "the messenger chat is soooo simple. everybody will udnerstand it."
It replaces polylogue's `chat`, and may subsume voice mode.
A piece of Ag² (`~/dev/agentics`), in a repository of its own, private for now, Luis's decision.

## Decided

- The UI is the messenger: no UI of our own, everything is infrastructure and backend, Luis's decision.
- It is built without polylogue for now, Luis's decision:
  the messenger chat is already the shared room, and with no relay a polylogue session would only be a local log between two programs.
- It is agnostic to the messenger, Luis's decision: each messenger connects on top with the capabilities it has.
- The capabilities are traits, Luis's decision: `Messages`, finished pieces of content in the modalities the messenger carries,
  and `Calls`, live audio both ways.
  A voice note is a message with audio content.
- Voice notes use the speech engines of voice mode, ElevenLabs and local, Luis's decision.
- It runs on the user's machine, where their agent already runs, Luis's decision.
  We host as little as possible, for cost and maintenance.
- Telegram first, as a bot that is a contact of its own, with voice notes for now, Luis's decision.
  WhatsApp is not pursued for now: its setup and its rules on AI make it a hassle.
  Phone numbers for an agent's own account wait until a Telegram bot's limits are reached.
- Onboarding creates the bot, through Telegram's managed bots, Luis's decision.
  The manager bot is ours to host, kept tiny.
  The user connects a phone by scanning a QR code, or by typing a code once.
- We are trustworthy to the user and to the user's agent, Luis's decision.
  Agents run in terminals and in GUIs alike, so onboarding assumes neither.
- Setup is a conversation with the agent, which acts as the setup flow, rather than a website, Luis's decision:
  "everything done through the agent", which he sees as a cool idea for all of Ag².
- What we hand out is a prompt, which points the agent at the resources it reads, Luis's decision.
  Its page for people, `messagent.agentics.lwirth.com`, a domain of its own as every client has, shows the prompt most prominently, with a little explanation, and is nice to look at.

- The agent is wakeable, not always running, Luis's decision.
  messagent keeps the messages, so the agent can also read them later:
  whether it answers at once or only checks now and then is the user's choice,
  which suits an agent on a laptop that is sometimes closed.
- A message wakes the agent through a small adapter per harness, Luis's decision.
  Senses and impulses are only a way of thinking about it for now, conceptual and philosophy, not something built: the messenger is the focus.
- A background process on the user's machine fetches the messages and stores them, Luis's decision:
  a bot gets no chat history, and Telegram keeps a message for it only until fetched, at most 24 hours.
  It need not always run, and may sleep with the laptop, but it must fetch each message within 24 hours.
  After it slept longer, a notice reaches the user that messages may have been lost before reaching the agent.
- Claude Code is woken through channels, Luis's decision.

- One messenger chat is one conversation, Luis's decision:
  the private chat with the bot is one, and a group with the bot is a shared one of several people and agents.
- No relay, Luis's decision: Telegram carries the traffic between phone and machine.
- The focus is only what the messenger needs, Luis's decision.

## Open

- Whether a bot is cut loose from the manager after onboarding, so that we hold no access to it.
