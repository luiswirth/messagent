//! The few routes of Discord's HTTP API that messagent uses.

use std::{fmt, thread, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use twilight_model::{
    channel::Message,
    id::{
        Id,
        marker::{ApplicationMarker, ChannelMarker, MessageMarker, UserMarker},
    },
};

const API: &str = "https://discord.com/api/v10";

/// Discord's limit on the length of a message, in characters, counted here in UTF-16 code units.
pub const MESSAGE_LIMIT: usize = 2000;

/// How many messages one request for the history returns at most.
pub const PAGE: usize = 100;

pub struct Rest {
    agent: ureq::Agent,
    token: String,
}

/// Discord refused a request, as opposed to the network failing.
#[derive(Debug)]
pub struct Refusal {
    pub status: u16,
    /// Discord's own error code, such as 50007 for a user who takes no DMs from the bot.
    pub code: i64,
    pub message: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Discord refused ({} {}): {}",
            self.status, self.code, self.message
        )
    }
}

impl std::error::Error for Refusal {}

#[derive(Deserialize, Default)]
struct Error {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    /// Seconds to wait, where the refusal is a rate limit.
    retry_after: Option<f64>,
}

#[derive(Deserialize, Debug)]
pub struct Application {
    pub id: Id<ApplicationMarker>,
    pub owner: Option<Owner>,
    pub team: Option<Team>,
}

#[derive(Deserialize, Debug)]
pub struct Owner {
    pub id: Id<UserMarker>,
    pub username: String,
    pub global_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Team {
    pub owner_user_id: Id<UserMarker>,
}

#[derive(Deserialize)]
struct Channel {
    id: Id<ChannelMarker>,
}

impl Rest {
    pub fn new(token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            token: token.into(),
        }
    }

    /// Makes a request, waiting out rate limits, and returns the body, `null` where there is none.
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        loop {
            let request = ureq::http::Request::builder()
                .method(method)
                .uri(format!("{API}{path}"))
                .header("Authorization", format!("Bot {}", self.token))
                .header(
                    "User-Agent",
                    concat!(
                        "DiscordBot (https://github.com/luiswirth/messagent, ",
                        env!("CARGO_PKG_VERSION"),
                        ")"
                    ),
                )
                .header("Content-Type", "application/json")
                .body(body.as_str())?;
            let mut response = self
                .agent
                .run(request)
                .with_context(|| format!("{method} {path} failed"))?;
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string()?;
            if (200..300).contains(&status) {
                return Ok(if text.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(&text)?
                });
            }
            let error: Error = serde_json::from_str(&text).unwrap_or_default();
            match error.retry_after {
                Some(seconds) if status == 429 => thread::sleep(Duration::from_secs_f64(seconds)),
                _ => {
                    return Err(Refusal {
                        status,
                        code: error.code,
                        message: error.message,
                    }
                    .into());
                }
            }
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_value(self.call("GET", path, None)?)?)
    }

    /// The bot's application, with its owner.
    pub fn application(&self) -> Result<Application> {
        self.get("/oauth2/applications/@me")
    }

    /// The DM channel with a user, opened where there is none yet.
    pub fn open_dm(&self, user: Id<UserMarker>) -> Result<Id<ChannelMarker>> {
        let channel: Channel = serde_json::from_value(self.call(
            "POST",
            "/users/@me/channels",
            Some(json!({ "recipient_id": user })),
        )?)?;
        Ok(channel.id)
    }

    pub fn send(&self, channel: Id<ChannelMarker>, content: &str) -> Result<Message> {
        Ok(serde_json::from_value(self.call(
            "POST",
            &format!("/channels/{channel}/messages"),
            Some(json!({ "content": content })),
        )?)?)
    }

    pub fn message(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
    ) -> Result<Message> {
        self.get(&format!("/channels/{channel}/messages/{message}"))
    }

    /// Up to `PAGE` messages after the one given, in no set order.
    pub fn messages_after(
        &self,
        channel: Id<ChannelMarker>,
        after: Id<MessageMarker>,
    ) -> Result<Vec<Message>> {
        self.get(&format!(
            "/channels/{channel}/messages?after={after}&limit={PAGE}"
        ))
    }

    pub fn react(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) -> Result<()> {
        let emoji = encode(emoji);
        self.call(
            "PUT",
            &format!("/channels/{channel}/messages/{message}/reactions/{emoji}/@me"),
            None,
        )?;
        Ok(())
    }

    pub fn unreact(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) -> Result<()> {
        let emoji = encode(emoji);
        self.call(
            "DELETE",
            &format!("/channels/{channel}/messages/{message}/reactions/{emoji}/@me"),
            None,
        )?;
        Ok(())
    }
}

/// Percent-encodes an emoji for a path.
fn encode(emoji: &str) -> String {
    emoji.bytes().map(|byte| format!("%{byte:02X}")).collect()
}
