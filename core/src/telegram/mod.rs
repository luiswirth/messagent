//! Telegram as the messenger: a bot whose private chat with the user is the chat.

mod bot;

use std::{fs, io::ErrorKind, path::PathBuf, thread, time::Duration};

pub use bot::{API, Bot, MESSAGE_LIMIT, Message, Refusal, Update, Upload, User};

use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    Paths,
    config::write_private,
    messenger::{Arrived, CONNECTED, File, Image, Messages, Messenger, Reactions, Stranger, split},
};

/// The type of a message in Telegram's own format: the body of the Bot API's `sendMessage`,
/// passed to it as is, with the chat set by messagent; or of `sendPhoto` or `sendDocument`,
/// where its `photo` or `document` is the number of the entry that holds the file.
pub const NATIVE: &str = "application/vnd.agentwire.telegram.message+json";

/// How long Telegram keeps an update that no one fetched.
pub const KEPT: SignedDuration = SignedDuration::from_hours(24);

/// Telegram's part of the config.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Settings {
    /// The bot's token, from BotFather.
    pub token: String,
    /// The Bot API, if not Telegram's own, such as a fake one for tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
}

/// What Telegram's part keeps across runs.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct State {
    /// The user's id, which is also the id of their private chat with the bot.
    /// Unset until the user opens the start link with the code.
    pub user: Option<i64>,
    pub code: Option<String>,
    /// The first update not fetched yet.
    pub offset: i64,
    /// When a fetch last succeeded.
    pub fetched: Option<Timestamp>,
}

pub struct Telegram {
    pub bot: Bot,
    pub state: State,
    file: PathBuf,
}

impl Telegram {
    pub fn open(settings: &Settings, paths: &Paths) -> Result<Self> {
        let file = paths.messenger_state("telegram");
        let state = match fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(error) if error.kind() == ErrorKind::NotFound => State::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            bot: Bot::new(settings.api.as_deref().unwrap_or(API), &settings.token),
            state,
            file,
        })
    }

    /// Starts over with a new code, and returns the start link:
    /// whoever opens it and taps Start becomes the user.
    pub fn setup(&mut self, code: String) -> Result<String> {
        let username = self
            .bot
            .get_me()?
            .username
            .context("the bot has no username")?;
        self.state = State {
            code: Some(code.clone()),
            ..State::default()
        };
        self.save()?;
        Ok(format!("https://t.me/{username}?start={code}"))
    }

    fn save(&self) -> Result<()> {
        write_private(&self.file, &serde_json::to_vec(&self.state)?)
    }

    fn user(&self) -> Result<i64> {
        self.state
            .user
            .context("no user is connected yet: they open the link `messagent setup` printed")
    }
}

impl Messages for Telegram {
    fn split(&self, text: &str) -> Vec<String> {
        split(text, MESSAGE_LIMIT)
    }

    fn send(&self, message: &str, quiet: bool) -> Result<String> {
        let mut body = Map::from_iter([("text".into(), message.into())]);
        silence(&mut body, quiet);
        let sent = self.bot.send_raw(self.user()?, body)?;
        Ok(sent.message_id.to_string())
    }

    fn native(&self) -> &'static str {
        NATIVE
    }

    fn send_image(&self, image: &Image, quiet: bool) -> Result<String> {
        let user = self.user()?;
        let upload = upload(image);
        let mut body = Map::new();
        silence(&mut body, quiet);
        // A photo is recompressed to a JPEG of limited size, which suits a JPEG, such as a
        // photograph; any other image goes as a document, so that it arrives exact,
        // such as a page of text. One Telegram refuses as a photo goes as a document too.
        if is_jpeg(&image.kind)
            && let Ok(sent) = self
                .bot
                .send_file(user, "sendPhoto", "photo", &upload, body.clone())
        {
            return Ok(sent.message_id.to_string());
        }
        let sent = self
            .bot
            .send_file(user, "sendDocument", "document", &upload, body)?;
        Ok(sent.message_id.to_string())
    }

    fn download(&self, file: &File) -> Result<Vec<u8>> {
        self.bot.download(&file.id)
    }

    fn send_native(
        &self,
        message: &[u8],
        entry: &dyn Fn(i64) -> Result<Image>,
        quiet: bool,
    ) -> Result<String> {
        let mut body: Map<String, Value> = serde_json::from_slice(message).context(
            "a Telegram message is a JSON object, the body of sendMessage, sendPhoto or sendDocument",
        )?;
        if !body.contains_key("disable_notification") {
            silence(&mut body, quiet);
        }
        let user = self.user()?;
        for (method, field) in [("sendPhoto", "photo"), ("sendDocument", "document")] {
            let Some(value) = body.get(field) else {
                continue;
            };
            let n = value.as_i64().with_context(|| {
                format!("its {field} is the number of the entry that holds the file")
            })?;
            let image = entry(n)?;
            let sent = self
                .bot
                .send_file(user, method, field, &upload(&image), body)?;
            return Ok(sent.message_id.to_string());
        }
        let sent = self.bot.send_raw(user, body)?;
        Ok(sent.message_id.to_string())
    }

    fn connected(&self) -> bool {
        self.state.user.is_some()
    }

    fn receive(&mut self) -> Result<Vec<Arrived>> {
        // Saved only now, after the last entries reached the log:
        // fetching from this offset confirms the updates before it, which Telegram then forgets.
        self.save()?;
        let mut delay = Duration::from_secs(1);
        loop {
            match self.bot.get_updates(self.state.offset) {
                Ok(updates) => {
                    let fetched = take(&mut self.state, Timestamp::now(), updates);
                    if let Some(user) = fetched.connected {
                        self.save()?;
                        if let Err(error) = self.bot.send_message(user, CONNECTED) {
                            eprintln!("messagent: can't confirm the connection: {error:#}");
                        }
                    }
                    return Ok(fetched.arrived);
                }
                Err(error) => {
                    if let Some(refusal) = error.downcast_ref::<Refusal>()
                        && refusal.code == 401
                    {
                        return Err(error).context("the bot's token is no longer valid");
                    }
                    eprintln!("messagent: {error:#}; trying again in {delay:?}");
                    thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
}

impl Reactions for Telegram {
    fn react(&self, message: &str, emoji: &str) -> Result<()> {
        let message = message.parse().context("not a Telegram message id")?;
        self.bot.react(self.user()?, message, emoji)
    }
}

impl Messenger for Telegram {
    fn reactions(&self) -> Option<&dyn Reactions> {
        Some(self)
    }
}

/// What one fetch brought.
#[derive(Debug, Default, PartialEq)]
pub struct Fetched {
    pub arrived: Vec<Arrived>,
    /// The user who just connected with the setup code.
    pub connected: Option<i64>,
}

/// Takes in the updates of one fetch at `now`:
/// what the user and strangers sent, and a gap longer than Telegram keeps updates as a notice.
pub fn take(state: &mut State, now: Timestamp, updates: Vec<Update>) -> Fetched {
    let mut fetched = Fetched::default();
    if let Some(last) = state.fetched
        && now.duration_since(last) > KEPT
    {
        fetched.arrived.push(Arrived::Notice(format!(
            "Nothing was fetched from Telegram between {} and {}. \
             Telegram keeps messages for 24 hours, so messages from that time may be lost.",
            local(last),
            local(now),
        )));
    }
    state.fetched = Some(now);
    for update in updates {
        if update.update_id < state.offset {
            continue;
        }
        state.offset = update.update_id + 1;
        let Some(message) = update.message else {
            continue;
        };
        let Some(sender) = &message.from else {
            continue;
        };
        // Only the user's private chat with the bot is the conversation so far.
        let private = message.chat.kind == "private";
        match state.user {
            Some(user) if user == sender.id => {
                if private {
                    let image = image_of(&message);
                    let text = match image {
                        Some(_) => message.caption.clone(),
                        None => Some(text_of(&message)),
                    };
                    fetched.arrived.push(Arrived::Message {
                        id: message.message_id.to_string(),
                        image,
                        text,
                    });
                }
            }
            None if private && opens_with(&message, state.code.as_deref()) => {
                state.user = Some(sender.id);
                state.code = None;
                fetched.connected = Some(sender.id);
                fetched.arrived.push(Arrived::Notice(format!(
                    "{} connected as the user.",
                    sender.first_name
                )));
            }
            _ => fetched.arrived.push(Arrived::Stranger(Stranger {
                time: Timestamp::from_second(message.date).unwrap_or(now),
                id: sender.id.to_string(),
                name: sender.first_name.clone(),
                text: text_of(&message),
            })),
        }
    }
    fetched
}

/// A time as the user reads it, in the machine's time zone.
pub fn local(time: Timestamp) -> String {
    time.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// Whether the message is the start link's: `/start <code>`, or the code alone.
fn opens_with(message: &Message, code: Option<&str>) -> bool {
    let (Some(text), Some(code)) = (message.text.as_deref(), code) else {
        return false;
    };
    let text = text.trim();
    text == code || text.strip_prefix("/start").map(str::trim) == Some(code)
}

/// The image a message carries, as a photo or as a file of an image type.
fn image_of(message: &Message) -> Option<File> {
    if let Some(largest) = message.photo.as_ref().and_then(|sizes| sizes.last()) {
        return Some(File {
            id: largest.file_id.clone(),
            kind: "image/jpeg".into(),
            filename: None,
        });
    }
    let document = message.document.as_ref()?;
    let kind = document.mime_type.as_deref()?;
    kind.starts_with("image/").then(|| File {
        id: document.file_id.clone(),
        kind: kind.into(),
        filename: document.file_name.clone(),
    })
}

fn is_jpeg(kind: &str) -> bool {
    let base = kind.split(';').next().unwrap_or("").trim();
    base.eq_ignore_ascii_case("image/jpeg") || base.eq_ignore_ascii_case("image/jpg")
}

/// An image as Telegram takes a file: with a name, made up from its type where it has none.
fn upload(image: &Image) -> Upload<'_> {
    let filename = image.filename.as_deref().unwrap_or(
        match image.kind.split(';').next().unwrap_or("").trim() {
            "image/jpeg" => "image.jpg",
            "image/png" => "image.png",
            "image/gif" => "image.gif",
            "image/webp" => "image.webp",
            _ => "image",
        },
    );
    Upload {
        filename,
        kind: &image.kind,
        bytes: &image.bytes,
    }
}

/// The text of a message, with a note on what else it carries that messagent can't carry yet.
fn text_of(message: &Message) -> String {
    if let Some(text) = &message.text {
        return text.clone();
    }
    let kinds = [
        ("photo", "photo"),
        ("voice", "voice note"),
        ("audio", "audio"),
        ("video", "video"),
        ("video_note", "video note"),
        ("animation", "GIF"),
        ("document", "file"),
        ("sticker", "sticker"),
        ("location", "location"),
        ("contact", "contact"),
        ("poll", "poll"),
    ];
    let kind = kinds
        .iter()
        .find(|(field, _)| match *field {
            "photo" => message.photo.is_some(),
            "document" => message.document.is_some(),
            field => message.rest.contains_key(field),
        })
        .map_or("message", |(_, kind)| kind);
    let note = format!("[{kind}: messagent carries only text and images so far]");
    match &message.caption {
        Some(caption) => format!("{caption}\n{note}"),
        None => note,
    }
}

/// Sends a message without a notification, where it is `quiet`.
fn silence(body: &mut Map<String, Value>, quiet: bool) {
    if quiet {
        body.insert("disable_notification".into(), true.into());
    }
}
