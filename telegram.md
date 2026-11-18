# Telegram

messagent links an agent's inbox and outbox to a Telegram bot of its own.
The inbox and its file convention: `https://github.com/luiswirth/agentinbox/blob/main/AGENTS.md`; the outbox is its dual.

## What arrives

A message from one of the bot's people, in their private chat with it, arrives in the inbox as an item from `telegram:<name>`, proposing a ring:
a text item whose first line says it is a Telegram message, from whom, and what it replies to, quoted, where it replies to one.
A photo, a file, a voice note or a video arrives as an item of its own, before the text, which names it.
Once the agent takes an item in, its message gets 👀 in Telegram.

## Answering

An answer is an item for `telegram:<name>` in the outbox: written into `tmp/`, then hard-linked into `new/`, named `<time>.telegram%3A<name>.<ext>`, as the inbox names its items.
messagent sends it by its extension:

- `md`: markdown, as Telegram's formatting; `txt`: plain text.
- `jpg`: a photo, which Telegram recompresses; any other, such as `png` or `pdf`, a file, which arrives exact.
- `json`: Telegram's own message, the body of `sendMessage` (`https://core.telegram.org/bots/api#sendmessage`) with `text`, `parse_mode`, `entities`, `link_preview_options` and `disable_notification` alone.

```sh
o=$AGENT_OUTBOX; n="$(date -u +%Y%m%dT%H%M%S.%6NZ).telegram%3Aluis.md"
printf '**Done.** The tests pass.\n' > "$o/tmp/$n" && ln "$o/tmp/$n" "$o/new/$n" && rm "$o/tmp/$n"
```

Or, with agentinbox's command line: `agentinbox send telegram:luis < answer.md`.

The name holds a `:`, which no Vibe Together name can, so no participant there passes for one of the bot's people.

What messagent can't send, such as an item for someone the bot doesn't know, comes back into the inbox as an item from `messagent`.
