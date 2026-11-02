//! Telegram as the messenger: a bot whose private chat with the user is the chat.

mod bot;

use std::{fs, io::ErrorKind, path::PathBuf, thread, time::Duration};

pub use bot::{Bot, MESSAGE_LIMIT, Message, Refusal, Update, User};

use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{
    Entry, Party, Paths,
    config::write_private,
    log::local,
    messenger::{CONNECTED, Messages, Messenger, Reactions, split},
};

/// How long Telegram keeps an update that no one fetched.
pub const KEPT: SignedDuration = SignedDuration::from_hours(24);

/// Telegram's part of the config.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Settings {
    /// The bot's token, from BotFather.
    pub token: String,
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
            bot: Bot::new(&settings.token),
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

    fn send(&self, message: &str, format: Option<&str>) -> Result<String> {
        let sent = self.bot.send_message(self.user()?, message, format)?;
        Ok(sent.message_id.to_string())
    }

    fn receive(&mut self) -> Result<Vec<Entry>> {
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
                        if let Err(error) = self.bot.send_message(user, CONNECTED, None) {
                            eprintln!("messagent: can't confirm the connection: {error:#}");
                        }
                    }
                    return Ok(fetched.entries);
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
    pub entries: Vec<Entry>,
    /// The user who just connected with the setup code.
    pub connected: Option<i64>,
}

/// Takes in the updates of one fetch at `now`:
/// the user's messages become entries, and a gap longer than Telegram keeps updates a notice.
pub fn take(state: &mut State, now: Timestamp, updates: Vec<Update>) -> Fetched {
    let mut fetched = Fetched::default();
    if let Some(last) = state.fetched
        && now.duration_since(last) > KEPT
    {
        fetched.entries.push(Entry::notice(
            now,
            format!(
                "Nothing was fetched from Telegram between {} and {}. \
                 Telegram keeps messages for 24 hours, so messages from that time may be lost.",
                local(last),
                local(now),
            ),
        ));
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
        if message.chat.kind != "private" {
            continue;
        }
        let Some(sender) = &message.from else {
            continue;
        };
        match state.user {
            Some(user) if user == sender.id => fetched.entries.push(Entry {
                time: Timestamp::from_second(message.date).unwrap_or(now),
                from: Party::User,
                name: Some(sender.first_name.clone()),
                message: Some(message.message_id.to_string()),
                text: text_of(&message),
            }),
            None if opens_with(&message, state.code.as_deref()) => {
                state.user = Some(sender.id);
                state.code = None;
                fetched.connected = Some(sender.id);
                fetched.entries.push(Entry::notice(
                    now,
                    format!("{} connected as the user.", sender.first_name),
                ));
            }
            _ => {}
        }
    }
    fetched
}

/// Whether the message is the start link's: `/start <code>`, or the code alone.
fn opens_with(message: &Message, code: Option<&str>) -> bool {
    let (Some(text), Some(code)) = (message.text.as_deref(), code) else {
        return false;
    };
    let text = text.trim();
    text == code || text.strip_prefix("/start").map(str::trim) == Some(code)
}

/// The text of a message, with a note on what else it carries that the log can't hold yet.
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
        .find(|(field, _)| message.rest.contains_key(*field))
        .map_or("message", |(_, kind)| kind);
    let note = format!("[{kind}: messagent carries only text so far]");
    match &message.caption {
        Some(caption) => format!("{caption}\n{note}"),
        None => note,
    }
}
