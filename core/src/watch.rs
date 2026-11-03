//! The fetch process: it writes what the messenger receives to the log
//! and runs the wake command.

use std::{
    collections::HashSet,
    fs::{self, File},
    process::Command,
    sync::mpsc,
    thread,
};

use anyhow::Result;
use jiff::Timestamp;

use crate::{Config, Entry, Log, Party, Paths, messenger};

/// What the user reads when their agent can't be woken, since no agent is there to tell them.
pub const WAKE_FAILED: &str =
    "Your agent couldn't be woken: the wake command failed. Your messages wait for it.";

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
    let log = Log::new(paths);
    let mut logged: HashSet<String> = log
        .entries()?
        .into_iter()
        .filter_map(|entry| entry.message)
        .collect();
    let failed = {
        let paths = paths.clone();
        move || {
            if let Err(error) = tell(&paths, WAKE_FAILED) {
                eprintln!("messagent watch: can't tell the user the wake failed: {error:#}");
            }
        }
    };
    let waker = Waker::spawn(config.wake.clone(), failed);
    loop {
        let entries: Vec<Entry> = messenger
            .receive()?
            .into_iter()
            .filter(|entry| match &entry.message {
                Some(id) => logged.insert(id.clone()),
                None => true,
            })
            .collect();
        if !entries.is_empty() {
            log.append(&entries)?;
            waker.wake();
        }
    }
}

/// Sends the user a message from messagent itself, and logs it as one,
/// so that the agent reads it too once it is woken.
fn tell(paths: &Paths, text: &str) -> Result<()> {
    let messenger = messenger::open(&Config::load(paths)?.messenger, paths)?;
    let id = messenger.send(text, None)?;
    Log::new(paths).append(&[Entry {
        time: Timestamp::now(),
        from: Party::Messagent,
        name: None,
        message: Some(id),
        spoken: false,
        text: text.to_string(),
    }])
}
