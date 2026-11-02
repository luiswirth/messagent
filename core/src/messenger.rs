//! What a messenger can do, as capabilities: each messenger connects with the ones it has.
//! A message's id is the messenger's own, kept as an opaque string.

use anyhow::{Result, bail};

use crate::{Config, Entry, Paths, telegram::Telegram};

/// Finished pieces of content between the user and the agent; text only so far.
pub trait Messages {
    /// Splits a text into messages the messenger carries.
    fn split(&self, text: &str) -> Vec<String>;

    /// Sends one message to the user, and returns its id.
    /// A format is the messenger's own, passed to it as is; without one, the message is plain text.
    fn send(&self, message: &str, format: Option<&str>) -> Result<String>;

    /// Waits for what arrives next, as entries for the log:
    /// the user's messages, and notices about the chat, such as messages that may be lost.
    /// It rides out passing failures itself, so an error is one that ends watching.
    /// An entry may come again after a crash; its id tells.
    fn receive(&mut self) -> Result<Vec<Entry>>;
}

/// Reacting to a message of the chat with an emoji.
pub trait Reactions {
    fn react(&self, message: &str, emoji: &str) -> Result<()>;
}

/// A messenger: messages always, the other capabilities where it has them.
pub trait Messenger: Messages {
    fn reactions(&self) -> Option<&dyn Reactions> {
        None
    }
}

/// The messenger set up in the config.
pub fn open(config: &Config, paths: &Paths) -> Result<Box<dyn Messenger>> {
    match &config.telegram {
        Some(settings) => Ok(Box::new(Telegram::open(settings, paths)?)),
        None => bail!("no messenger is set up: `messagent setup` connects one"),
    }
}
