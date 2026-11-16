# Reactions

Reactions let a participant react to an entry with an emoji.
It is an extension of Vibe Together, like chat (`https://vibetogether.potentialhedge.com/specs/chat.md`).
messagent introduced it, so its spec lives here until a second client speaks it.

## Entries

- `application/vnd.messagent.reaction+json`, as `{"entry":12,"emoji":"👍"}`:
  the sender's reaction to entry `entry`.
  It replaces the sender's earlier reaction to that entry, and an empty `emoji` takes it back.

## Example

```sh
echo '{"entry":12,"emoji":"👍"}' | vibetogether send <session> --type application/vnd.messagent.reaction+json
```

## In messagent

A reaction to a message from the chat shows on it in the messenger, as the bot's own reaction,
and replaces the 👀 that marks it seen.
The messenger may take only some emoji: Telegram takes 👍 ❤ 🔥 🎉 😁 🤔 👌 and a few more.
One it refuses is dropped: the bot says nothing in the session.
