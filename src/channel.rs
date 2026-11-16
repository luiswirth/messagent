//! messagent's Telegram channel: one bot for one agent, linked to its inbox and its outbox like a
//! subscriber. What the agent's person writes to the bot is delivered into the inbox, proposing a
//! ring; what the agent writes into its outbox for `telegram/<name>` goes to that person's chat.
//!
//! It fetches Telegram's updates by long polling, so it listens on nothing, and watches the
//! outbox's and the inbox's `new/` by filesystem notifications. Its state directory holds the
//! offset of the updates (`offset`) and the inbox items not yet taken in, each with its message,
//! to mark it 👀 once the agent takes it (`unseen.json`).

use std::{
  collections::BTreeMap,
  fs,
  io::Write,
  os::unix::fs::{OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
  time::Duration,
};

use anyhow::{Context, Result, anyhow};
use notify::Watcher;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
  maildir::{Maildir, Name},
  markdown,
  telegram::{self, Bot, Message, Upload, User},
};

/// The prefix of the names this channel serves, both ways: the person `luis` writes as
/// `telegram/luis`, and an outbox item for `telegram/luis` reaches them.
pub const CHANNEL: &str = "telegram";
/// The name messagent's own items in the inbox come from, such as an outbox item it couldn't send.
pub const SELF: &str = "messagent";
/// The most messages one outbox item becomes; the rest is cut.
const MAX_PIECES: usize = 20;
/// The longest piece of markdown, leaving room for its tags as HTML.
const PIECE: usize = 3000;

/// What the channel runs with.
pub struct Settings {
  pub bot: Bot,
  pub inbox: Maildir,
  pub outbox: Maildir,
  /// The people who may write to the bot, by Telegram id, each with their name.
  pub people: Vec<(i64, String)>,
  pub state: PathBuf,
}

struct Channel {
  bot: Bot,
  inbox: Maildir,
  outbox: Maildir,
  people: Vec<(i64, String)>,
  state: PathBuf,
  /// The inbox items not yet taken in, each with its chat and message.
  unseen: Mutex<BTreeMap<String, (i64, i64)>>,
}

/// Writes a file readable by its owner alone, whole or not at all.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
  let temporary = path.with_extension("new");
  let mut file = fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .mode(0o600)
    .open(&temporary)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  fs::rename(&temporary, path)?;
  Ok(())
}

impl Channel {
  fn name_of(&self, user: i64) -> Option<&str> {
    self
      .people
      .iter()
      .find(|(id, _)| *id == user)
      .map(|(_, n)| n.as_str())
  }

  fn chat_of(&self, name: &str) -> Option<i64> {
    self
      .people
      .iter()
      .find(|(_, n)| n == name)
      .map(|(id, _)| *id)
  }

  fn save_unseen(&self, unseen: &BTreeMap<String, (i64, i64)>) {
    let saved = serde_json::to_vec(unseen)
      .map_err(anyhow::Error::from)
      .and_then(|bytes| write_private(&self.state.join("unseen.json"), &bytes));
    if let Err(e) = saved {
      eprintln!("messagent: can't save what is unseen: {e:#}");
    }
  }

  async fn tell(&self, chat: i64, text: &str) {
    if let Err(e) = self.bot.send(chat, text, false, true).await {
      eprintln!("messagent: can't tell chat {chat}: {e}");
    }
  }

  /// Tells the agent something of messagent's own, through its inbox, proposing a ring.
  fn notice(&self, text: &str) {
    let delivered = self
      .inbox
      .deliver(format!("messagent: {text}\n").as_bytes(), SELF, "txt")
      .and_then(|_| {
        if self.inbox.rings(&[("from", SELF)], true)? {
          self.inbox.ring()?;
        }
        Ok(())
      });
    if let Err(e) = delivered {
      eprintln!("messagent: can't tell the agent: {e}");
    }
  }

  /// Takes in one message from Telegram: one of the people's, in their private chat with the bot.
  async fn message(&self, message: &Message, edited: bool) {
    let Some(from) = message.from.as_ref().filter(|f| !f.is_bot) else {
      return;
    };
    // Only the people of the config reach the agent, in their private chat: the rest is ignored.
    let Some(name) = self.name_of(from.id).map(str::to_string) else {
      return;
    };
    if !message.chat.private() || message.sender_chat.is_some() {
      return;
    }
    let chat = message.chat.id;
    if !edited
      && let Some(text) = &message.text
      && text.split_whitespace().next() == Some("/start")
    {
      return self
        .tell(
          chat,
          "What you write here reaches the agent's inbox, and its answers come back here.",
        )
        .await;
    }
    if let Err(e) = self.carry(message, from, &name, edited).await {
      eprintln!("messagent: a message didn't reach the inbox: {e:#}");
      self
        .tell(chat, &format!("That didn't reach the agent: {e}"))
        .await;
    }
  }

  /// Delivers a message into the inbox as its sender's: its file first where it has one, then a
  /// text that says where it is from, what it replies to, and where an answer goes.
  async fn carry(&self, message: &Message, from: &User, name: &str, edited: bool) -> Result<()> {
    let who = format!("{CHANNEL}/{name}");
    let mut header = format!("Telegram message from {who}");
    if let Some(username) = &from.username {
      header += &format!(" (@{})", telegram::label(username, 40));
    }
    if edited {
      header += ", edited";
    }
    if let Some(forward) = &message.forward_origin {
      header += &format!(", forwarded from {}", telegram::origin(forward));
    }
    if let Some(replied) = &message.reply_to_message {
      let quoted = replied
        .text
        .as_deref()
        .or(replied.caption.as_deref())
        .unwrap_or("(no text)");
      header += &format!(", replying to: \"{}\"", telegram::label(quoted, 300));
    }
    let mut lines = vec![format!("{header}:")];
    let mut delivered = Vec::new();
    if let Some((id, size, ext, kind)) = file_of(message) {
      if size.unwrap_or(0) > telegram::DOWNLOAD_LIMIT {
        return Err(anyhow!("the file is too large"));
      }
      let bytes = self.bot.download(&id).await?;
      let file = self.inbox.deliver(&bytes, &who, &ext)?;
      lines.push(format!("[{kind}: the inbox item {file}]"));
      delivered.push(file);
    }
    if let Some(text) = message.text.as_deref().or(message.caption.as_deref()) {
      lines.push(text.to_string());
    }
    lines.push(format!(
      "(An answer is an item for {who} in the outbox {}.)",
      self.outbox.root.display()
    ));
    let file = self
      .inbox
      .deliver(lines.join("\n").as_bytes(), &who, "txt")?;
    delivered.push(file);
    if self.inbox.rings(&[("from", &who)], true)? {
      self.inbox.ring()?;
    }
    let mut unseen = self.unseen.lock().expect("no holder panics");
    for file in delivered {
      unseen.insert(file, (message.chat.id, message.message_id));
    }
    self.save_unseen(&unseen);
    Ok(())
  }

  /// Marks 👀 the messages whose items the agent has taken in.
  async fn seen(&self) {
    let taken: Vec<(String, (i64, i64))> = {
      let unseen = self.unseen.lock().expect("no holder panics");
      unseen
        .iter()
        .filter(|(file, _)| !self.inbox.is_new(file))
        .map(|(file, at)| (file.clone(), *at))
        .collect()
    };
    if taken.is_empty() {
      return;
    }
    let mut marked = Vec::new();
    for (_, at) in &taken {
      if !marked.contains(at) {
        if let Err(e) = self.bot.react(at.0, at.1, "👀").await {
          eprintln!("messagent: can't mark a message seen: {e}");
        }
        marked.push(*at);
      }
    }
    let mut unseen = self.unseen.lock().expect("no holder panics");
    for (file, _) in taken {
      unseen.remove(&file);
    }
    self.save_unseen(&unseen);
  }

  /// Sends every outbox item for this channel to its person.
  async fn send_all(&self) {
    let prefix = format!("{CHANNEL}/");
    let items = match self.outbox.untaken(|who| who.starts_with(&prefix)) {
      Ok(items) => items,
      Err(e) => return eprintln!("messagent: can't read the outbox: {e}"),
    };
    for item in items {
      let path = match self.outbox.take(&item) {
        Ok(Some(path)) => path,
        Ok(None) => continue,
        Err(e) => {
          eprintln!("messagent: can't take {}: {e}", item.file);
          continue;
        }
      };
      if let Err(e) = self.send(&item, &path).await {
        eprintln!("messagent: {} wasn't sent: {e:#}", item.file);
        self.notice(&format!(
          "the outbox item {} for {} wasn't sent: {e}",
          item.file, item.who
        ));
      }
    }
  }

  /// Sends one outbox item by its type: markdown and text as text, a JPEG as a photo, `json` as
  /// Telegram's own `sendMessage`, and anything else as a file.
  async fn send(&self, item: &Name, path: &Path) -> Result<()> {
    let name = &item.who[CHANNEL.len() + 1..];
    let chat = self
      .chat_of(name)
      .ok_or_else(|| anyhow!("{} is no one this bot knows", item.who))?;
    let bytes = fs::read(path)?;
    let ext = item.ext.to_ascii_lowercase();
    let mut delay = Duration::from_secs(1);
    loop {
      let sent = match ext.as_str() {
        "md" | "txt" => {
          self
            .text(chat, &String::from_utf8_lossy(&bytes), ext == "md")
            .await
        }
        "json" => self.native(chat, &bytes).await,
        _ => {
          let photo = matches!(ext.as_str(), "jpg" | "jpeg");
          let upload = Upload {
            filename: format!("file.{ext}"),
            mime: mime_of(&ext).to_string(),
            bytes: bytes.clone(),
          };
          let (method, field) = if photo {
            ("sendPhoto", "photo")
          } else {
            ("sendDocument", "document")
          };
          self
            .bot
            .send_file(method, field, &upload, json!({"chat_id": chat}))
            .await
            .map(drop)
            .map_err(anyhow::Error::from)
        }
      };
      match sent {
        Ok(()) => return Ok(()),
        Err(e)
          if e
            .downcast_ref::<telegram::Error>()
            .is_some_and(telegram::Error::transient)
            && delay <= Duration::from_secs(60) =>
        {
          tokio::time::sleep(delay).await;
          delay *= 2;
        }
        Err(e) => return Err(e),
      }
    }
  }

  /// Sends a text, markdown as Telegram's HTML; formatting Telegram refuses goes as plain text.
  async fn text(&self, chat: i64, text: &str, markdown: bool) -> Result<()> {
    let mut pieces = telegram::split(text, PIECE);
    if pieces.len() > MAX_PIECES {
      pieces.truncate(MAX_PIECES);
      pieces.push("[cut: too long for Telegram]".into());
    }
    for piece in pieces {
      let html = if markdown {
        markdown::to_html(&piece)
      } else {
        markdown::escape(&piece)
      };
      let sent = if html.encode_utf16().count() <= telegram::TEXT_LIMIT {
        self.bot.send(chat, &html, true, false).await
      } else {
        Err(telegram::Error::Refused {
          code: 400,
          description: "too long".into(),
        })
      };
      match sent {
        Ok(_) => {}
        Err(telegram::Error::Refused { code: 400, .. }) => {
          for part in telegram::split(&piece, telegram::TEXT_LIMIT) {
            self.bot.send(chat, &part, false, false).await?;
          }
        }
        Err(e) => return Err(e.into()),
      }
    }
    Ok(())
  }

  /// Sends a message in Telegram's own format, with these fields alone, so that it reaches this
  /// chat and does nothing else.
  async fn native(&self, chat: i64, body: &[u8]) -> Result<()> {
    const FIELDS: &[&str] = &[
      "text",
      "parse_mode",
      "entities",
      "link_preview_options",
      "disable_notification",
    ];
    let given: serde_json::Map<String, Value> =
      serde_json::from_slice(body).context("a Telegram message is a JSON object")?;
    let mut message = serde_json::Map::new();
    for field in FIELDS {
      if let Some(value) = given.get(*field) {
        message.insert(field.to_string(), value.clone());
      }
    }
    if !message.contains_key("text") {
      return Err(anyhow!("a Telegram message has a text"));
    }
    message.insert("chat_id".into(), json!(chat));
    self
      .bot
      .send_raw("sendMessage", Value::Object(message))
      .await?;
    Ok(())
  }
}

/// A message's file, where it has one: its id, size, its item's extension, and what it is.
fn file_of(message: &Message) -> Option<(String, Option<u64>, String, &'static str)> {
  if let Some(photo) = message.photo.last() {
    return Some((
      photo.file_id.clone(),
      photo.file_size,
      "jpg".into(),
      "a photo",
    ));
  }
  for (document, kind, fallback) in [
    (&message.document, "a file", "bin"),
    (&message.voice, "a voice note", "ogg"),
    (&message.audio, "an audio", "mp3"),
    (&message.video, "a video", "mp4"),
  ] {
    if let Some(d) = document {
      let ext = d
        .file_name
        .as_deref()
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| {
          !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| fallback.into());
      return Some((d.file_id.clone(), d.file_size, ext, kind));
    }
  }
  None
}

/// The MIME type of an extension, for a file sent.
fn mime_of(ext: &str) -> &'static str {
  match ext {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "webp" => "image/webp",
    "pdf" => "application/pdf",
    "ogg" => "audio/ogg",
    "mp3" => "audio/mpeg",
    "mp4" => "video/mp4",
    "csv" => "text/csv",
    _ => "application/octet-stream",
  }
}

/// Watches a directory, waking `wake` on each change in it.
fn watch(dir: &Path, wake: mpsc::UnboundedSender<()>) -> Result<notify::RecommendedWatcher> {
  let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
    let _ = wake.send(());
  })?;
  watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
  Ok(watcher)
}

/// Runs `work` once now and again after each change of `dir`, for good.
fn on_change<F, Fut>(dir: &Path, work: F) -> Result<notify::RecommendedWatcher>
where
  F: Fn() -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send,
{
  let (wake, mut woken) = mpsc::unbounded_channel();
  let watcher = watch(dir, wake)?;
  tokio::spawn(async move {
    loop {
      work().await;
      if woken.recv().await.is_none() {
        return;
      }
      while woken.try_recv().is_ok() {}
    }
  });
  Ok(watcher)
}

/// Runs the channel for good.
pub async fn serve(settings: Settings) -> Result<()> {
  fs::create_dir_all(&settings.state)?;
  fs::set_permissions(&settings.state, fs::Permissions::from_mode(0o700))?;
  settings.inbox.create()?;
  settings.outbox.create()?;
  let unseen = match fs::read(settings.state.join("unseen.json")) {
    Ok(bytes) => serde_json::from_slice(&bytes).context("unseen.json")?,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
    Err(e) => return Err(e.into()),
  };
  let me = settings.bot.me().await.context("the bot's token")?;
  let channel = Arc::new(Channel {
    bot: settings.bot,
    inbox: settings.inbox,
    outbox: settings.outbox,
    people: settings.people,
    state: settings.state,
    unseen: Mutex::new(unseen),
  });
  eprintln!(
    "messagent: serving as @{}",
    me.username.as_deref().unwrap_or("?")
  );
  let sender = channel.clone();
  let _outbox = on_change(&channel.outbox.new_dir(), move || {
    let sender = sender.clone();
    async move { sender.send_all().await }
  })?;
  let seer = channel.clone();
  let _inbox = on_change(&channel.inbox.new_dir(), move || {
    let seer = seer.clone();
    async move { seer.seen().await }
  })?;

  let offset_file = channel.state.join("offset");
  let mut offset: i64 = fs::read_to_string(&offset_file)
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0);
  let mut delay = Duration::from_secs(1);
  loop {
    let updates = match channel.bot.updates(offset, 50).await {
      Ok(updates) => {
        delay = Duration::from_secs(1);
        updates
      }
      Err(telegram::Error::Refused { code: 401, .. }) => {
        return Err(anyhow!("the bot's token is no longer valid"));
      }
      Err(e) => {
        eprintln!("messagent: {e}; trying again in {delay:?}");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
        continue;
      }
    };
    for update in updates {
      if update.update_id < offset {
        continue;
      }
      offset = update.update_id + 1;
      if let Some(message) = &update.message {
        channel.message(message, false).await;
      }
      if let Some(message) = &update.edited_message {
        channel.message(message, true).await;
      }
      write_private(&offset_file, offset.to_string().as_bytes())?;
    }
  }
}
