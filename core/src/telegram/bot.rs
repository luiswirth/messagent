//! The few methods of Telegram's Bot API that messagent uses.

use std::{fmt, time::Duration};

use anyhow::Result;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

/// Telegram's limit on the length of a message, in UTF-16 code units.
pub const MESSAGE_LIMIT: usize = 4096;

/// How long a long poll waits for updates, in seconds.
pub const POLL_SECONDS: u64 = 50;

/// Telegram's Bot API, where none other is set up.
pub const API: &str = "https://api.telegram.org";

pub struct Bot {
    agent: ureq::Agent,
    api: String,
    token: String,
}

/// Telegram refused a request, as opposed to the network failing.
#[derive(Debug)]
pub struct Refusal {
    pub code: i64,
    pub description: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Telegram refused ({}): {}", self.code, self.description)
    }
}

impl std::error::Error for Refusal {}

#[derive(Deserialize)]
struct Response<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize, Debug)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Debug)]
pub struct Message {
    pub message_id: i64,
    pub date: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
    pub caption: Option<String>,
    /// The fields not named above, which say what else the message carries.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Deserialize, Debug)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

impl Bot {
    /// The bot of `token`, at the Bot API at `api`.
    pub fn new(api: &str, token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(POLL_SECONDS + 30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            api: api.trim_end_matches('/').into(),
            token: token.into(),
        }
    }

    fn call<T: DeserializeOwned>(&self, method: &str, body: Value) -> Result<T> {
        let url = format!("{}/bot{}/{method}", self.api, self.token);
        // The URL holds the token, which an error must not show.
        let mut response = self.agent.post(&url).send_json(body).map_err(|error| {
            let error = error.to_string().replace(&self.token, "<token>");
            anyhow::anyhow!("{method} failed: {error}")
        })?;
        let response: Response<T> = response.body_mut().read_json()?;
        match response.result {
            Some(result) if response.ok => Ok(result),
            _ => Err(Refusal {
                code: response.error_code,
                description: response.description,
            }
            .into()),
        }
    }

    pub fn get_me(&self) -> Result<User> {
        self.call("getMe", json!({}))
    }

    /// Waits up to `POLL_SECONDS` for updates from `offset` on,
    /// and confirms those before it, which Telegram then forgets.
    pub fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        self.call(
            "getUpdates",
            json!({
                "offset": offset,
                "timeout": POLL_SECONDS,
                "allowed_updates": ["message"],
            }),
        )
    }

    /// Sets the bot's reaction to a message, or removes it with an empty emoji.
    pub fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()> {
        let reaction = match emoji {
            "" => json!([]),
            emoji => json!([{ "type": "emoji", "emoji": emoji }]),
        };
        self.call::<bool>(
            "setMessageReaction",
            json!({ "chat_id": chat, "message_id": message, "reaction": reaction }),
        )?;
        Ok(())
    }

    /// Sends a plain text.
    pub fn send_message(&self, chat: i64, text: &str) -> Result<Message> {
        self.send_raw(chat, Map::from_iter([("text".into(), text.into())]))
    }

    /// Sends a message given as the body of `sendMessage`, as is, to `chat`.
    pub fn send_raw(&self, chat: i64, mut body: Map<String, Value>) -> Result<Message> {
        body.insert("chat_id".into(), chat.into());
        self.call("sendMessage", Value::Object(body))
    }
}
