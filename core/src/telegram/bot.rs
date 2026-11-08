//! The few methods of Telegram's Bot API that messagent uses.

use std::{fmt, time::Duration};

use anyhow::Result;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

/// Telegram's limit on the length of a message, in UTF-16 code units.
pub const MESSAGE_LIMIT: usize = 4096;

/// The largest file a bot may download, in bytes.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;

/// A boundary for a form, which no payload is likely to hold.
fn rand_boundary() -> u64 {
    let mut bytes = [0; 8];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        std::io::Read::read_exact(&mut random, &mut bytes).ok();
    }
    u64::from_ne_bytes(bytes)
}

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
    /// A photo, in the sizes Telegram keeps, the largest last.
    pub photo: Option<Vec<PhotoSize>>,
    /// A file sent as one, which may be an image.
    pub document: Option<Document>,
    /// The fields not named above, which say what else the message carries.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Deserialize, Debug)]
pub struct PhotoSize {
    pub file_id: String,
}

#[derive(Deserialize, Debug)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Deserialize, Debug)]
struct File {
    file_path: Option<String>,
}

/// A file to upload: its name, its type and its bytes.
pub struct Upload<'a> {
    pub filename: &'a str,
    pub kind: &'a str,
    pub bytes: &'a [u8],
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

    /// The URL holds the token, which an error must not show.
    fn hide(&self, method: &str, error: ureq::Error) -> anyhow::Error {
        let error = error.to_string().replace(&self.token, "<token>");
        anyhow::anyhow!("{method} failed: {error}")
    }

    fn call<T: DeserializeOwned>(&self, method: &str, body: Value) -> Result<T> {
        let url = format!("{}/bot{}/{method}", self.api, self.token);
        let response = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|error| self.hide(method, error))?;
        Self::result(response)
    }

    fn result<T: DeserializeOwned>(mut response: ureq::http::Response<ureq::Body>) -> Result<T> {
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

    /// The bytes of a file the bot received, by its id.
    pub fn download(&self, file_id: &str) -> Result<Vec<u8>> {
        let file: File = self.call("getFile", json!({ "file_id": file_id }))?;
        let path = file
            .file_path
            .ok_or_else(|| anyhow::anyhow!("Telegram gave no path for the file"))?;
        let url = format!("{}/file/bot{}/{path}", self.api, self.token);
        let mut response = self
            .agent
            .get(&url)
            .call()
            .map_err(|error| self.hide("download", error))?;
        if response.status() != 200 {
            anyhow::bail!("downloading a file failed ({})", response.status());
        }
        Ok(response
            .body_mut()
            .with_config()
            .limit(MAX_FILE)
            .read_to_vec()?)
    }

    /// Sends a file with `method`, such as `sendPhoto`, as its `field`, such as `photo`,
    /// with the other fields of `body`, given as JSON values, to `chat`.
    pub fn send_file(
        &self,
        chat: i64,
        method: &str,
        field: &str,
        file: &Upload,
        body: Map<String, Value>,
    ) -> Result<Message> {
        let boundary = format!("messagent-{:016x}", rand_boundary());
        let mut form = Vec::new();
        let mut part = |name: &str, value: &str| {
            form.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        part("chat_id", &chat.to_string());
        for (name, value) in &body {
            if name == "chat_id" || name == field {
                continue;
            }
            // A form takes text: a string as it is, anything else as JSON.
            match value {
                Value::String(text) => part(name, text),
                value => part(name, &value.to_string()),
            }
        }
        let filename = file.filename.replace(['"', '\r', '\n'], "_");
        form.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
                file.kind
            )
            .as_bytes(),
        );
        form.extend_from_slice(file.bytes);
        form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = format!("{}/bot{}/{method}", self.api, self.token);
        let response = self
            .agent
            .post(&url)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&form[..])
            .map_err(|error| self.hide(method, error))?;
        Self::result(response)
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
