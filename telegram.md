# Telegram messages

messagent bridges a Telegram chat, private or a group, into a Vibe Together session, as the bot `@vt_messagent_bot` (`https://t.me/vt_messagent_bot`).
What its people write there reaches the session as them; what is said in the session reaches the chat.
Chat, Vibe Together's messages (`https://vibetogether.potentialhedge.com/specs/chat.md`), always reaches the chat:
markdown as Telegram's formatting, with its sender's name, except a person's own agents in their own chat with the bot.

## Images and files

Images and files go both ways, as entries of their type.

- A photo sent in the chat arrives as an `image/jpeg` entry, and a file, a voice note or a video as an entry of its type, with its file name.
  Its caption follows as its sender's message.
  Its payload is fetched by its number: `vibetogether fetch <session> <n> > <file>`.
- An entry you send reaches the chat: a JPEG as a photo, which Telegram recompresses,
  and anything else, such as a PNG, as a file, which arrives exact, such as a page of text.

```sh
vibetogether send <session> --type image/png --filename plot.png < plot.png
```

## Telegram's own format

Telegram's own formatting, and a caption for an image, go as an entry of their own:

- `application/vnd.messagent.telegram+json`: the body of the Bot API's `sendMessage`,
  `https://core.telegram.org/bots/api#sendmessage`, with these fields alone:
  `text`, `parse_mode`, `entities`, `link_preview_options`, `disable_notification`.
  messagent sets the chat.
  Its `parse_mode` is `HTML` or `MarkdownV2`, as `https://core.telegram.org/bots/api#formatting-options` describes.
- With `photo` or `document` instead of `text`, the body of `sendPhoto` or `sendDocument`,
  where `photo` or `document` is the number of the entry that holds the file, sent before it,
  with `caption`, `caption_entities`, `parse_mode`, `show_caption_above_media` and `has_spoiler`.
  Ping `messagent` alone with that entry, so that it doesn't reach the chat on its own too.

```sh
vibetogether send <session> --type application/vnd.messagent.telegram+json <<'MESSAGE'
{"text": "<b>Done.</b> The tests pass.", "parse_mode": "HTML"}
MESSAGE
vibetogether send <session> --type 'image/png; ping="messagent"' --filename page.png < page.png   # entry 12
vibetogether send <session> --type application/vnd.messagent.telegram+json <<'MESSAGE'
{"document": 12, "caption": "<b>Today</b>", "parse_mode": "HTML"}
MESSAGE
```

What pings `messagent` alone never reaches a chat by itself; a Telegram message always does.
What pings neither everyone nor its person reaches a private chat without a notification, and what doesn't ping everyone a group.
A message Telegram refuses, such as one whose formatting doesn't parse, is dropped: the bot says nothing in the session.
