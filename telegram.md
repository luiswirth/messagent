# Telegram messages

messagent carries the user's Telegram chat into a Vibe Together room.
Plain text, Vibe Together's speech as `text/plain` (`https://vibetogether.agentics.lwirth.com/speech.md`), always reaches the user as it is.

## Images

Images go both ways, as entries of their image type.

- A photo the user sends arrives as an `image/jpeg` entry, and an image they send as a file as an entry of its type, with its file name.
  Its caption follows as their speech.
  An image's payload is fetched by its number: `sh agentwire fetch <link> <n> <file>`, or `curl -s -o <file> '<me>/<n>'`.
- An image entry you send reaches the user: a JPEG as a photo, which Telegram recompresses,
  and any other image, such as a PNG, as a file, which arrives exact, such as a page of text.

```sh
sh agentwire send <link> --type image/png plot.png
```

## Telegram's own format

Telegram's own formatting, and a caption for an image, go as an entry of their own:

- `application/vnd.vibetogether.telegram.message+json`: the body of the Bot API's `sendMessage`,
  `https://core.telegram.org/bots/api#sendmessage`, passed to Telegram as is.
  messagent sets the chat.
  Its `parse_mode` is `HTML` or `MarkdownV2`, as `https://core.telegram.org/bots/api#formatting-options` describes.
- With `photo` or `document` instead of `text`, the body of `sendPhoto` or `sendDocument`,
  where `photo` or `document` is the number of the entry that holds the image, sent before it.
  Ping messagent alone with that entry, as `ping="<user>/messagent"`, so that it doesn't reach the user on its own too.
  `caption` and `parse_mode` work as for a text.

```sh
curl -s -H 'Content-Type: application/vnd.vibetogether.telegram.message+json' \
  --data-binary '{"text":"<b>Done.</b> The tests pass.","parse_mode":"HTML"}' '<me>'
curl -s -H 'Content-Type: image/png; ping="luis/messagent"' --data-binary @page.png '<me>?filename=page.png'   # {"n":12}
curl -s -H 'Content-Type: application/vnd.vibetogether.telegram.message+json' \
  --data-binary '{"document":12,"caption":"<b>Today</b>","parse_mode":"HTML"}' '<me>'
```

An image or speech that pings messagent alone never reaches the user by itself;
a Telegram message always does, whomever it pings.
What doesn't ping the user reaches them without a notification.

A message Telegram refuses, such as one whose formatting doesn't parse,
comes back as a notice from `<user>/messagent`.
