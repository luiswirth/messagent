# messagent

Message your agent in a messaging app, like a friend:
Vibe Together's bridge, which mirrors a messenger chat and a Vibe Together session onto each other.
Everything here is Luis's decision, except what is marked as open.
How each decision came about is in this file's Git history, in Vibe Together's from 2026-11-03 to 2026-11-11 (`bridges/messagent`), and in this repository's first issue.
Vibe Together's own design, which this builds on, is in the collective's `projects/vibetogether/design.md`.

## What it is

- It exists only for Vibe Together, in its own repository, built on it, Luis's decision (2026-11-16): "own repos, built on".
  It changes nothing in Vibe Together, and calls it only through its client library.
- The UI is the messenger: no UI of our own.
  It is managed in the messenger itself, through the Vibe Together Telegram bot, `@vt_messagent_bot`.
- It embraces how people already use each messenger, and is agnostic to which one:
  each connects with the capabilities it has.
  The capabilities are traits: `Messages`, finished content in what the messenger carries, and `Calls`, live audio both ways.
  Reacting is an optional capability of its own.
- Telegram first; Discord comes into messagent later.
  WhatsApp is not pursued for now: its setup and its rules on AI make it a hassle.
- One messenger chat, private or a group, is one bridged session.
- In Rust.

## The bridge

- One bot for everyone, `@vt_messagent_bot`: Telegram gives a bot's updates to one receiver only.
  It runs on lwirth-server for now, for the accounts its config allows, Luis's alone.
- The bridge speaks only as the people it carries, and adds no participant of its own.
  - A person links once, by OTP: the bot shows a link and a code, and the person approves on the page that shows the same code.
    With Vibe Together's rewrite (2026-11-11) the bridge grants went, so the OTP is one of a person token, which messagent keeps; open whether Luis agrees.
    It lasts until revoked, with `/unlink` or in the account's logins.
  - Vibe Together knows nothing of the messenger: messagent alone keeps which Telegram user is which account.
  - A linked person speaks under their account name.
    Anyone else in a group has no voice in the session, since only a person token speaks: `telegram/<name>`, unverified, went with the bridge grants.
- The bot manages it: `/start` links in the private chat, and once linked, offers the person's sessions as buttons, where the one pressed gets the chat.
  `/stop` ends a chat's bridge.
- It passes everything both ways, so the bot's group privacy mode is off.
- It marks nothing read for the people it carries, since it can't tell what they read in the messenger: it reads the session's log, which has no side effects.
- A message the agents have read gets 👀 in the messenger.
- Images are first-class, both ways.
- The agent writes in each messenger's own format, passed through raw.
  It can react to messages, through the reactions extension.
- No typing indicator.

## Open

- Groups, as built with the bridge, with these points, which Luis has not reviewed:
  the sender is the human `from.id` alone, never a bot, a channel or an anonymous admin;
  a forward is marked with its origin, and no quoted text comes in; an edit is marked and counts as its sender's message;
  the group's title is a one-line label of at most 60 characters;
  a group nobody bridges is left after 10 minutes;
  a migration to a supergroup moves the bridge.
- Calls: Telegram bots can't call. A call would use voice's core.
- Voice notes, through voice's speech engines.
- Handing over the phone: messagent puts another agent on the chat, which answers through the same bot until it hands back.
