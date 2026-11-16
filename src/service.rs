//! The Telegram bridge: one bot, which mirrors each Telegram chat bridged, private or a group, and
//! one Vibe Together session onto each other.
//!
//! A person links their Telegram account once, by a Vibe Together OTP of a person token, which they
//! approve on the website; the bot keeps the token and speaks as them with it, so Vibe Together
//! knows nothing of Telegram. Once linked, `/start` offers their sessions as buttons, and the one
//! pressed gets the chat. The chat's session is read by whoever bridged it, through the session's
//! log, which marks nothing read for anyone.
//!
//! It fetches Telegram's updates by long polling, so it listens on nothing. Its state directory
//! holds the linked people (`linked.json`), the offset of the updates (`offset`), and each bridged
//! chat's place in its session (`chats/<chat>.json`), all readable by its user alone.

use std::{
  collections::{BTreeMap, BTreeSet, HashMap, HashSet},
  fs,
  io::Write,
  os::unix::fs::{OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
  time::Duration,
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use vibetogether_client::{
  Client, Error as VtError,
  protocol::{Entry, LogQuery, OtpRequest, TokenKind, is_message, names_of},
};

use crate::{
  markdown,
  telegram::{self, Bot, CallbackQuery, Message, Upload, User},
};

/// The type of a message in Telegram's own format: the body of the Bot API's `sendMessage`,
/// `sendPhoto` or `sendDocument`, as `telegram.md` says.
pub const TELEGRAM: &str = "application/vnd.messagent.telegram+json";
/// The type of a reaction to an entry, as `reactions.md` says.
pub const REACTION: &str = "application/vnd.messagent.reaction+json";
/// The name an entry pings alone where it is for the bridge's use only, such as the image of a
/// Telegram message, so that it doesn't reach the chat on its own too.
pub const BRIDGE: &str = "messagent";

/// What Telegram shows of the bot, set as the service starts.
const COMMANDS: &[(&str, &str)] = &[
  ("start", "Link your account, or bridge this chat"),
  ("stop", "End this chat's bridge"),
  ("unlink", "Unlink your Vibe Together account"),
];
const DESCRIPTION: &str = "Talk with the agents of your Vibe Together sessions from Telegram.\n\n\
/start links your Vibe Together account and bridges this chat into a session. In a group, add me and send /start.\n\n\
https://vibetogether.potentialhedge.com";
const SHORT_DESCRIPTION: &str = "Talk with your agents from Telegram, through Vibe Together.";

/// The most people linking at once, each waiting on an OTP.
const MAX_LINKING: usize = 20;
/// The most sessions offered as buttons.
const MAX_CHOICES: usize = 40;
/// The most entries of a chat whose message is kept, for reactions and 👀.
const KEPT: usize = 2000;
/// How long a read of the session waits for something new.
const WAIT: u64 = 300;
/// The most messages one entry becomes in the chat; the rest stays in the session.
const MAX_PIECES: usize = 5;
/// The longest a piece of markdown is before it becomes HTML, leaving room for the tags.
const PIECE: usize = 3000;

/// What the service runs with.
pub struct Settings {
  pub bot: Bot,
  /// Vibe Together, such as `https://vibetogether.potentialhedge.com`.
  pub server: String,
  pub state: PathBuf,
  /// The accounts that may link, by name; anyone where it is empty.
  pub allow: Vec<String>,
  /// The Telegram users the bot serves, by id; anyone where it is empty. Everyone else's
  /// messages and presses are ignored, so no stranger links or asks for OTPs.
  pub telegram: Vec<i64>,
}

/// A Telegram user who linked their account: its name, and the person token of it.
#[derive(Serialize, Deserialize, Clone)]
struct Linked {
  account: String,
  token: String,
  /// The login the token is, which it revokes itself; none for a person token of before apps.
  #[serde(default)]
  login: Option<String>,
}

/// A bridged chat's place in its session, as kept on disk.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Place {
  session: String,
  /// The session's name, as it was bridged.
  name: String,
  /// Who bridged it, by Telegram id, whose token reads the session.
  by: i64,
  private: bool,
  /// The last entry handled.
  after: u64,
  /// The entries that came from this chat, each with its message in Telegram.
  messages: BTreeMap<u64, i64>,
  /// Those of them no agent has read yet.
  unseen: BTreeSet<u64>,
}

/// A bridged chat while the service runs.
struct Bridge {
  chat: i64,
  place: Mutex<Place>,
  /// Held while a message of the chat goes into the session until its entry is kept, so that
  /// reading the session never takes it for someone else's and carries it back.
  carrying: tokio::sync::Mutex<()>,
  listener: Mutex<Option<JoinHandle<()>>>,
}

impl Bridge {
  fn place(&self) -> Place {
    self.place.lock().expect("no holder panics").clone()
  }
}

struct Service {
  bot: Bot,
  username: String,
  server: String,
  state: PathBuf,
  allow: Vec<String>,
  telegram: Vec<i64>,
  linked: Mutex<BTreeMap<i64, Linked>>,
  bridges: Mutex<HashMap<i64, Arc<Bridge>>>,
  linking: Mutex<HashSet<i64>>,
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

fn private_dir(dir: &Path) -> Result<()> {
  fs::create_dir_all(dir)?;
  fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
  Ok(())
}

fn load<T: DeserializeOwned + Default>(file: &Path) -> Result<T> {
  match fs::read(file) {
    Ok(bytes) => {
      serde_json::from_slice(&bytes).with_context(|| format!("can't read {}", file.display()))
    }
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
    Err(e) => Err(e.into()),
  }
}

/// The type without its parameters, in lower case.
fn base(ty: &str) -> String {
  ty.split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase()
}

/// The command a message gives the bot, such as `start` from `/start` or `/start@bot`, where it
/// is for this bot.
fn command<'a>(text: &'a str, username: &str) -> Option<&'a str> {
  let word = text.strip_prefix('/')?.split_whitespace().next()?;
  match word.split_once('@') {
    Some((command, bot)) if bot.eq_ignore_ascii_case(username) => Some(command),
    Some(_) => None,
    None => Some(word),
  }
}

/// Whether an entry is the bridge's alone: one that pings only `messagent`.
fn for_bridge(entry: &Entry) -> bool {
  base(&entry.ty) != TELEGRAM && names_of(&entry.ty, "ping") == [BRIDGE]
}

impl Service {
  fn serves(&self, user: i64) -> bool {
    self.telegram.is_empty() || self.telegram.contains(&user)
  }

  fn client(&self, token: &str) -> Client {
    Client::new(&self.server, Some(token.to_string()))
  }

  fn linked(&self, user: i64) -> Option<Linked> {
    self
      .linked
      .lock()
      .expect("no holder panics")
      .get(&user)
      .cloned()
  }

  fn save_linked(&self, linked: &BTreeMap<i64, Linked>) -> Result<()> {
    write_private(
      &self.state.join("linked.json"),
      &serde_json::to_vec(linked)?,
    )
  }

  fn bridge(&self, chat: i64) -> Option<Arc<Bridge>> {
    self
      .bridges
      .lock()
      .expect("no holder panics")
      .get(&chat)
      .cloned()
  }

  fn chat_file(&self, chat: i64) -> PathBuf {
    self.state.join("chats").join(format!("{chat}.json"))
  }

  fn save(&self, bridge: &Bridge) -> Result<()> {
    let place = bridge.place();
    write_private(&self.chat_file(bridge.chat), &serde_json::to_vec(&place)?)
  }

  fn update(&self, bridge: &Bridge, change: impl FnOnce(&mut Place)) {
    change(&mut bridge.place.lock().expect("no holder panics"));
    if let Err(e) = self.save(bridge) {
      eprintln!("messagent: can't save chat {}: {e:#}", bridge.chat);
    }
  }

  /// Tells a chat something of the bot's own.
  async fn tell(&self, chat: i64, text: &str) {
    if let Err(e) = self.bot.send(chat, text, false, false).await {
      eprintln!("messagent: can't tell chat {chat}: {e}");
    }
  }

  /// Starts carrying a bridged chat.
  fn start(self: &Arc<Self>, chat: i64, place: Place) -> Arc<Bridge> {
    let bridge = Arc::new(Bridge {
      chat,
      place: Mutex::new(place),
      carrying: tokio::sync::Mutex::new(()),
      listener: Mutex::new(None),
    });
    let listener = tokio::spawn(self.clone().listen(bridge.clone()));
    *bridge.listener.lock().expect("no holder panics") = Some(listener);
    if let Some(old) = self
      .bridges
      .lock()
      .expect("no holder panics")
      .insert(chat, bridge.clone())
    {
      Self::halt(&old);
    }
    bridge
  }

  fn halt(bridge: &Bridge) {
    if let Some(listener) = bridge.listener.lock().expect("no holder panics").take() {
      listener.abort();
    }
  }

  /// Ends a chat's bridge; the session stays.
  fn end(&self, chat: i64) {
    let bridge = self.bridges.lock().expect("no holder panics").remove(&chat);
    if let Some(bridge) = bridge {
      Self::halt(&bridge);
    }
    let _ = fs::remove_file(self.chat_file(chat));
  }

  /// Forgets a linked person, whose token no longer holds: the chats they bridged end, each told.
  async fn forget(&self, user: i64) {
    let removed = {
      let mut linked = self.linked.lock().expect("no holder panics");
      let removed = linked.remove(&user);
      if let Err(e) = self.save_linked(&linked) {
        eprintln!("messagent: can't save the linked people: {e:#}");
      }
      removed
    };
    if removed.is_none() {
      return;
    }
    let chats: Vec<i64> = self
      .bridges
      .lock()
      .expect("no holder panics")
      .values()
      .filter(|b| b.place().by == user)
      .map(|b| b.chat)
      .collect();
    for chat in chats {
      self
        .tell(
          chat,
          "This chat's bridge ended: whoever bridged it is no longer linked.",
        )
        .await;
      self.end(chat);
    }
  }

  async fn handle(self: &Arc<Self>, update: telegram::Update) {
    if let Some(message) = update.message {
      self.message(&message, false).await;
    }
    if let Some(message) = update.edited_message {
      self.message(&message, true).await;
    }
    if let Some(query) = update.callback_query {
      self.pressed(&query).await;
    }
    if let Some(member) = update.my_chat_member
      && matches!(member.new_chat_member.status.as_str(), "left" | "kicked")
    {
      self.end(member.chat.id);
    }
  }

  async fn message(self: &Arc<Self>, message: &Message, edited: bool) {
    let chat = message.chat.id;
    if let Some(to) = message.migrate_to_chat_id {
      self.migrate(chat, to);
      return;
    }
    // Only a person speaks: never a bot, a channel or an anonymous admin.
    let Some(from) = message.from.as_ref().filter(|f| !f.is_bot) else {
      return;
    };
    if message.sender_chat.is_some() || !self.serves(from.id) {
      return;
    }
    if let Some(command) = message
      .text
      .as_deref()
      .and_then(|t| command(t, &self.username))
    {
      if edited {
        return;
      }
      match command {
        "start" => return self.begin(message, from).await,
        "stop" => return self.stop(message, from).await,
        "unlink" => return self.unlink(chat, from).await,
        _ => {}
      }
    }
    if let Err(e) = self.carry(message, from, edited).await {
      eprintln!("messagent: a message of chat {chat} didn't reach its session: {e:#}");
      self
        .tell(chat, &format!("That didn't reach the session: {e}"))
        .await;
    }
  }

  /// /start: links the sender's account, in their private chat, or where they linked it offers
  /// their sessions to bridge the chat into.
  async fn begin(self: &Arc<Self>, message: &Message, from: &User) {
    let chat = message.chat.id;
    if self.linked(from.id).is_some() {
      return self.offer(chat, from).await;
    }
    if !message.chat.private() {
      return self
        .tell(
          chat,
          &format!(
            "Link your Vibe Together account first, in your chat with me: https://t.me/{}",
            self.username
          ),
        )
        .await;
    }
    let refusal = {
      let mut linking = self.linking.lock().expect("no holder panics");
      if linking.contains(&from.id) {
        Some("Approve the link I sent you first.")
      } else if linking.len() >= MAX_LINKING {
        Some("Too many are linking right now; try again in a few minutes.")
      } else {
        linking.insert(from.id);
        None
      }
    };
    if let Some(refusal) = refusal {
      return self.tell(chat, refusal).await;
    }
    let service = self.clone();
    let from = from.clone();
    tokio::spawn(async move {
      if let Err(e) = service.link(chat, &from).await {
        eprintln!("messagent: linking {} failed: {e:#}", from.id);
        service
          .tell(
            chat,
            &format!("Linking failed: {e}. Send /start to try again."),
          )
          .await;
      }
      service
        .linking
        .lock()
        .expect("no holder panics")
        .remove(&from.id);
    });
  }

  /// Links `from`'s account: they approve the OTP, signed in, and the bot keeps the token.
  async fn link(self: &Arc<Self>, chat: i64, from: &User) -> Result<()> {
    let anonymous = Client::new(&self.server, None);
    // An app token, by the app's name, which the approval page and every entry it sends show.
    let name = "messagent".to_string();
    let otp = anonymous
      .request_otp(&OtpRequest {
        kind: TokenKind::App,
        name,
      })
      .await?;
    self
      .tell(
        chat,
        &format!(
          "To link your Vibe Together account, open\n{}\nsigned in, and approve only if the page shows the code {}.\n\n\
           It lets me speak as you in the chats you bridge, until you /unlink.",
          otp.link, otp.code
        ),
      )
      .await;
    let token = tokio::time::timeout(
      Duration::from_secs(16 * 60),
      anonymous.redeem_otp(&otp.secret),
    )
    .await
    .map_err(|_| anyhow!("the link expired"))??;
    if token.kind != TokenKind::App {
      return Err(anyhow!("Vibe Together gave no app token"));
    }
    if !self.allow.is_empty() && !self.allow.contains(&token.account) {
      revoke(&self.client(&token.token), Some(&token.login)).await;
      return Err(anyhow!("this bot doesn't serve that account"));
    }
    {
      let mut linked = self.linked.lock().expect("no holder panics");
      let old = linked.insert(
        from.id,
        Linked {
          account: token.account.clone(),
          token: token.token,
          login: Some(token.login),
        },
      );
      self.save_linked(&linked)?;
      if let Some(old) = old {
        let client = self.client(&old.token);
        tokio::spawn(async move { revoke(&client, old.login.as_deref()).await });
      }
    }
    self
      .tell(chat, &format!("Linked as {}.", token.account))
      .await;
    self.offer(chat, from).await;
    Ok(())
  }

  /// Whether `from` may move or end a chat's bridge: in a private chat its person, in a group
  /// whoever bridged it or an admin.
  async fn may_change(&self, chat: i64, bridge: &Bridge, from: &User) -> bool {
    let place = bridge.place();
    if place.private || place.by == from.id {
      return true;
    }
    matches!(
      self.bot.status(chat, from.id).await.as_deref(),
      Ok("creator" | "administrator")
    )
  }

  /// Offers `from`'s sessions as buttons, to bridge the chat into the one pressed.
  async fn offer(&self, chat: i64, from: &User) {
    let Some(linked) = self.linked(from.id) else {
      return;
    };
    if let Some(bridge) = self.bridge(chat)
      && !self.may_change(chat, &bridge, from).await
    {
      return self
        .tell(
          chat,
          "Only whoever bridged this chat, or a group admin, moves it.",
        )
        .await;
    }
    let client = self.client(&linked.token);
    let listed = match client.sessions().await {
      Ok(sessions) => client.dms().await.map(|dms| (sessions, dms)),
      Err(e) => Err(e),
    };
    let (sessions, dms) = match listed {
      Ok(listed) => listed,
      Err(e) if e.status() == Some(401) => {
        self.forget(from.id).await;
        return self
          .tell(
            chat,
            "Your link was revoked in Vibe Together. /start links again.",
          )
          .await;
      }
      Err(e) => {
        return self
          .tell(chat, &format!("Vibe Together refused: {e}"))
          .await;
      }
    };
    let mut choices = Vec::new();
    for session in sessions.iter().chain(&dms) {
      let label = match (&session.group, &session.project, &session.friend) {
        (_, _, Some(friend)) => format!("DM with {friend}"),
        (Some(group), Some(project), _) => {
          format!("{} › {} › {}", group.name, project.name, session.name)
        }
        (Some(group), None, _) => format!("{} › {}", group.name, session.name),
        _ => session.name.clone(),
      };
      if !choices
        .iter()
        .any(|(_, id): &(String, String)| *id == session.id)
      {
        choices.push((label, session.id.clone()));
      }
    }
    let buttons: Vec<(String, String)> = choices
      .into_iter()
      .take(MAX_CHOICES)
      .map(|(label, id)| (telegram::label(&label, 60), format!("b {} {id}", from.id)))
      .filter(|(_, data)| data.len() <= 64)
      .collect();
    if buttons.is_empty() {
      return self
        .tell(chat, "You have no session yet: make one at https://vibetogether.potentialhedge.com, then /start.")
        .await;
    }
    let now = self
      .bridge(chat)
      .map(|b| format!(" It goes into {} now.", b.place().name))
      .unwrap_or_default();
    if let Err(e) = self
      .bot
      .send_buttons(
        chat,
        &format!("Which session should this chat go into?{now}"),
        &buttons,
      )
      .await
    {
      eprintln!("messagent: can't offer sessions in {chat}: {e}");
    }
  }

  /// A button pressed: bridges the chat into the session it names, by the presser's token.
  async fn pressed(self: &Arc<Self>, query: &CallbackQuery) {
    let answer = |text: &'static str| {
      let bot = self.bot.clone();
      let id = query.id.clone();
      async move {
        let _ = bot.answer_callback(&id, text).await;
      }
    };
    let (Some(data), Some(message)) = (query.data.as_deref(), query.message.as_ref()) else {
      return answer("").await;
    };
    if !self.serves(query.from.id) {
      return answer("").await;
    }
    let mut parts = data.split(' ');
    let (Some("b"), Some(user), Some(session)) = (parts.next(), parts.next(), parts.next()) else {
      return answer("").await;
    };
    if user != query.from.id.to_string() {
      return answer("These are someone else's sessions.").await;
    }
    let Some(linked) = self.linked(query.from.id) else {
      return answer("Link your account first: /start").await;
    };
    let chat = message.chat.id;
    if let Some(bridge) = self.bridge(chat)
      && !self.may_change(chat, &bridge, &query.from).await
    {
      return answer("Only whoever bridged this chat, or a group admin, moves it.").await;
    }
    match self
      .bridge_into(
        chat,
        message.chat.private(),
        query.from.id,
        &linked,
        session,
      )
      .await
    {
      Ok(name) => {
        answer("").await;
        let _ = self
          .bot
          .edit(
            chat,
            message.message_id,
            &format!("This chat goes into {name} now. /stop ends it."),
          )
          .await;
      }
      Err(e) => {
        answer("").await;
        self.tell(chat, &format!("Bridging failed: {e}")).await;
      }
    }
  }

  /// Bridges a chat into a session, from its next entry on; the session's name.
  async fn bridge_into(
    self: &Arc<Self>,
    chat: i64,
    private: bool,
    by: i64,
    linked: &Linked,
    session: &str,
  ) -> Result<String> {
    let client = self.client(&linked.token);
    let found = client.session(session).await?;
    client.join(session).await?;
    let mut after = 0;
    loop {
      let log = client
        .log(
          session,
          &LogQuery {
            after: Some(after),
            ..Default::default()
          },
        )
        .await?;
      match log.entries.last() {
        Some(last) => after = last.n,
        None => break,
      }
    }
    let place = Place {
      session: session.to_string(),
      name: found.name.clone(),
      by,
      private,
      after,
      ..Default::default()
    };
    write_private(&self.chat_file(chat), &serde_json::to_vec(&place)?)?;
    self.start(chat, place);
    Ok(found.name)
  }

  /// /stop: ends the chat's bridge.
  async fn stop(&self, message: &Message, from: &User) {
    let chat = message.chat.id;
    let Some(bridge) = self.bridge(chat) else {
      return self.tell(chat, "This chat goes nowhere.").await;
    };
    if !self.may_change(chat, &bridge, from).await {
      return self
        .tell(
          chat,
          "Only whoever bridged this chat, or a group admin, ends it.",
        )
        .await;
    }
    self.end(chat);
    self
      .tell(chat, "This chat's bridge ended; the session stays.")
      .await;
  }

  /// /unlink: revokes the sender's token, and the chats they bridged end.
  async fn unlink(&self, chat: i64, from: &User) {
    let Some(linked) = self.linked(from.id) else {
      return self.tell(chat, "You are not linked.").await;
    };
    let revoked = revoke(&self.client(&linked.token), linked.login.as_deref()).await;
    self.forget(from.id).await;
    let text = if revoked {
      "Unlinked: I no longer speak as you."
    } else {
      "Unlinked here, but Vibe Together didn't revoke the token: revoke messagent in your logins."
    };
    self.tell(chat, text).await;
  }

  /// A group became a supergroup: its bridge moves to the new id.
  fn migrate(self: &Arc<Self>, from: i64, to: i64) {
    let Some(bridge) = self.bridge(from) else {
      return;
    };
    let place = bridge.place();
    self.end(from);
    if let Err(e) = write_private(
      &self.chat_file(to),
      &serde_json::to_vec(&place).unwrap_or_default(),
    ) {
      eprintln!("messagent: can't move chat {from} to {to}: {e:#}");
      return;
    }
    self.start(to, place);
  }

  /// Carries a message into the chat's session, as its sender, where they linked their account.
  async fn carry(&self, message: &Message, from: &User, edited: bool) -> Result<()> {
    let chat = message.chat.id;
    let Some(bridge) = self.bridge(chat) else {
      if message.chat.private() {
        let hint = if self.linked(from.id).is_some() {
          "This chat goes nowhere yet: /start picks a session."
        } else {
          "/start links your Vibe Together account."
        };
        self.tell(chat, hint).await;
      }
      return Ok(());
    };
    // Anyone else in a group, who linked no account, has no voice in the session.
    let Some(linked) = self.linked(from.id) else {
      return Ok(());
    };
    let mut prefix = String::new();
    if let Some(forward) = &message.forward_origin {
      prefix += &format!("[forwarded from {}] ", telegram::origin(forward));
    }
    if edited {
      prefix += "[edited] ";
    }
    let file = file_of(message);
    let text = message
      .text
      .as_deref()
      .or(message.caption.as_deref())
      .map(|text| format!("{prefix}{text}"));
    if file.is_none() && text.is_none() {
      return Ok(());
    }
    let session = bridge.place().session;
    let client = self.client(&linked.token);
    let _carrying = bridge.carrying.lock().await;
    let mut sent = Vec::new();
    if let Some((id, size, ty, filename)) = file {
      if size.unwrap_or(0)
        > telegram::DOWNLOAD_LIMIT.min(vibetogether_client::protocol::MAX_PAYLOAD as u64)
      {
        return Err(anyhow!("the file is too large"));
      }
      let bytes = self.bot.download(&id).await?;
      sent.push(
        self
          .send(&client, from.id, &session, &ty, bytes, filename.as_deref())
          .await?,
      );
    }
    if let Some(text) = text {
      sent.push(
        self
          .send(
            &client,
            from.id,
            &session,
            "text/plain; charset=utf-8",
            text.into_bytes(),
            None,
          )
          .await?,
      );
    }
    self.update(&bridge, |place| {
      for n in sent {
        place.messages.insert(n, message.message_id);
        place.unseen.insert(n);
      }
      while place.messages.len() > KEPT {
        place.messages.pop_first();
      }
    });
    Ok(())
  }

  /// Sends an entry as the person, joining the session again where they left it.
  async fn send(
    &self,
    client: &Client,
    user: i64,
    session: &str,
    ty: &str,
    bytes: Vec<u8>,
    filename: Option<&str>,
  ) -> Result<u64> {
    let mut joined = false;
    let mut delay = Duration::from_secs(1);
    loop {
      match client
        .send_entry(session, ty, bytes.clone(), filename)
        .await
      {
        Ok(sent) => return Ok(sent.n),
        Err(e) if e.status() == Some(403) && !joined => {
          client.join(session).await?;
          joined = true;
        }
        Err(e) if e.status() == Some(401) => {
          self.forget(user).await;
          return Err(anyhow!("your link was revoked in Vibe Together"));
        }
        Err(e) if e.transient() && delay <= Duration::from_secs(16) => {
          tokio::time::sleep(delay).await;
          delay *= 2;
        }
        Err(e) => return Err(e.into()),
      }
    }
  }

  /// Reads the chat's session for good, by the token of whoever bridged it, and carries what
  /// comes out to the chat.
  async fn listen(self: Arc<Self>, bridge: Arc<Bridge>) {
    let chat = bridge.chat;
    let mut revision: Option<String> = None;
    let mut delay = Duration::from_secs(1);
    loop {
      let place = bridge.place();
      // Whoever bridged it is linking again: the chat waits. Unlinking ends it apart.
      let Some(linked) = self.linked(place.by) else {
        tokio::time::sleep(Duration::from_secs(10)).await;
        continue;
      };
      let client = self.client(&linked.token);
      let query = LogQuery {
        after: Some(place.after),
        wait: revision.is_some().then_some(WAIT),
        revision: revision.clone(),
      };
      match client.log(&place.session, &query).await {
        Ok(log) => {
          delay = Duration::from_secs(1);
          revision = Some(log.revision.clone());
          self.take(&bridge, &client, &linked, log).await;
        }
        Err(e) if e.status() == Some(401) => {
          // Forgetting ends this listener among the chats, so it runs apart.
          let service = self.clone();
          tokio::spawn(async move { service.forget(place.by).await });
          return;
        }
        Err(e) if matches!(e.status(), Some(403 | 404)) => {
          self
            .tell(
              chat,
              &format!("This chat's bridge ended: {} is gone.", place.name),
            )
            .await;
          return self.end(chat);
        }
        Err(e) => {
          eprintln!("messagent: reading the session of chat {chat}: {e}");
          tokio::time::sleep(delay).await;
          delay = (delay * 2).min(Duration::from_secs(60));
        }
      }
    }
  }

  /// Takes in one read of the session: what goes out to the chat, and what the agents have read.
  async fn take(
    &self,
    bridge: &Bridge,
    client: &Client,
    linked: &Linked,
    log: vibetogether_client::protocol::Log,
  ) {
    for entry in &log.entries {
      let ours = {
        let _carrying = bridge.carrying.lock().await;
        let place = bridge.place.lock().expect("no holder panics");
        if entry.n <= place.after {
          continue;
        }
        place.messages.contains_key(&entry.n)
      };
      if !ours && !for_bridge(entry) {
        self.deliver(bridge, client, linked, entry).await;
      }
      self.update(bridge, |place| place.after = entry.n);
    }
    // A message of the chat is seen once an agent has read it.
    let read = log
      .participants
      .iter()
      .filter(|(name, _)| name.contains('/'))
      .map(|(_, p)| p.read)
      .max()
      .unwrap_or(0);
    let seen: BTreeSet<i64> = {
      let place = bridge.place.lock().expect("no holder panics");
      place
        .unseen
        .range(..=read)
        .filter_map(|n| place.messages.get(n).copied())
        .collect()
    };
    if seen.is_empty() {
      return;
    }
    for message in seen {
      if let Err(e) = self.bot.react(bridge.chat, message, "👀").await {
        eprintln!(
          "messagent: can't mark a message of chat {} seen: {e}",
          bridge.chat
        );
      }
    }
    self.update(bridge, |place| {
      let messages = &place.messages;
      place
        .unseen
        .retain(|n| *n > read && messages.contains_key(n));
    });
  }

  /// Carries an entry out to the chat, riding out passing failures; one Telegram refuses is
  /// skipped.
  async fn deliver(&self, bridge: &Bridge, client: &Client, linked: &Linked, entry: &Entry) {
    let mut delay = Duration::from_secs(1);
    loop {
      match self.out(bridge, client, linked, entry).await {
        Ok(()) => return,
        Err(e) => {
          let transient = match e.downcast_ref::<telegram::Error>() {
            Some(e) => e.transient(),
            None => e.downcast_ref::<VtError>().is_some_and(VtError::transient),
          };
          eprintln!(
            "messagent: entry {} didn't reach chat {}: {e:#}",
            entry.n, bridge.chat
          );
          if !transient || delay > Duration::from_secs(60) {
            return;
          }
          tokio::time::sleep(delay).await;
          delay *= 2;
        }
      }
    }
  }

  /// Carries one entry out to the chat, if it is for people.
  async fn out(
    &self,
    bridge: &Bridge,
    client: &Client,
    linked: &Linked,
    entry: &Entry,
  ) -> Result<()> {
    let chat = bridge.chat;
    let place = bridge.place();
    let ty = base(&entry.ty);
    let body = || async {
      match &entry.text {
        Some(text) => Ok::<_, anyhow::Error>(text.clone().into_bytes()),
        None => Ok(client.fetch(&place.session, entry.n).await?.bytes),
      }
    };
    if ty == REACTION {
      let reaction: Value = serde_json::from_slice(&body().await?)?;
      let target = reaction["entry"]
        .as_u64()
        .context("a reaction names its entry")?;
      let emoji = reaction["emoji"].as_str().unwrap_or_default();
      if let Some(message) = place.messages.get(&target) {
        self.bot.react(chat, *message, emoji).await?;
        self.update(bridge, |place| {
          place.unseen.remove(&target);
        });
      }
      return Ok(());
    }
    // What pings no one in the chat reaches it without a notification: in a private chat its
    // person is pinged by their name, in a group anyone by a message for everyone.
    let ping = names_of(&entry.ty, "ping");
    let loud = ping.iter().any(|n| n == "*") || (place.private && ping.contains(&linked.account));
    let quiet = !loud;
    // A message in Telegram's own format carries no sender's name, so only the chat's person and
    // their own agents send one: nobody else poses as them.
    let own = format!("{}/", linked.account);
    let theirs = entry.from == linked.account || entry.from.starts_with(&own);
    if ty == TELEGRAM {
      if !theirs {
        return Ok(());
      }
      return self
        .native(chat, client, &place.session, &body().await?, quiet)
        .await;
    }
    if !is_message(&entry.ty) {
      return Ok(());
    }
    // In a private chat its person's own agents speak without a name, as a friend does.
    let name = (!(place.private && entry.from.starts_with(&own))).then(|| entry.from.clone());
    let is_text = matches!(ty.as_str(), "text/plain" | "text/markdown") && entry.filename.is_none();
    if !is_text {
      let payload = client.fetch(&place.session, entry.n).await?;
      let photo = ty == "image/jpeg";
      let upload = Upload {
        filename: payload.filename.unwrap_or_else(|| {
          format!(
            "entry-{}.{}",
            entry.n,
            ty.rsplit('/').next().unwrap_or("bin")
          )
        }),
        mime: ty.clone(),
        bytes: payload.bytes,
      };
      let mut body = json!({"chat_id": chat, "disable_notification": quiet});
      if let Some(name) = &name {
        body["caption"] = json!(telegram::label(name, 100));
      }
      let (method, field) = if photo {
        ("sendPhoto", "photo")
      } else {
        ("sendDocument", "document")
      };
      self.bot.send_file(method, field, &upload, body).await?;
      return Ok(());
    }
    let text = String::from_utf8_lossy(&body().await?).into_owned();
    let header = name
      .map(|n| format!("<b>{}</b>\n", markdown::escape(&n)))
      .unwrap_or_default();
    let mut pieces = telegram::split(&text, PIECE);
    if pieces.len() > MAX_PIECES {
      let rest: usize = pieces[MAX_PIECES..].iter().map(|p| p.chars().count()).sum();
      pieces.truncate(MAX_PIECES);
      pieces.push(format!("[{rest} more characters, in the session]"));
    }
    for piece in pieces {
      let html = if ty == "text/markdown" {
        markdown::to_html(&piece)
      } else {
        markdown::escape(&piece)
      };
      let html = format!("{header}{html}");
      let fits = html.encode_utf16().count() <= telegram::TEXT_LIMIT;
      let sent = if fits {
        self.bot.send(chat, &html, true, quiet).await
      } else {
        Err(telegram::Error::Refused {
          code: 400,
          description: "too long".into(),
        })
      };
      match sent {
        Ok(_) => {}
        // Formatting Telegram doesn't take goes as plain text.
        Err(telegram::Error::Refused { code: 400, .. }) => {
          let plain = match &header {
            h if h.is_empty() => piece.clone(),
            _ => format!("{}\n{piece}", entry.from),
          };
          for part in telegram::split(&plain, telegram::TEXT_LIMIT) {
            self.bot.send(chat, &part, false, quiet).await?;
          }
        }
        Err(e) => return Err(e.into()),
      }
    }
    Ok(())
  }

  /// Sends a message in Telegram's own format, with the fields of `telegram.md` alone, so that it
  /// reaches this chat and does nothing else.
  async fn native(
    &self,
    chat: i64,
    client: &Client,
    session: &str,
    body: &[u8],
    quiet: bool,
  ) -> Result<()> {
    const FIELDS: &[&str] = &[
      "text",
      "parse_mode",
      "entities",
      "caption",
      "caption_entities",
      "link_preview_options",
      "disable_notification",
      "show_caption_above_media",
      "has_spoiler",
    ];
    let given: serde_json::Map<String, Value> =
      serde_json::from_slice(body).context("a Telegram message is a JSON object")?;
    let mut message = serde_json::Map::new();
    for field in FIELDS {
      if let Some(value) = given.get(*field) {
        message.insert(field.to_string(), value.clone());
      }
    }
    message.insert("chat_id".into(), json!(chat));
    message
      .entry("disable_notification")
      .or_insert(json!(quiet));
    for (field, method) in [("photo", "sendPhoto"), ("document", "sendDocument")] {
      if let Some(n) = given.get(field).and_then(Value::as_u64) {
        let payload = client.fetch(session, n).await?;
        let upload = Upload {
          filename: payload.filename.unwrap_or_else(|| format!("entry-{n}")),
          mime: base(&payload.ty),
          bytes: payload.bytes,
        };
        self
          .bot
          .send_file(method, field, &upload, Value::Object(message))
          .await?;
        return Ok(());
      }
    }
    if !message.contains_key("text") {
      return Err(anyhow!(
        "a Telegram message has a text, a photo or a document"
      ));
    }
    self
      .bot
      .send_raw("sendMessage", Value::Object(message))
      .await?;
    Ok(())
  }
}

/// A message's file, where it has one: its id, size, type and file name.
fn file_of(message: &Message) -> Option<(String, Option<u64>, String, Option<String>)> {
  if let Some(photo) = message.photo.last() {
    return Some((
      photo.file_id.clone(),
      photo.file_size,
      "image/jpeg".into(),
      None,
    ));
  }
  for (document, kind) in [
    (&message.document, "application/octet-stream"),
    (&message.voice, "audio/ogg"),
    (&message.audio, "audio/mpeg"),
    (&message.video, "video/mp4"),
  ] {
    if let Some(d) = document {
      let ty = d.mime_type.clone().unwrap_or_else(|| kind.into());
      let filename = d
        .file_name
        .as_deref()
        .map(|f| telegram::label(f, 100))
        .or_else(|| {
          Some(format!(
            "telegram.{}",
            ty.rsplit('/').next().unwrap_or("bin")
          ))
        });
      // An image sent as a file arrives as an image, named.
      return Some((d.file_id.clone(), d.file_size, ty, filename));
    }
  }
  None
}

/// Revokes the token a client holds, its own login, by its id; a person token of before apps,
/// which has none, finds it among the logins. Whether it is gone.
async fn revoke(client: &Client, login: Option<&str>) -> bool {
  let login = match login {
    Some(login) => login.to_string(),
    None => match client.logins().await {
      Ok(logins) => match logins.into_iter().find(|l| l.current) {
        Some(login) => login.id,
        None => return false,
      },
      Err(e) if e.status() == Some(401) => return true,
      Err(e) => {
        eprintln!("messagent: can't list the logins to revoke one: {e}");
        return false;
      }
    },
  };
  match client.revoke_login(&login).await {
    Ok(()) => true,
    Err(e) if e.status() == Some(401) => true,
    Err(e) => {
      eprintln!("messagent: can't revoke a login: {e}");
      false
    }
  }
}

/// Runs the bot for good.
pub async fn serve(settings: Settings) -> Result<()> {
  private_dir(&settings.state)?;
  private_dir(&settings.state.join("chats"))?;
  let me = settings.bot.me().await.context("the bot's token")?;
  let username = me.username.context("the bot has no username")?;
  let service = Arc::new(Service {
    bot: settings.bot,
    username,
    server: settings.server,
    linked: Mutex::new(load(&settings.state.join("linked.json"))?),
    bridges: Mutex::new(HashMap::new()),
    linking: Mutex::new(HashSet::new()),
    allow: settings.allow,
    telegram: settings.telegram,
    state: settings.state,
  });
  for entry in fs::read_dir(service.state.join("chats"))? {
    let path = entry?.path();
    let Some(chat) = path
      .file_stem()
      .and_then(|s| s.to_str())
      .and_then(|s| s.parse::<i64>().ok())
      .filter(|_| path.extension().is_some_and(|e| e == "json"))
    else {
      continue;
    };
    let place: Place = load(&path)?;
    service.start(chat, place);
  }
  // Person tokens of before apps go: each person links again, by an app token, Luis's decision
  // (2026-11-16); their chats stay bridged meanwhile.
  let before: Vec<(i64, Linked)> = service
    .linked
    .lock()
    .expect("no holder panics")
    .iter()
    .filter(|(_, l)| l.login.is_none())
    .map(|(user, l)| (*user, l.clone()))
    .collect();
  for (user, linked) in before {
    revoke(&service.client(&linked.token), None).await;
    {
      let mut all = service.linked.lock().expect("no holder panics");
      all.remove(&user);
      service.save_linked(&all)?;
    }
    service
      .tell(
        user,
        "messagent now links by an app token: send /start to link again. Your chats stay bridged.",
      )
      .await;
  }
  if let Err(e) = service
    .bot
    .introduce(COMMANDS, DESCRIPTION, SHORT_DESCRIPTION)
    .await
  {
    eprintln!("messagent: can't set what Telegram shows of the bot: {e}");
  }
  eprintln!("messagent: serving as @{}", service.username);
  let offset_file = service.state.join("offset");
  let mut offset: i64 = fs::read_to_string(&offset_file)
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0);
  let mut delay = Duration::from_secs(1);
  loop {
    let updates = match service.bot.updates(offset, 50).await {
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
      service.handle(update).await;
      // Saved after each, so that a restart takes none twice.
      write_private(&offset_file, offset.to_string().as_bytes())?;
    }
  }
}
