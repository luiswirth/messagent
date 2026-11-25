//! messagent's Telegram channel: one bot for several agents, each in a topic of its own in each
//! person's private chat with the bot, and linked to the agent's inbox and outbox, its MindIO,
//! like a subscriber. What a person writes in an agent's topic is delivered into that agent's
//! inbox, as an event; what the agent writes into its outbox for `telegram:<name>` goes to its topic
//! in that person's chat.
//!
//! It fetches Telegram's updates by long polling, so it listens on nothing, and watches each
//! outbox's and inbox's `new/` by filesystem notifications. Its state directory holds the offset
//! of the updates, per bot (`offset-<bot id>`), each agent's topic in each chat
//! (`topics.json`, by `<chat>/<agent>`), and the inbox items not yet taken in, each with its
//! message, to mark it 👀 once the agent takes it (`unseen.json`, by `<agent>/<item>`).

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
use mindio::{Kind, MindIo, Outgoing};
use notify::{EventKind, Watcher, event::ModifyKind};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
  markdown,
  telegram::{self, Bot, Message, To, Upload, User},
};

/// The prefix of the names this channel serves, both ways: the person `luis` writes as
/// `telegram:luis`, and an outbox item for `telegram:luis` reaches them. No Vibe Together name
/// holds a `:`, so no participant there passes for one of these people.
pub const CHANNEL: &str = "telegram";
/// The name messagent's own items in the inbox come from, such as an outbox item it couldn't send.
pub const SELF: &str = "messagent";
/// The most messages one outbox item becomes; the rest is cut.
const MAX_PIECES: usize = 20;
/// The longest piece of markdown, leaving room for its tags as HTML.
const PIECE: usize = 3000;

/// An agent the bot serves: its name, which is its topic's, and its IO.
pub struct Agent {
  pub name: String,
  pub io: MindIo,
}

/// What the channel runs with.
pub struct Settings {
  pub bot: Bot,
  pub agents: Vec<Agent>,
  /// The people who may write to the bot, by Telegram id, each with their name.
  pub people: Vec<(i64, String)>,
  pub state: PathBuf,
}

struct Channel {
  bot: Bot,
  agents: Vec<Agent>,
  people: Vec<(i64, String)>,
  state: PathBuf,
  /// Each agent's topic in each chat, by `<chat>/<agent>`.
  topics: Mutex<BTreeMap<String, i64>>,
  /// The inbox items not yet taken in, by `<agent>/<item>`, each with its chat and message.
  unseen: Mutex<BTreeMap<String, (i64, i64)>>,
}

/// Reads a JSON state file, empty where there is none yet.
fn read_state<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
  match fs::read(path) {
    Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("{}", path.display())),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
    Err(e) => Err(e.into()),
  }
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

  fn save(&self, file: &str, value: &impl serde::Serialize) {
    let saved = serde_json::to_vec(value)
      .map_err(anyhow::Error::from)
      .and_then(|bytes| write_private(&self.state.join(file), &bytes));
    if let Err(e) = saved {
      eprintln!("messagent: can't save {file}: {e:#}");
    }
  }

  /// The agent whose topic in `chat` is `topic`.
  fn agent_of(&self, chat: i64, topic: Option<i64>) -> Option<usize> {
    let topic = topic?;
    let topics = self.topics.lock().expect("no holder panics");
    self
      .agents
      .iter()
      .position(|a| topics.get(&format!("{chat}/{}", a.name)) == Some(&topic))
  }

  /// The agent's topic in `chat`, created where it has none yet.
  async fn topic(&self, chat: i64, agent: usize) -> Result<i64> {
    let key = format!("{chat}/{}", self.agents[agent].name);
    if let Some(topic) = self.topics.lock().expect("no holder panics").get(&key) {
      return Ok(*topic);
    }
    let topic = self
      .bot
      .create_topic(chat, &self.agents[agent].name)
      .await
      .with_context(|| format!("can't create the topic {}", self.agents[agent].name))?;
    let mut topics = self.topics.lock().expect("no holder panics");
    topics.insert(key, topic);
    self.save("topics.json", &*topics);
    Ok(topic)
  }

  /// Forgets the agent's topic in `chat`, once it is gone, so that the next message creates it anew.
  fn forget_topic(&self, chat: i64, agent: usize) {
    let mut topics = self.topics.lock().expect("no holder panics");
    topics.remove(&format!("{chat}/{}", self.agents[agent].name));
    self.save("topics.json", &*topics);
  }

  /// The agents' names, as a person reads them.
  fn names(&self) -> String {
    let names: Vec<&str> = self.agents.iter().map(|a| a.name.as_str()).collect();
    names.join(", ")
  }

  async fn tell(&self, to: To, text: &str) {
    if let Err(e) = self.bot.send(to, text, false, true).await {
      eprintln!("messagent: can't tell chat {}: {e}", to.chat);
    }
  }

  /// Tells an agent something of messagent's own, through its inbox, as an event.
  fn notice(&self, agent: usize, text: &str) {
    let delivered = self.agents[agent].io.deliver(
      format!("messagent: {text}\n").as_bytes(),
      SELF,
      "txt",
      Kind::Event,
      &[],
    );
    if let Err(e) = delivered {
      eprintln!("messagent: can't tell the agent: {e}");
    }
  }

  /// Takes in one message from Telegram: one of the people's, in an agent's topic of their private
  /// chat with the bot.
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
    let here = To {
      chat,
      topic: message.message_thread_id,
    };
    let start = !edited
      && message
        .text
        .as_deref()
        .is_some_and(|t| t.split_whitespace().next() == Some("/start"));
    if start {
      // Each agent's topic, created where it is missing, such as on the first start.
      for agent in 0..self.agents.len() {
        if let Err(e) = self.topic(chat, agent).await {
          eprintln!("messagent: {e:#}");
        }
      }
    }
    let Some(agent) = self.agent_of(chat, message.message_thread_id) else {
      let text = format!(
        "Each agent has a topic of its own here: {}. What you write in one reaches that agent.",
        self.names()
      );
      return self.tell(here, &text).await;
    };
    if start {
      return self
        .tell(
          here,
          "What you write here reaches the agent's inbox, and its answers come back here.",
        )
        .await;
    }
    if let Err(e) = self.carry(agent, message, from, &name, edited).await {
      eprintln!("messagent: a message didn't reach the inbox: {e:#}");
      self
        .tell(here, &format!("That didn't reach the agent: {e}"))
        .await;
    }
  }

  /// Delivers a message into the inbox as its sender's: its file first where it has one, then a
  /// text that says where it is from, what it replies to, and where an answer goes.
  async fn carry(
    &self,
    agent: usize,
    message: &Message,
    from: &User,
    name: &str,
    edited: bool,
  ) -> Result<()> {
    let io = &self.agents[agent].io;
    let who = format!("{CHANNEL}:{name}");
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
      // Quiet: the text after it is the event, and wakes for both.
      let file = file_name(&io.inbox().deliver(&bytes[..], &who, &ext)?);
      lines.push(format!("[{kind}: the inbox item {file}]"));
      delivered.push(file);
    }
    if let Some(text) = message.text.as_deref().or(message.caption.as_deref()) {
      lines.push(text.to_string());
    }
    lines.push(format!(
      "(An answer is an item for {who} in the outbox {}.)",
      io.outbox().root.display()
    ));
    let text = lines.join("\n");
    let file = io.deliver(text.as_bytes(), &who, "txt", Kind::Event, &[])?;
    delivered.push(file_name(&file));
    let mut unseen = self.unseen.lock().expect("no holder panics");
    for file in delivered {
      unseen.insert(
        format!("{}/{file}", self.agents[agent].name),
        (message.chat.id, message.message_id),
      );
    }
    self.save("unseen.json", &*unseen);
    Ok(())
  }

  /// Whether an item, by `<agent>/<item>`, is still in its inbox's `new/`; one of an agent no
  /// longer served counts as taken.
  fn is_new(&self, key: &str) -> bool {
    key.split_once('/').is_some_and(|(name, file)| {
      self
        .agents
        .iter()
        .any(|a| a.name == name && a.io.inbox().new_dir().join(file).exists())
    })
  }

  /// Marks 👀 the messages whose items the agent has taken in.
  async fn seen(&self) {
    let taken: Vec<(String, (i64, i64))> = {
      let unseen = self.unseen.lock().expect("no holder panics");
      unseen
        .iter()
        .filter(|(key, _)| !self.is_new(key))
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
    for (key, _) in taken {
      unseen.remove(&key);
    }
    self.save("unseen.json", &*unseen);
  }

  /// Sends every item of an agent's outbox for this channel to its person, in the agent's topic.
  async fn send_all(&self, agent: usize) {
    let prefix = format!("{CHANNEL}:");
    let items = match self.agents[agent]
      .io
      .outbox()
      .take(|who| who.starts_with(&prefix))
    {
      Ok(items) => items,
      Err(e) => return eprintln!("messagent: can't take from the outbox: {e}"),
    };
    for item in items {
      if let Err(e) = self.send(agent, &item).await {
        let file = file_name(&item.path);
        eprintln!("messagent: {file} wasn't sent: {e:#}");
        self.notice(
          agent,
          &format!(
            "the outbox item {file} for {} wasn't sent: {e}",
            item.recipient
          ),
        );
      }
    }
  }

  /// Sends one outbox item by its type: markdown and text as text, a JPEG as a photo, `json` as
  /// Telegram's own `sendMessage`, and anything else as a file. A topic found gone is created anew,
  /// once.
  async fn send(&self, agent: usize, item: &Outgoing) -> Result<()> {
    let name = &item.recipient[CHANNEL.len() + 1..];
    let chat = self
      .chat_of(name)
      .ok_or_else(|| anyhow!("{} is no one this bot knows", item.recipient))?;
    let bytes = fs::read(&item.path)?;
    let ext = item.ext.to_ascii_lowercase();
    let mut delay = Duration::from_secs(1);
    let mut recreated = false;
    loop {
      let chat = To {
        chat,
        topic: Some(self.topic(chat, agent).await?),
      };
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
            .send_file(method, field, &upload, chat.body())
            .await
            .map(drop)
            .map_err(anyhow::Error::from)
        }
      };
      match sent {
        Ok(()) => return Ok(()),
        Err(e)
          if !recreated
            && e
              .downcast_ref::<telegram::Error>()
              .is_some_and(telegram::Error::topic_gone) =>
        {
          self.forget_topic(chat.chat, agent);
          recreated = true;
        }
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
  async fn text(&self, chat: To, text: &str, markdown: bool) -> Result<()> {
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
  async fn native(&self, chat: To, body: &[u8]) -> Result<()> {
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
    if let Value::Object(to) = chat.body() {
      message.extend(to);
    }
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
  // Only an item arriving or leaving counts: reading the directory, as each scan does, is an
  // event too on Linux, and would wake the next scan for good.
  let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
    let moved = event.is_ok_and(|e| {
      matches!(
        e.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
      )
    });
    if moved {
      let _ = wake.send(());
    }
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

/// An item's file name, by which `unseen.json` keeps it.
fn file_name(path: &Path) -> String {
  path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default()
}

/// Runs the channel for good.
pub async fn serve(settings: Settings) -> Result<()> {
  fs::create_dir_all(&settings.state)?;
  fs::set_permissions(&settings.state, fs::Permissions::from_mode(0o700))?;
  for agent in &settings.agents {
    agent.io.create()?;
  }
  let topics = read_state(&settings.state.join("topics.json"))?;
  let mut unseen: BTreeMap<String, (i64, i64)> = read_state(&settings.state.join("unseen.json"))?;
  // An item kept before the agents had topics names no agent: it is let go.
  unseen.retain(|key, _| key.contains('/'));
  let me = settings.bot.me().await.context("the bot's token")?;
  let channel = Arc::new(Channel {
    bot: settings.bot,
    agents: settings.agents,
    people: settings.people,
    state: settings.state,
    topics: Mutex::new(topics),
    unseen: Mutex::new(unseen),
  });
  eprintln!(
    "messagent: serving {} as @{}",
    channel.names(),
    me.username.as_deref().unwrap_or("?")
  );
  // Each agent's topic in each person's chat, created where it is missing.
  for (chat, _) in &channel.people {
    for agent in 0..channel.agents.len() {
      if let Err(e) = channel.topic(*chat, agent).await {
        eprintln!("messagent: {e:#}");
      }
    }
  }
  let mut watchers = Vec::new();
  for agent in 0..channel.agents.len() {
    let sender = channel.clone();
    watchers.push(on_change(
      &channel.agents[agent].io.outbox().new_dir(),
      move || {
        let sender = sender.clone();
        async move { sender.send_all(agent).await }
      },
    )?);
    let seer = channel.clone();
    watchers.push(on_change(
      &channel.agents[agent].io.inbox().new_dir(),
      move || {
        let seer = seer.clone();
        async move { seer.seen().await }
      },
    )?);
  }

  // Telegram numbers the updates of each bot apart, so the offset is the bot's own.
  let offset_file = channel.state.join(format!("offset-{}", me.id));
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
