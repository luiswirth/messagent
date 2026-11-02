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

use crate::{Config, Entry, Log, Paths, calls, messenger};

/// Runs the wake command, one run at a time:
/// wakes asked for during a run become one more run after it.
#[derive(Clone)]
pub struct Waker {
    sender: Option<mpsc::Sender<()>>,
}

impl Waker {
    pub fn spawn(command: String) -> Self {
        if command.trim().is_empty() {
            return Self { sender: None };
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            while receiver.recv().is_ok() {
                while receiver.try_recv().is_ok() {}
                match Command::new("sh").arg("-c").arg(&command).status() {
                    Ok(status) if status.success() => {}
                    Ok(status) => eprintln!("messagent watch: the wake command failed ({status})"),
                    Err(error) => eprintln!("messagent watch: can't run the wake command: {error}"),
                }
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
    let waker = Waker::spawn(config.wake.clone());
    if let Some(calls) = messenger.calls() {
        let (engine, socket, log, waker) = (
            config.speech.clone(),
            paths.speak_socket(),
            Log::new(paths),
            waker.clone(),
        );
        thread::spawn(move || {
            if let Err(error) = calls::serve(calls, engine, socket, log, waker) {
                eprintln!("messagent watch: calls: {error:#}");
            }
        });
    }
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
