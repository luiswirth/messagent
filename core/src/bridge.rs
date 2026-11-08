//! The bridge: messagent as a participant in one Vibe Together room per chat, under the user's name,
//! carrying the messenger chat in and out as Vibe Together speech
//! (`https://vibetogether.agentics.lwirth.com/speech.md`).
//! The room is an agentwire session, which is only its transport.
//!
//! What the user writes becomes their speech, and what the others say goes out to the user.
//! messagent's own notices come from a second participant, `<user>/messagent`,
//! so that no one takes them for the user's words.
//! The agent is the participant the config names: how far it has read marks the user's
//! messages seen, and the bridge wakes it when an entry for it arrives while it is not receiving.
//! Others may take part for the user too, such as through a page, and count for neither.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::OpenOptionsExt,
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    Config, Paths,
    agentwire::{self, Entry, JOIN, LEAVE, Received, Relay},
    config::write_private,
    messenger::{self, Arrived, Image, Messenger, Stranger},
    reactions::{self, Reaction},
};

/// What the user reads when their agent can't be woken, since no agent is there to tell them.
pub const WAKE_FAILED: &str =
    "Your agent couldn't be woken: the wake command failed. Your messages wait for it.";

/// The type of what the user and messagent say: speech in plain text, which pings everyone
/// in the room, since everyone in the chat's room is in the conversation.
const SPEECH: &str = "text/plain; charset=utf-8";

/// A type without its parameters, such as `ping`.
fn base(kind: &str) -> String {
    kind.split(';').next().unwrap_or("").trim().to_string()
}

/// Whether an entry is an image, by the base of its type.
fn is_image(entry: &Entry) -> bool {
    entry
        .kind
        .split(';')
        .next()
        .is_some_and(|base| base.trim().to_ascii_lowercase().starts_with("image/"))
}

/// The chat's room, its agentwire session, as the bridge keeps it across runs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Session {
    /// The session's link, which the agent joins the room with.
    pub link: String,
    /// The user's own link.
    pub me: String,
    /// The own link of messagent's participant for its notices.
    pub notices: String,
    /// The last entry the bridge has handled.
    #[serde(default)]
    pub after: i64,
    /// The user's entries, each with its message in the messenger.
    #[serde(default)]
    pub messages: BTreeMap<i64, String>,
    /// The user's entries the agent has not read yet, nor reacted to.
    #[serde(default)]
    pub unseen: BTreeSet<i64>,
    /// The last entry for the agent, and how far the agent has read.
    #[serde(default)]
    pub latest: i64,
    #[serde(default)]
    pub read: i64,
}

impl Session {
    pub fn load(paths: &Paths) -> Result<Option<Self>> {
        match fs::read_to_string(paths.session()) {
            Ok(text) => Ok(Some(
                serde_json::from_str(&text).context("the session file")?,
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        write_private(&paths.session(), &serde_json::to_vec(self)?)
    }

    /// Whether entries wait for the agent that it has not read.
    pub fn waiting(&self) -> bool {
        self.latest > self.read
    }
}

/// The name of messagent's participant for a user's name.
fn notices_name(user: &str) -> String {
    format!("{user}/messagent")
}

/// Runs the wake command, one run at a time:
/// wakes asked for during a run become one more run after it.
pub struct Waker {
    sender: Option<mpsc::Sender<()>>,
}

impl Waker {
    /// `failed` runs once per streak of failed runs, at its first.
    pub fn spawn(command: String, failed: impl Fn() + Send + 'static) -> Self {
        if command.trim().is_empty() {
            return Self { sender: None };
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut failing = false;
            while receiver.recv().is_ok() {
                while receiver.try_recv().is_ok() {}
                let succeeded = match Command::new("sh").arg("-c").arg(&command).status() {
                    Ok(status) if status.success() => true,
                    Ok(status) => {
                        eprintln!("messagent watch: the wake command failed ({status})");
                        false
                    }
                    Err(error) => {
                        eprintln!("messagent watch: can't run the wake command: {error}");
                        false
                    }
                };
                if !succeeded && !failing {
                    failed();
                }
                failing = !succeeded;
            }
        });
        Self {
            sender: Some(sender),
        }
    }

    pub fn wake(&self) {
        if let Some(sender) = &self.sender {
            sender.send(()).ok();
        }
    }
}

/// The bridge while it runs: what both of its loops share.
struct Bridge {
    paths: Paths,
    config: Config,
    relay: Relay,
    session: Mutex<Option<Session>>,
}

/// A pause after a failure that may pass, growing up to a minute.
struct Backoff(Duration);

impl Backoff {
    fn new() -> Self {
        Self(Duration::from_secs(1))
    }

    fn wait(&mut self, error: &anyhow::Error) {
        let retry = error
            .downcast_ref::<agentwire::Refusal>()
            .and_then(|refusal| refusal.retry)
            .map(Duration::from_secs);
        let pause = retry.unwrap_or(self.0);
        eprintln!("messagent watch: {error:#}; trying again in {pause:?}");
        thread::sleep(pause);
        self.0 = (self.0 * 2).min(Duration::from_secs(60));
    }
}

impl Bridge {
    fn relay_url(&self) -> &str {
        self.config.relay.as_deref().unwrap_or(agentwire::RELAY)
    }

    /// Starts the chat's room, a session on the relay, with the user and messagent's notices in it.
    fn create(&self) -> Result<Session> {
        let link = self.relay.create(self.relay_url())?;
        let me = self.relay.join(&link, &self.config.name)?.link;
        let notices = self
            .relay
            .join(&link, &notices_name(&self.config.name))?
            .link;
        let session = Session {
            link,
            me,
            notices,
            after: 0,
            messages: BTreeMap::new(),
            unseen: BTreeSet::new(),
            latest: 0,
            read: 0,
        };
        session.save(&self.paths)?;
        Ok(session)
    }

    /// The session, started if there is none yet, waiting for the relay as long as it takes;
    /// true if it is new.
    fn ensure(&self) -> Result<(Session, bool)> {
        let mut current = self.session.lock().expect("no holder panics");
        if let Some(session) = &*current {
            return Ok((session.clone(), false));
        }
        // Riding out a relay that is away, since nothing goes on without a session.
        let mut backoff = Backoff::new();
        let session = loop {
            match self.create() {
                Ok(session) => break session,
                Err(error) => backoff.wait(&error),
            }
        };
        *current = Some(session.clone());
        Ok((session, true))
    }

    fn current(&self) -> Option<Session> {
        self.session.lock().expect("no holder panics").clone()
    }

    /// Changes the session, and saves it.
    fn update(&self, change: impl FnOnce(&mut Session)) -> Result<()> {
        let mut current = self.session.lock().expect("no holder panics");
        if let Some(session) = current.as_mut() {
            change(session);
            session.save(&self.paths)?;
        }
        Ok(())
    }

    /// The session is gone from the relay: the next one starts afresh.
    fn forget(&self, link: &str) {
        let mut current = self.session.lock().expect("no holder panics");
        if current.as_ref().is_some_and(|session| session.link == link) {
            eprintln!("messagent watch: the session is gone from the relay; starting a new one");
            *current = None;
            fs::remove_file(self.paths.session()).ok();
        }
    }

    /// Sends a payload with one of the bridge's own links, joining again if it left.
    /// It rides out passing failures, and starts a new session if the relay lost this one.
    fn send(&self, notices: bool, kind: &str, filename: Option<&str>, body: &[u8]) -> Result<i64> {
        let mut backoff = Backoff::new();
        loop {
            let (session, _) = self.ensure()?;
            let me = if notices {
                &session.notices
            } else {
                &session.me
            };
            match self.relay.send(me, kind, filename, body) {
                Ok(n) => return Ok(n),
                Err(error) => match agentwire::status(&error) {
                    Some(404) => self.forget(&session.link),
                    Some(403) => {
                        if let Err(error) = self.relay.rejoin(me) {
                            backoff.wait(&error);
                        }
                    }
                    Some(400 | 413) => return Err(error),
                    _ => backoff.wait(&error),
                },
            }
        }
    }

    fn notice(&self, text: &str) -> Result<i64> {
        self.send(true, SPEECH, None, text.as_bytes())
    }

    /// Sends what the user said, as theirs, and keeps which message of the messenger it is.
    fn said(&self, id: &str, kind: &str, filename: Option<&str>, body: &[u8]) -> Result<()> {
        let n = self.send(false, kind, filename, body)?;
        self.update(|session| {
            session.messages.insert(n, id.to_string());
            session.unseen.insert(n);
        })
    }

    /// Carries an image the user sent into the room, as an entry of its type,
    /// or a note that it couldn't be where that fails, such as one too large for the relay.
    fn image(&self, messenger: &dyn Messenger, id: &str, file: &messenger::File) -> Result<()> {
        let sent = messenger
            .download(file)
            .and_then(|bytes| self.said(id, &file.kind, file.filename.as_deref(), &bytes));
        if let Err(error) = sent {
            eprintln!("messagent watch: can't carry an image into the room: {error:#}");
            let note = format!("[an image messagent couldn't carry: {error:#}]");
            self.said(id, SPEECH, None, note.as_bytes())?;
        }
        Ok(())
    }

    /// Takes in what arrived from the messenger.
    /// An image comes first, and its caption after it, as the user's speech.
    fn arrived(&self, messenger: &dyn Messenger, arrived: Arrived) -> Result<()> {
        match arrived {
            Arrived::Message { id, image, text } => {
                if let Some(file) = &image {
                    self.image(messenger, &id, file)?;
                }
                match text.filter(|text| !text.is_empty()) {
                    Some(text) => self.said(&id, SPEECH, None, text.as_bytes()),
                    None => Ok(()),
                }
            }
            Arrived::Notice(text) => self.notice(&text).map(drop),
            Arrived::Stranger(stranger) => log_stranger(&self.paths, &stranger),
        }
    }
}

/// Logs a stranger's message on this machine, where it goes no further.
pub fn log_stranger(paths: &Paths, stranger: &Stranger) -> Result<()> {
    fs::create_dir_all(&paths.state)?;
    let mut line = serde_json::to_string(stranger)?;
    line.push('\n');
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(paths.strangers())?
        .write_all(line.as_bytes())?;
    Ok(())
}

/// What one entry of the session is for the bridge.
#[derive(Debug, PartialEq)]
pub enum Seen {
    /// Nothing for the user or the agent: joining and leaving.
    Nothing,
    /// The bridge's own, which is for the agent.
    Own,
    /// What goes out to the user.
    Out,
}

/// Sorts an entry, given the user's name.
pub fn sort(entry: &Entry, user: &str) -> Seen {
    if entry.is(JOIN) || entry.is(LEAVE) {
        Seen::Nothing
    } else if entry.from == user || entry.from == notices_name(user) {
        Seen::Own
    } else {
        Seen::Out
    }
}

/// An entry of someone else, as the plain text the user reads,
/// named after its sender unless it is the agent's.
pub fn plain(entry: &Entry, agent: &str) -> String {
    let text = match &entry.text {
        Some(text) => text.clone(),
        None => {
            let file = entry
                .filename
                .as_deref()
                .map(|name| format!(" {name}"))
                .unwrap_or_default();
            format!(
                "[a file{file} ({}): messagent carries only text and images so far]",
                entry.kind
            )
        }
    };
    if entry.from == agent {
        text
    } else {
        format!("{}: {text}", entry.from)
    }
}

/// Whether an error is the messenger refusing, or a message it can't take,
/// rather than a failure that may pass.
fn refused(error: &anyhow::Error) -> bool {
    error.downcast_ref::<crate::telegram::Refusal>().is_some()
        || error.downcast_ref::<serde_json::Error>().is_some()
}

impl Bridge {
    /// Carries one entry out to the user, if it is for them.
    fn out(&self, messenger: &dyn Messenger, entry: &Entry, session: &Session) -> Result<()> {
        if entry.is(reactions::REACTION) {
            let reaction: Reaction = serde_json::from_str(entry.text.as_deref().unwrap_or(""))?;
            let Some(message) = session.messages.get(&reaction.entry) else {
                return Ok(());
            };
            if let Some(reactions) = messenger.reactions() {
                reactions.react(message, &reaction.emoji)?;
            }
            return self.update(|session| {
                session.unseen.remove(&reaction.entry);
            });
        }
        let image = |n: i64| -> Result<Image> {
            let payload = self.relay.fetch(&session.me, n)?;
            Ok(Image {
                kind: base(&payload.kind),
                filename: payload.filename,
                bytes: payload.bytes,
            })
        };
        // What doesn't ping the user reaches them as an fyi, without a notification.
        let quiet = !entry.pings(&self.config.name);
        if entry.is(messenger.native()) {
            let body = match &entry.text {
                Some(text) => text.clone().into_bytes(),
                None => self.relay.fetch(&session.me, entry.n)?.bytes,
            };
            messenger.send_native(&body, &image, quiet)?;
            return Ok(());
        }
        if is_image(entry) {
            messenger.send_image(&image(entry.n)?, quiet)?;
            return Ok(());
        }
        for part in messenger.split(&plain(entry, &self.config.agent)) {
            messenger.send(&part, quiet)?;
        }
        Ok(())
    }

    /// Takes in one receive: what goes out to the user, how far the agent has read,
    /// and whether to wake it.
    fn received(&self, messenger: &dyn Messenger, received: Received, waker: &Waker) -> Result<()> {
        let Some(session) = self.current() else {
            return Ok(());
        };
        let user = &self.config.name;
        let agent = &self.config.agent;
        let mut latest = session.latest;
        let mut fresh = false;
        for entry in &received.entries {
            if entry.n <= session.after {
                continue;
            }
            let seen = sort(entry, user);
            let for_agents = entry.pings(agent);
            if seen != Seen::Nothing && entry.from != *agent && for_agents {
                latest = entry.n;
                fresh = true;
            }
            // One that pings messagent alone is for its use, such as an image a Telegram
            // message sends, and not for the user by itself; a message in the messenger's own
            // format is for messagent to carry out, whomever it pings.
            let for_messagent = !entry.is(messenger.native())
                && entry.ping().is_some_and(|names| {
                    !names.is_empty() && names.iter().all(|name| *name == notices_name(user))
                });
            if seen == Seen::Out && !for_messagent {
                let current = self.current().unwrap_or_else(|| session.clone());
                self.deliver(messenger, entry, &current)?;
            }
            self.update(|session| session.after = entry.n)?;
        }
        let presence = received.presence.get(agent);
        let read = presence.map_or(session.read, |presence| presence.read);
        let current = self.current().unwrap_or_else(|| session.clone());
        for n in current.unseen.range(..=read) {
            if let (Some(reactions), Some(message)) =
                (messenger.reactions(), current.messages.get(n))
                && let Err(error) = reactions.react(message, "👀")
            {
                eprintln!("messagent watch: can't mark a message seen: {error:#}");
            }
            self.update(|session| {
                session.unseen.remove(n);
            })?;
        }
        self.update(|session| {
            session.latest = latest;
            session.read = read;
        })?;
        if fresh && !presence.is_some_and(|presence| presence.receiving) {
            waker.wake();
        }
        Ok(())
    }

    /// Carries an entry out, riding out passing failures;
    /// one the messenger refuses becomes a notice for the agent.
    fn deliver(&self, messenger: &dyn Messenger, entry: &Entry, session: &Session) -> Result<()> {
        let mut backoff = Backoff::new();
        loop {
            match self.out(messenger, entry, session) {
                Ok(()) => return Ok(()),
                Err(error) if refused(&error) => {
                    let text = format!("Entry {} didn't reach the user: {error:#}", entry.n);
                    eprintln!("messagent watch: {text}");
                    return self.notice(&text).map(drop);
                }
                Err(error) => backoff.wait(&error),
            }
        }
    }

    /// Receives from the session for good, carrying what comes out to the user.
    /// A change of presence while it is between two receives, such as the agent reading,
    /// returns no receive: it is seen at the next one that returns, so a 👀 may come late.
    fn listen(self: Arc<Self>, waker: Waker) -> Result<()> {
        let messenger = messenger::open(&self.config.messenger, &self.paths)?;
        let mut backoff = Backoff::new();
        loop {
            let (session, _) = self.ensure()?;
            match self.relay.receive(&session.me, session.after) {
                Ok(received) => {
                    backoff = Backoff::new();
                    self.received(messenger.as_ref(), received, &waker)?;
                }
                Err(error) => match agentwire::status(&error) {
                    Some(404) => self.forget(&session.link),
                    Some(403) => {
                        if let Err(error) = self.relay.rejoin(&session.me) {
                            backoff.wait(&error);
                        }
                    }
                    _ => backoff.wait(&error),
                },
            }
        }
    }
}

pub fn watch(paths: &Paths) -> Result<()> {
    let config = Config::load(paths)?;
    fs::create_dir_all(&paths.state)?;
    let lock = File::create(paths.watch_lock())?;
    // Another watch of this setup holds the lock: this one waits its turn rather than fails,
    // so that a service restarted while another watch runs neither loops nor alarms anyone.
    if lock.try_lock().is_err() {
        eprintln!("messagent watch: another `messagent watch` runs; waiting until it ends");
        lock.lock()?;
    }
    let mut messenger = messenger::open(&config.messenger, paths)?;
    let bridge = Arc::new(Bridge {
        paths: paths.clone(),
        relay: Relay::new(),
        session: Mutex::new(Session::load(paths)?),
        config,
    });
    let mut listening = false;
    loop {
        // The session starts once the user is connected, and the listening with it.
        if messenger.connected() && !listening {
            bridge.ensure()?;
            let failed = {
                let bridge = bridge.clone();
                move || {
                    if let Err(error) = wake_failed(&bridge) {
                        eprintln!(
                            "messagent watch: can't tell the user the wake failed: {error:#}"
                        );
                    }
                }
            };
            let waker = Waker::spawn(bridge.config.wake.clone(), failed);
            let listener = bridge.clone();
            thread::spawn(move || {
                if let Err(error) = listener.listen(waker) {
                    eprintln!("messagent watch: listening to the session ended: {error:#}");
                    std::process::exit(1);
                }
            });
            listening = true;
        }
        for arrived in messenger.receive()? {
            bridge.arrived(messenger.as_ref(), arrived)?;
        }
    }
}

/// Tells the user, since no agent is there to, and the agent, for once it is woken.
fn wake_failed(bridge: &Bridge) -> Result<()> {
    messenger::open(&bridge.config.messenger, &bridge.paths)?.send(WAKE_FAILED, false)?;
    bridge.notice(WAKE_FAILED)?;
    Ok(())
}
