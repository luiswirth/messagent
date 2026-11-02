//! Discord as the messenger: a bot in the user's server, as a friend in a friend group's.
//! The user is the owner of the bot's application, since on Discord the user makes it.
//! The chat is the server's text channel where the user first writes once the bot is set up.
//! Messages arrive through the Gateway; Discord keeps the history,
//! so whatever came while no one watched is fetched once the Gateway is ready.

mod rest;
mod voice;

use std::{fs, io::ErrorKind, path::PathBuf, sync::mpsc, thread, time::Duration};

pub use rest::{Application, MESSAGE_LIMIT, PAGE, Refusal, Rest};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use twilight_gateway::{CloseFrame, Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt};
use twilight_model::{
    channel::message::{EmojiReactionType, Message, MessageFlags},
    id::{
        Id,
        marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
    },
};

use crate::{
    Entry, Party, Paths,
    calls::Call,
    config::write_private,
    messenger::{CONNECTED, Messages, Messenger, Reactions, split},
};

/// What the bot asks for when added to a server: in the text channel, seeing it,
/// reading its history, writing and reacting; in the voice channel, joining and speaking.
const PERMISSIONS: u64 = (1 << 10) | (1 << 16) | (1 << 11) | (1 << 6) | (1 << 20) | (1 << 21);

/// The application flag that lets a bot in under 100 servers read messages it isn't mentioned in.
const MESSAGE_CONTENT: u64 = 1 << 19;

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
    /// The text channel that is the chat, set by the user's first message in a server.
    pub channel: Option<Id<ChannelMarker>>,
    /// The server of the chat.
    pub guild: Option<Id<GuildMarker>>,
    /// The newest message of the DM taken in.
    pub last: Option<Id<MessageMarker>>,
}

pub struct Discord {
    rest: Rest,
    token: String,
    state: State,
    file: PathBuf,
    gateway: Option<Gateway>,
    /// Where calls go, once `watch` takes them.
    calls: Option<mpsc::Sender<Call>>,
    voice: Option<voice::Voice>,
    /// The bot's own user, from the Gateway.
    bot: Option<Id<UserMarker>>,
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
            calls: None,
            voice: None,
            bot: None,
        })
    }

    /// Starts over with the application's owner as the user, lets the bot read the server's messages,
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
        if application.flags & MESSAGE_CONTENT == 0 {
            self.rest
                .set_flags(application.flags | MESSAGE_CONTENT)
                .context("can't let the bot read the server's messages")?;
        }
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
            "the user is not connected yet: they write in a text channel of the bot's server \
             while `messagent watch` runs",
        )
    }

    /// Makes the channel of the user's message the chat, and greets them there,
    /// returning the notice for the log.
    fn connect(&mut self, message: &Message) -> Result<Entry> {
        self.state.channel = Some(message.channel_id);
        self.state.guild = message.guild_id;
        self.save()?;
        patiently(|| self.rest.send(message.channel_id, CONNECTED))?;
        Ok(Entry::notice(
            Timestamp::now(),
            "The user connected: the channel they wrote in is the chat.".into(),
        ))
    }

    /// Fetches what came while no one watched.
    fn catch_up(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
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

impl Discord {
    /// Lets the bot follow the user into voice channels, once `watch` takes calls
    /// and the bot knows itself and the server.
    fn give_voice(&mut self) {
        if let (Some(calls), Some(bot), Some(user), Some(guild), Some(gateway), None) = (
            &self.calls,
            self.bot,
            self.state.user,
            self.state.guild,
            &self.gateway,
            &self.voice,
        ) {
            self.voice = Some(voice::Voice::new(
                gateway.runtime.handle().clone(),
                gateway.shard.sender(),
                bot,
                user,
                guild,
                calls.clone(),
            ));
        }
    }
}

impl Gateway {
    fn connect(token: &str) -> Result<Self> {
        // Several threads, since a call's tasks run on also while no one waits for an event.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let intents = Intents::GUILDS
            | Intents::GUILD_MESSAGES
            | Intents::MESSAGE_CONTENT
            | Intents::GUILD_VOICE_STATES;
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
        let wanted = EventTypeFlags::READY
            | EventTypeFlags::MESSAGE_CREATE
            | EventTypeFlags::GUILD_CREATE
            | EventTypeFlags::VOICE_STATE_UPDATE
            | EventTypeFlags::VOICE_SERVER_UPDATE;
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
                    Some(frame) if frame.code == 4014 => {
                        bail!("the bot may not read messages: `messagent setup` lets it")
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
            let gateway = self.gateway.as_mut().context("connected above")?;
            let event = gateway.next()?;
            if let Some(voice) = &self.voice {
                voice.process(&event);
            }
            let entries = match event {
                // A new session may have missed messages, which Discord's history still holds.
                Event::Ready(ready) => {
                    self.bot = Some(ready.user.id);
                    self.give_voice();
                    self.catch_up()?
                }
                // Where the user is in a voice channel already, the bot joins them.
                Event::GuildCreate(guild) if Some(guild.id()) == self.state.guild => {
                    if let (
                        twilight_model::gateway::payload::incoming::GuildCreate::Available(guild),
                        Some(voice),
                    ) = (guild.as_ref(), self.voice.as_mut())
                    {
                        let channel = guild
                            .voice_states
                            .iter()
                            .find(|state| Some(state.user_id) == self.state.user)
                            .and_then(|state| state.channel_id);
                        voice.follow(channel);
                    }
                    Vec::new()
                }
                Event::VoiceStateUpdate(update)
                    if Some(update.user_id) == self.state.user
                        && update.guild_id == self.state.guild =>
                {
                    if let Some(voice) = &mut self.voice {
                        voice.follow(update.channel_id);
                    }
                    Vec::new()
                }
                Event::MessageCreate(message) => {
                    let mut entries = Vec::new();
                    if self.state.channel.is_none()
                        && message.guild_id.is_some()
                        && Some(message.author.id) == self.state.user
                    {
                        entries.push(self.connect(&message)?);
                        self.give_voice();
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

    fn calls(&mut self) -> Option<mpsc::Receiver<Call>> {
        let (sender, receiver) = mpsc::channel();
        self.calls = Some(sender);
        Some(receiver)
    }
}

/// Takes in messages: the user's in the chat become entries, each once.
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
            spoken: false,
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
