//! Telegram's Bot API, the part messagent uses: `https://core.telegram.org/bots/api`.

use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Telegram's own Bot API.
pub const API: &str = "https://api.telegram.org";

/// The most UTF-16 code units of a text message.
pub const TEXT_LIMIT: usize = 4096;
/// The largest file a bot downloads.
pub const DOWNLOAD_LIMIT: u64 = 20 * 1024 * 1024;

/// Why a call failed.
#[derive(Debug)]
pub enum Error {
  /// Telegram refused it, with its code and description.
  Refused { code: i64, description: String },
  /// Telegram was not reached, or its answer not read.
  Network(String),
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Error::Refused { code, description } => write!(f, "Telegram refused ({code}): {description}"),
      Error::Network(e) => write!(f, "Telegram not reached: {e}"),
    }
  }
}

impl Error {
  /// Whether asking again may succeed.
  pub fn transient(&self) -> bool {
    match self {
      Error::Network(_) => true,
      Error::Refused { code, .. } => *code == 429 || *code >= 500,
    }
  }
}

impl From<reqwest::Error> for Error {
  fn from(e: reqwest::Error) -> Self {
    Error::Network(e.without_url().to_string())
  }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize, Debug, Clone)]
pub struct Update {
  pub update_id: i64,
  pub message: Option<Message>,
  pub edited_message: Option<Message>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct User {
  pub id: i64,
  #[serde(default)]
  pub is_bot: bool,
  pub username: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Chat {
  pub id: i64,
  #[serde(rename = "type")]
  pub kind: String,
}

impl Chat {
  pub fn private(&self) -> bool {
    self.kind == "private"
  }
}

#[derive(Deserialize, Debug, Clone)]
pub struct PhotoSize {
  pub file_id: String,
  pub file_size: Option<u64>,
}

/// A file of a message: a document, a voice note, an audio, a video.
#[derive(Deserialize, Debug, Clone)]
pub struct Document {
  pub file_id: String,
  pub file_name: Option<String>,
  pub file_size: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Message {
  pub message_id: i64,
  pub chat: Chat,
  pub from: Option<User>,
  /// Who sent it on behalf of a chat, such as an anonymous admin: then `from` is no person.
  pub sender_chat: Option<Chat>,
  pub text: Option<String>,
  pub caption: Option<String>,
  #[serde(default)]
  pub photo: Vec<PhotoSize>,
  pub document: Option<Document>,
  pub voice: Option<Document>,
  pub audio: Option<Document>,
  pub video: Option<Document>,
  pub forward_origin: Option<Value>,
  /// The message it replies to.
  pub reply_to_message: Option<Box<Message>>,
}

#[derive(Deserialize)]
struct Answer {
  ok: bool,
  result: Option<Value>,
  error_code: Option<i64>,
  description: Option<String>,
  parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
  retry_after: Option<u64>,
}

/// A file to upload.
pub struct Upload {
  pub bytes: Vec<u8>,
  pub filename: String,
  pub mime: String,
}

/// The bot, by its token.
#[derive(Clone)]
pub struct Bot {
  http: reqwest::Client,
  api: String,
  token: String,
}

impl Bot {
  pub fn new(api: &str, token: &str) -> Self {
    let http = reqwest::Client::builder()
      .timeout(Duration::from_secs(90))
      .build()
      .expect("a client builds");
    Self {
      http,
      api: api.trim_end_matches('/').to_string(),
      token: token.to_string(),
    }
  }

  fn url(&self, method: &str) -> String {
    format!("{}/bot{}/{method}", self.api, self.token)
  }

  /// One call, waiting out Telegram's flood control where it asks to.
  async fn answer(&self, request: impl Fn() -> reqwest::RequestBuilder) -> Result<Value> {
    loop {
      let answer: Answer = request().send().await?.json().await?;
      if answer.ok {
        return Ok(answer.result.unwrap_or(Value::Null));
      }
      if let Some(after) = answer.parameters.and_then(|p| p.retry_after) {
        tokio::time::sleep(Duration::from_secs(after.min(60))).await;
        continue;
      }
      return Err(Error::Refused {
        code: answer.error_code.unwrap_or(0),
        description: answer.description.unwrap_or_default(),
      });
    }
  }

  /// Calls `method` with a JSON body.
  pub async fn call<T: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<T> {
    let url = self.url(method);
    let value = self.answer(|| self.http.post(&url).json(body)).await?;
    serde_json::from_value(value).map_err(|e| Error::Network(format!("unreadable answer: {e}")))
  }

  /// Calls `method` with a file as `field`, and the rest of `body` as fields.
  async fn upload(&self, method: &str, field: &str, file: &Upload, body: &Value) -> Result<Value> {
    let url = self.url(method);
    self
      .answer(|| {
        let mut form = Form::new();
        if let Value::Object(fields) = body {
          for (key, value) in fields {
            let text = match value {
              Value::String(s) => s.clone(),
              other => other.to_string(),
            };
            form = form.text(key.clone(), text);
          }
        }
        let part = Part::bytes(file.bytes.clone())
          .file_name(file.filename.clone())
          .mime_str(&file.mime)
          .unwrap_or_else(|_| Part::bytes(file.bytes.clone()).file_name(file.filename.clone()));
        self
          .http
          .post(&url)
          .multipart(form.part(field.to_string(), part))
      })
      .await
  }

  pub async fn me(&self) -> Result<User> {
    self.call("getMe", &json!({})).await
  }

  /// The updates from `offset` on, waiting up to `timeout` seconds for one.
  pub async fn updates(&self, offset: i64, timeout: u64) -> Result<Vec<Update>> {
    let allowed = ["message", "edited_message"];
    self
      .call(
        "getUpdates",
        &json!({"offset": offset, "timeout": timeout, "allowed_updates": allowed}),
      )
      .await
  }

  /// Sends a text, and gives its message's id.
  pub async fn send(&self, chat: i64, text: &str, html: bool, quiet: bool) -> Result<i64> {
    let mut body = json!({
      "chat_id": chat,
      "text": text,
      "disable_notification": quiet,
      "link_preview_options": {"is_disabled": true},
    });
    if html {
      body["parse_mode"] = json!("HTML");
    }
    self.send_raw("sendMessage", body).await
  }

  /// Calls a method that sends a message, and gives its id.
  pub async fn send_raw(&self, method: &str, body: Value) -> Result<i64> {
    #[derive(Deserialize)]
    struct Sent {
      message_id: i64,
    }
    Ok(self.call::<Sent>(method, &body).await?.message_id)
  }

  /// Sends a file as `field` of `method`, such as `photo` of `sendPhoto`, with the rest of `body`.
  pub async fn send_file(
    &self,
    method: &str,
    field: &str,
    file: &Upload,
    body: Value,
  ) -> Result<i64> {
    let value = self.upload(method, field, file, &body).await?;
    value["message_id"]
      .as_i64()
      .ok_or_else(|| Error::Network("no message id".into()))
  }

  /// The bot's reaction to a message: one emoji, or none where it is empty.
  pub async fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()> {
    let reaction: Vec<Value> = if emoji.is_empty() {
      vec![]
    } else {
      vec![json!({"type": "emoji", "emoji": emoji})]
    };
    self
      .call::<Value>(
        "setMessageReaction",
        &json!({"chat_id": chat, "message_id": message, "reaction": reaction}),
      )
      .await
      .map(drop)
  }

  /// The bytes of a file a message holds.
  pub async fn download(&self, file_id: &str) -> Result<Vec<u8>> {
    #[derive(Deserialize)]
    struct File {
      file_path: Option<String>,
    }
    let file: File = self.call("getFile", &json!({"file_id": file_id})).await?;
    let path = file
      .file_path
      .ok_or_else(|| Error::Network("Telegram gave no file path".into()))?;
    let url = format!("{}/file/bot{}/{path}", self.api, self.token);
    let response = self.http.get(url).send().await?;
    if !response.status().is_success() {
      return Err(Error::Refused {
        code: response.status().as_u16().into(),
        description: "the file didn't download".into(),
      });
    }
    Ok(response.bytes().await?.to_vec())
  }
}

/// One line of at most `most` characters, for a label anyone in Telegram sets.
pub fn label(text: &str, most: usize) -> String {
  let line: String = text
    .chars()
    .map(|c| if c.is_control() { ' ' } else { c })
    .collect();
  let line = line.trim();
  match line.char_indices().nth(most) {
    Some((cut, _)) => format!("{}…", &line[..cut]),
    None => line.to_string(),
  }
}

/// Where a forwarded message came from, as one line.
pub fn origin(forward: &Value) -> String {
  let name = match forward["type"].as_str() {
    Some("user") => {
      let user = &forward["sender_user"];
      let first = user["first_name"].as_str().unwrap_or_default();
      match user["last_name"].as_str() {
        Some(last) => format!("{first} {last}"),
        None => first.to_string(),
      }
    }
    Some("hidden_user") => forward["sender_user_name"]
      .as_str()
      .unwrap_or_default()
      .into(),
    Some("chat") => forward["sender_chat"]["title"]
      .as_str()
      .unwrap_or_default()
      .into(),
    Some("channel") => forward["chat"]["title"].as_str().unwrap_or_default().into(),
    _ => String::new(),
  };
  match label(&name, 60) {
    name if name.is_empty() => "someone".into(),
    name => name,
  }
}

/// Splits a text into pieces of at most `limit` UTF-16 code units, at the last line break in the
/// second half of each where there is one.
pub fn split(text: &str, limit: usize) -> Vec<String> {
  let mut pieces = Vec::new();
  let mut rest = text;
  while !rest.is_empty() {
    let mut length = 0;
    let mut cut = rest.len();
    let mut line_break = None;
    for (index, c) in rest.char_indices() {
      length += c.len_utf16();
      if length > limit {
        cut = index;
        break;
      }
      if c == '\n' && length > limit / 2 {
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
    pieces.push(rest[..cut].to_string());
    rest = &rest[next..];
  }
  pieces
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn splits_at_line_breaks() {
    let text = format!("{}\n{}", "a".repeat(30), "b".repeat(30));
    assert_eq!(split(&text, 40), vec!["a".repeat(30), "b".repeat(30)]);
    assert_eq!(split("abc", 2), vec!["ab", "c"]);
    assert_eq!(split("", 2), Vec::<String>::new());
  }

  #[test]
  fn labels_are_one_short_line() {
    assert_eq!(label(" a\nb ", 10), "a b");
    assert_eq!(label("abcdef", 3), "abc…");
  }
}
