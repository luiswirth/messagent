//! Discord as the messenger: a bot whose DM with the user is the chat.
//! The user is the owner of the bot's application, since on Discord the user makes it.
//! The bot reaches the user only while they share a server, which calls will need anyway.
//! Messages arrive through the Gateway; Discord keeps the history,
//! so whatever came while no one watched is fetched once the Gateway is ready.

mod rest;

use std::{fs, io::ErrorKind, path::PathBuf, thread, time::Duration};

pub use rest::{Application, MESSAGE_LIMIT, PAGE, Refusal, Rest};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use twilight_gateway::{CloseFrame, Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt};
use twilight_model::{
    channel::message::{EmojiReactionType, Message, MessageFlags},
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker, UserMarker},
    },
};

use crate::{
    Entry, Party, Paths,
    config::write_private,
    messenger::{CONNECTED, Messages, Messenger, Reactions, split},
};

/// What the bot asks for when added to a server: seeing a voice channel, joining it and speaking.
/// The chat is a DM, which needs no permission.
const PERMISSIONS: u64 = (1 << 10) | (1 << 20) | (1 << 21);

/// Discord's error codes for a user the bot can't DM yet:
/// one who takes no DMs from it, and one it shares no server with.
const CANT_DM: [i64; 2] = [50007, 50278];

/// Discord's part of the config.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Settings {
    /// The bot's token, from the Developer Portal.
    pub token: String,
}

/// What Discord's part keeps across runs.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct State {
    /// The user: the owner of the bot's application.
    pub user: Option<Id<UserMarker>>,
    /// The DM with the user, set once the bot has reached them there.
    pub channel: Option<Id<ChannelMarker>>,
    /// The newest message of the DM taken in.
    pub last: Option<Id<MessageMarker>>,
}

pub struct Discord {
    rest: Rest,
    token: String,
    state: State,
    file: PathBuf,
    gateway: Option<Gateway>,
}

/// The connection that receives, opened by the first `receive`.
struct Gateway {
    runtime: tokio::runtime::Runtime,
    shard: Shard,
    /// Why Discord last closed the connection.
    closed: Option<CloseFrame<'static>>,
}

impl Discord {
    pub fn open(settings: &Settings, paths: &Paths) -> Result<Self> {
        let file = paths.messenger_state("discord");
        let state = match fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(error) if error.kind() == ErrorKind::NotFound => State::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            rest: Rest::new(&settings.token),
            token: settings.token.clone(),
            state,
            file,
            gateway: None,
        })
    }

    /// Starts over with the application's owner as the user,
    /// and returns the link that adds the bot to a server, with the owner's name.
    pub fn setup(&mut self) -> Result<(String, String)> {
        let application = self
            .rest
            .application()
            .context("can't get the bot's application: is the token right?")?;
        let (user, name) = match (&application.team, &application.owner) {
            (Some(team), _) => (team.owner_user_id, "the team's owner".to_string()),
            (None, Some(owner)) => (
                owner.id,
                owner.global_name.clone().unwrap_or(owner.username.clone()),
            ),
            (None, None) => bail!("Discord names no owner of the bot's application"),
        };
        self.state = State {
            user: Some(user),
            ..State::default()
        };
        self.save()?;
        let link = format!(
            "https://discord.com/oauth2/authorize?client_id={}&scope=bot&permissions={PERMISSIONS}",
            application.id
        );
        Ok((link, name))
    }

    fn save(&self) -> Result<()> {
        write_private(&self.file, &serde_json::to_vec(&self.state)?)
    }

    fn channel(&self) -> Result<Id<ChannelMarker>> {
        self.state.channel.context(
            "the user is not connected yet: the bot reaches them once it is in a server of theirs \
             while `messagent watch` runs",
        )
    }

    /// Opens the DM with the user and greets them there, returning the notice for the log,
    /// or nothing while the bot can't reach them yet.
    fn connect(&mut self) -> Result<Option<Entry>> {
        let user = self
            .state
            .user
            .context("no user is set: `messagent setup` sets the application's owner")?;
        let channel = patiently(|| self.rest.open_dm(user))?;
        match patiently(|| self.rest.send(channel, CONNECTED)) {
            Ok(_) => {}
            Err(error)
                if error
                    .downcast_ref::<Refusal>()
                    .is_some_and(|refusal| CANT_DM.contains(&refusal.code)) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        self.state.channel = Some(channel);
        self.save()?;
        Ok(Some(Entry::notice(
            Timestamp::now(),
            "The user connected: the bot reached them in a DM.".into(),
        )))
    }

    /// Connects where the user isn't yet, and fetches what came while no one watched.
    fn catch_up(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        if self.state.channel.is_none() {
            entries.extend(self.connect()?);
        }
        let Some(channel) = self.state.channel else {
            return Ok(entries);
        };
        while let Some(after) = self.state.last {
            let page = patiently(|| self.rest.messages_after(channel, after))?;
            let full = page.len() == PAGE;
            entries.extend(take(&mut self.state, page));
            if !full {
                break;
            }
        }
        Ok(entries)
    }
}

impl Gateway {
    fn connect(token: &str) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let intents = Intents::DIRECT_MESSAGES | Intents::GUILDS;
        // The shard's queue starts a task, so it is made within the runtime.
        let shard = {
            let _runtime = runtime.enter();
            Shard::new(ShardId::ONE, token.into(), intents)
        };
        Ok(Self {
            runtime,
            shard,
            closed: None,
        })
    }

    /// The next event of interest, closes always among them;
    /// it rides out passing failures, and the shard reconnects itself.
    fn next(&mut self) -> Result<Event> {
        let wanted =
            EventTypeFlags::READY | EventTypeFlags::GUILD_CREATE | EventTypeFlags::MESSAGE_CREATE;
        loop {
            match self.runtime.block_on(self.shard.next_event(wanted)) {
                Some(Ok(Event::GatewayClose(frame))) => {
                    self.closed = frame;
                }
                Some(Ok(event)) => return Ok(event),
                Some(Err(error)) => eprintln!("messagent: Discord's Gateway: {error}"),
                None => match &self.closed {
                    // https://docs.discord.com/developers/topics/opcodes-and-status-codes#gateway-gateway-close-event-codes
                    Some(frame) if frame.code == 4004 => {
                        bail!("Discord no longer takes the bot's token")
                    }
                    Some(frame) => bail!(
                        "Discord closed the Gateway for good ({}): {}",
                        frame.code,
                        frame.reason
                    ),
                    None => bail!("Discord closed the Gateway for good"),
                },
            }
        }
    }
}

/// Retries a request through passing failures: the network, and Discord's own errors.
fn patiently<T>(mut request: impl FnMut() -> Result<T>) -> Result<T> {
    let mut delay = Duration::from_secs(1);
    loop {
        match request() {
            Ok(value) => return Ok(value),
            Err(error)
                if error
                    .downcast_ref::<Refusal>()
                    .is_some_and(|refusal| refusal.status < 500) =>
            {
                return Err(error);
            }
            Err(error) => {
                eprintln!("messagent: {error:#}; trying again in {delay:?}");
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

impl Messages for Discord {
    fn split(&self, text: &str) -> Vec<String> {
        split(text, MESSAGE_LIMIT)
    }

    fn send(&self, message: &str, format: Option<&str>) -> Result<String> {
        if format.is_some() {
            bail!("Discord has one format, its Markdown, always on: send without a format");
        }
        Ok(self.rest.send(self.channel()?, message)?.id.to_string())
    }

    fn receive(&mut self) -> Result<Vec<Entry>> {
        // Saved only now, after the last entries reached the log.
        self.save()?;
        if self.gateway.is_none() {
            self.gateway = Some(Gateway::connect(&self.token)?);
        }
        loop {
            let event = self.gateway.as_mut().context("connected above")?.next()?;
            let entries = match event {
                // A new session may have missed messages, which Discord's history still holds.
                Event::Ready(_) => self.catch_up()?,
                // A server shared with the user may be what lets the bot reach them.
                Event::GuildCreate(_) if self.state.channel.is_none() => self.catch_up()?,
                Event::MessageCreate(message) => {
                    let mut entries = Vec::new();
                    if self.state.channel.is_none()
                        && message.guild_id.is_none()
                        && Some(message.author.id) == self.state.user
                    {
                        entries.extend(self.connect()?);
                    }
                    entries.extend(take(&mut self.state, vec![message.0]));
                    entries
                }
                _ => Vec::new(),
            };
            if !entries.is_empty() {
                return Ok(entries);
            }
        }
    }
}

impl Reactions for Discord {
    fn react(&self, message: &str, emoji: &str) -> Result<()> {
        let channel = self.channel()?;
        let message = message.parse().context("not a Discord message id")?;
        // Reactions add up on Discord, so the agent's earlier ones go first.
        for reaction in self.rest.message(channel, message)?.reactions {
            if let EmojiReactionType::Unicode { name } = &reaction.emoji
                && reaction.me
                && name != emoji
            {
                self.rest.unreact(channel, message, name)?;
            }
        }
        self.rest.react(channel, message, emoji)
    }
}

impl Messenger for Discord {
    fn reactions(&self) -> Option<&dyn Reactions> {
        Some(self)
    }
}

/// Takes in messages: the user's in the DM become entries, each once.
pub fn take(state: &mut State, mut messages: Vec<Message>) -> Vec<Entry> {
    messages.sort_by_key(|message| message.id);
    let mut entries = Vec::new();
    for message in messages {
        if state.channel != Some(message.channel_id) || state.last >= Some(message.id) {
            continue;
        }
        state.last = Some(message.id);
        if Some(message.author.id) != state.user {
            continue;
        }
        entries.push(Entry {
            time: Timestamp::from_microsecond(message.timestamp.as_micros())
                .unwrap_or_else(|_| Timestamp::now()),
            from: Party::User,
            name: Some(
                message
                    .author
                    .global_name
                    .clone()
                    .unwrap_or(message.author.name.clone()),
            ),
            message: Some(message.id.to_string()),
            text: text_of(&message),
        });
    }
    entries
}

/// The text of a message, with a note on what else it carries that the log can't hold yet.
fn text_of(message: &Message) -> String {
    let voice = message
        .flags
        .is_some_and(|flags| flags.contains(MessageFlags::IS_VOICE_MESSAGE));
    let mut kinds: Vec<&str> = message
        .attachments
        .iter()
        .map(|attachment| {
            let kind = attachment.content_type.as_deref().unwrap_or("");
            if voice {
                "voice note"
            } else if kind.starts_with("image/") {
                "image"
            } else if kind.starts_with("audio/") {
                "audio"
            } else if kind.starts_with("video/") {
                "video"
            } else {
                "file"
            }
        })
        .collect();
    kinds.extend(message.sticker_items.iter().map(|_| "sticker"));
    kinds.extend(message.poll.iter().map(|_| "poll"));
    if kinds.is_empty() && message.content.is_empty() {
        kinds.push("message");
    }
    let notes = kinds
        .iter()
        .map(|kind| format!("[{kind}: messagent carries only text so far]"));
    std::iter::once(message.content.clone())
        .filter(|content| !content.is_empty())
        .chain(notes)
        .collect::<Vec<_>>()
        .join("\n")
}
