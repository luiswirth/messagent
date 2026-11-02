//! The fetch process: it long-polls Telegram, writes each message to the log
//! and runs the wake command.

use std::{
    fs::{self, File},
    io::ErrorKind,
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{
    Config, Entry, Log, Party, Paths,
    config::write_private,
    log::local,
    telegram::{Bot, Message, Refusal, Update},
};

/// How long Telegram keeps an update that no one fetched.
pub const KEPT: SignedDuration = SignedDuration::from_hours(24);

/// Where fetching stands, kept across runs.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Fetch {
    /// The first update not fetched yet.
    pub offset: i64,
    /// When a fetch last succeeded.
    pub fetched: Option<Timestamp>,
}

impl Fetch {
    pub fn load(paths: &Paths) -> Result<Self> {
        match fs::read_to_string(paths.fetch()) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        write_private(&paths.fetch(), &serde_json::to_vec(self)?)
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
pub fn take(
    config: &mut Config,
    fetch: &mut Fetch,
    now: Timestamp,
    updates: Vec<Update>,
) -> Fetched {
    let mut fetched = Fetched::default();
    if let Some(last) = fetch.fetched
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
    fetch.fetched = Some(now);
    for update in updates {
        if update.update_id < fetch.offset {
            continue;
        }
        fetch.offset = update.update_id + 1;
        let Some(message) = update.message else {
            continue;
        };
        if message.chat.kind != "private" {
            continue;
        }
        let Some(sender) = &message.from else {
            continue;
        };
        match config.user {
            Some(user) if user == sender.id => fetched.entries.push(Entry {
                time: Timestamp::from_second(message.date).unwrap_or(now),
                from: Party::User,
                name: Some(sender.first_name.clone()),
                update: Some(update.update_id),
                text: text_of(&message),
            }),
            None if config.code.is_some() && opens_with(&message, config.code.as_deref()) => {
                config.user = Some(sender.id);
                config.code = None;
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

/// Runs the wake command, one run at a time:
/// wakes asked for during a run become one more run after it.
pub struct Waker {
    sender: Option<mpsc::Sender<()>>,
}

impl Waker {
    pub fn spawn(command: String) -> Self {
        if command.trim().is_empty() {
            return Self { sender: None };
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            while receiver.recv().is_ok() {
                while receiver.try_recv().is_ok() {}
                match Command::new("sh").arg("-c").arg(&command).status() {
                    Ok(status) if status.success() => {}
                    Ok(status) => eprintln!("messagent watch: the wake command failed ({status})"),
                    Err(error) => eprintln!("messagent watch: can't run the wake command: {error}"),
                }
            }
        });
        Self {
            sender: Some(sender),
        }
    }

    pub fn wake(&self) {
        if let Some(sender) = &self.sender {
            sender.send(()).ok();
        }
    }
}

pub fn watch(paths: &Paths) -> Result<()> {
    let mut config = Config::load(paths)?;
    fs::create_dir_all(&paths.state)?;
    let lock = File::create(paths.watch_lock())?;
    if lock.try_lock().is_err() {
        bail!("another `messagent watch` is running");
    }
    let bot = Bot::new(&config.token);
    let log = Log::new(paths);
    let mut fetch = Fetch::load(paths)?;
    let logged = log.entries()?.iter().filter_map(|entry| entry.update).max();
    if let Some(logged) = logged {
        fetch.offset = fetch.offset.max(logged + 1);
    }
    let waker = Waker::spawn(config.wake.clone());
    let mut delay = Duration::from_secs(1);
    loop {
        let updates = match bot.get_updates(fetch.offset) {
            Ok(updates) => updates,
            Err(error) => {
                if let Some(refusal) = error.downcast_ref::<Refusal>()
                    && refusal.code == 401
                {
                    return Err(error).context("the bot's token is no longer valid");
                }
                eprintln!("messagent watch: {error:#}; trying again in {delay:?}");
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        delay = Duration::from_secs(1);
        let fetched = take(&mut config, &mut fetch, Timestamp::now(), updates);
        if let Some(user) = fetched.connected {
            config.save(paths)?;
            let confirmation = "Connected: your messages here now reach your agent.";
            if let Err(error) = bot.send_message(user, confirmation) {
                eprintln!("messagent watch: can't confirm the connection: {error:#}");
            }
        }
        if !fetched.entries.is_empty() {
            log.append(&fetched.entries)?;
            waker.wake();
        }
        fetch.save(paths)?;
    }
}
