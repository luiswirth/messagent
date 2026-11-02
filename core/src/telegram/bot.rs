//! The few methods of Telegram's Bot API that messagent uses.

use std::{fmt, time::Duration};

use anyhow::Result;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

/// Telegram's limit on the length of a message, in UTF-16 code units.
pub const MESSAGE_LIMIT: usize = 4096;

/// How long a long poll waits for updates, in seconds.
pub const POLL_SECONDS: u64 = 50;

pub struct Bot {
    agent: ureq::Agent,
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
    pub fn new(token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(POLL_SECONDS + 30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            token: token.into(),
        }
    }

    fn call<T: DeserializeOwned>(&self, method: &str, body: Value) -> Result<T> {
        let url = format!("https://api.telegram.org/bot{}/{method}", self.token);
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

    pub fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()> {
        self.call::<bool>(
            "setMessageReaction",
            json!({
                "chat_id": chat,
                "message_id": message,
                "reaction": [{ "type": "emoji", "emoji": emoji }],
            }),
        )?;
        Ok(())
    }

    /// Sends a text, formatted by the parse mode where there is one.
    pub fn send_message(&self, chat: i64, text: &str, parse_mode: Option<&str>) -> Result<Message> {
        let mut body = json!({ "chat_id": chat, "text": text });
        if let Some(parse_mode) = parse_mode {
            body["parse_mode"] = parse_mode.into();
        }
        self.call("sendMessage", body)
    }
}

/// Splits a text into messages within Telegram's limit,
/// at the last line break in the second half of each where there is one.
pub fn split(text: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut length = 0;
        let mut cut = rest.len();
        let mut line_break = None;
        for (index, character) in rest.char_indices() {
            length += character.len_utf16();
            if length > MESSAGE_LIMIT {
                cut = index;
                break;
            }
            if character == '\n' && length > MESSAGE_LIMIT / 2 {
                line_break = Some(index);
            }
        }
        let mut next = cut;
        if cut < rest.len()
            && let Some(line_break) = line_break
        {
            cut = line_break;
            next = line_break + 1;
        }
        messages.push(rest[..cut].to_string());
        rest = &rest[next..];
    }
    messages
}
