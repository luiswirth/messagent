//! Calls on Discord: the bot follows the user into the server's voice channels, through songbird.
//! It joins the channel the user joins, and leaves when they leave.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use songbird::{
    Config, CoreEvent, Event, EventContext, EventHandler, Songbird,
    driver::{Channels, DecodeConfig, DecodeMode, SampleRate},
    input::Input,
    shards::TwilightMap,
    tracks::TrackHandle,
};
use tokio::runtime::Handle;
use twilight_gateway::MessageSender;
use twilight_model::id::{
    Id,
    marker::{ChannelMarker, GuildMarker, UserMarker},
};

use crate::calls::{Call, Clip, FRAME, Speaker};

/// The bot's voice: which channel it is in, and where the user's voice goes.
pub struct Voice {
    songbird: Arc<Songbird>,
    runtime: Handle,
    user: Id<UserMarker>,
    guild: Id<GuildMarker>,
    channel: Option<Id<ChannelMarker>>,
    /// Where the user's voice goes during a call; dropping it ends the call for `calls`.
    heard: Arc<Mutex<Option<mpsc::SyncSender<Vec<i16>>>>>,
    calls: mpsc::Sender<Call>,
}

impl Voice {
    pub fn new(
        runtime: Handle,
        shard: MessageSender,
        bot: Id<UserMarker>,
        user: Id<UserMarker>,
        guild: Id<GuildMarker>,
        calls: mpsc::Sender<Call>,
    ) -> Self {
        let config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::new(
            Channels::Mono,
            SampleRate::Hz48000,
        )));
        let shards = Arc::new(TwilightMap::new(HashMap::from([(0, shard)])));
        Self {
            songbird: Arc::new(Songbird::twilight_from_config(shards, bot, config)),
            runtime,
            user,
            guild,
            channel: None,
            heard: Arc::default(),
            calls,
        }
    }

    /// Hands the Gateway's events to songbird, which needs the voice ones.
    pub fn process(&self, event: &twilight_gateway::Event) {
        self.runtime.block_on(self.songbird.process(event));
    }

    /// Follows the user to the voice channel they are in now, or out of the one they left.
    pub fn follow(&mut self, channel: Option<Id<ChannelMarker>>) {
        if channel == self.channel {
            return;
        }
        self.channel = channel;
        // The call so far ends, also where the user only moved.
        self.heard.lock().expect("no panics").take();
        let (songbird, guild) = (self.songbird.clone(), self.guild);
        let Some(channel) = channel else {
            self.runtime.spawn(async move {
                if let Err(error) = songbird.remove(guild).await {
                    eprintln!("messagent: can't leave the voice channel: {error}");
                }
            });
            return;
        };
        let (sender, heard) = mpsc::sync_channel(500);
        *self.heard.lock().expect("no panics") = Some(sender);
        let call = Call {
            heard,
            speaker: Box::new(DiscordSpeaker {
                songbird: songbird.clone(),
                runtime: self.runtime.clone(),
                guild,
            }),
        };
        let (shared, user, calls) = (self.heard.clone(), self.user, self.calls.clone());
        self.runtime.spawn(async move {
            // The handlers come first: Discord names the user's stream as the bot joins,
            // and a handler added after that never learns it.
            let handler = songbird.get_or_insert(guild);
            let joining = {
                let mut handler = handler.lock().await;
                let receiver = Arc::new(Receiver {
                    user: user.get(),
                    ssrc: Mutex::new(None),
                    heard: shared,
                });
                handler.remove_all_global_events();
                handler.add_global_event(
                    CoreEvent::SpeakingStateUpdate.into(),
                    Shared(receiver.clone()),
                );
                handler.add_global_event(CoreEvent::VoiceTick.into(), Shared(receiver));
                handler.join(channel).await
            };
            match joining {
                Ok(joined) => match joined.await {
                    Ok(()) => {
                        calls.send(call).ok();
                    }
                    Err(error) => eprintln!("messagent: can't join the voice channel: {error}"),
                },
                Err(error) => eprintln!("messagent: can't join the voice channel: {error}"),
            }
        });
    }
}

/// Takes the user's voice out of the call, one frame per tick, silence included.
struct Receiver {
    user: u64,
    /// The user's stream in the call, once Discord has named it.
    ssrc: Mutex<Option<u32>>,
    heard: Arc<Mutex<Option<mpsc::SyncSender<Vec<i16>>>>>,
}

struct Shared(Arc<Receiver>);

#[async_trait]
impl EventHandler for Shared {
    async fn act(&self, context: &EventContext<'_>) -> Option<Event> {
        let receiver = &self.0;
        match context {
            EventContext::SpeakingStateUpdate(speaking) => {
                if speaking.user_id.map(|id| id.0) == Some(receiver.user) {
                    *receiver.ssrc.lock().expect("no panics") = Some(speaking.ssrc);
                }
            }
            EventContext::VoiceTick(tick) => {
                let ssrc = *receiver.ssrc.lock().expect("no panics");
                let frame = ssrc
                    .and_then(|ssrc| tick.speaking.get(&ssrc))
                    .and_then(|data| data.decoded_voice.clone())
                    .unwrap_or_else(|| vec![0; FRAME]);
                if let Some(heard) = receiver.heard.lock().expect("no panics").as_ref() {
                    // A full queue means the speech layer is stuck; a frame lost then matters little.
                    heard.try_send(frame).ok();
                }
            }
            _ => {}
        }
        None
    }
}

struct DiscordSpeaker {
    songbird: Arc<Songbird>,
    runtime: Handle,
    guild: Id<GuildMarker>,
}

impl Speaker for DiscordSpeaker {
    fn play(&self, wav: Vec<u8>) -> Result<Box<dyn Clip>> {
        let handler = self
            .songbird
            .get(self.guild)
            .context("the bot is in no voice channel")?;
        let track = self
            .runtime
            .block_on(async { handler.lock().await.play_input(Input::from(wav)) });
        Ok(Box::new(DiscordClip {
            track,
            runtime: self.runtime.clone(),
        }))
    }
}

struct DiscordClip {
    track: TrackHandle,
    runtime: Handle,
}

impl Clip for DiscordClip {
    fn position(&self) -> Option<Duration> {
        let state = self.runtime.block_on(self.track.get_info()).ok()?;
        (!state.playing.is_done()).then_some(state.position)
    }

    fn stop(&self) {
        self.track.stop().ok();
    }
}
