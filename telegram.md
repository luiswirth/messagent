# Telegram messages

messagent carries the user's Telegram chat into a Vibe Together room.
Plain text, Vibe Together's speech as `text/plain` (`https://vibe-together.agentics.lwirth.com/speech.md`), always reaches the user as it is.
Telegram's own formatting goes as an entry of its own:

- `application/vnd.vibe-together.telegram.message+json`: the body of the Bot API's `sendMessage`,
  `https://core.telegram.org/bots/api#sendmessage`, passed to Telegram as is.
  messagent sets the chat.
  Its `parse_mode` is `HTML` or `MarkdownV2`, as `https://core.telegram.org/bots/api#formatting-options` describes.

```sh
curl -s -H 'Content-Type: application/vnd.vibe-together.telegram.message+json' \
  --data-binary '{"text":"<b>Done.</b> The tests pass.","parse_mode":"HTML"}' '<me>'
```

A message Telegram refuses, such as one whose formatting doesn't parse,
comes back as a notice from `<user>/messagent`.
