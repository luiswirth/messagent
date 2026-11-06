//! What a messenger can do, as capabilities: each messenger connects with the ones it has.
//! A message's id is the messenger's own, kept as an opaque string.

use anyhow::Result;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{
    Paths,
    telegram::{self, Telegram},
};

/// What arrives from the messenger.
#[derive(Debug, Clone, PartialEq)]
pub enum Arrived {
    /// A message of the user, which goes into the session as theirs.
    Message { id: String, text: String },
    /// A notice of messagent's own about the chat, such as that messages may be lost.
    Notice(String),
    /// A message of someone else, which is logged on this machine and goes no further.
    Stranger(Stranger),
}

/// A message from someone other than the user, as one line of the strangers' log.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Stranger {
    pub time: Timestamp,
    /// The sender's id in the messenger.
    pub id: String,
    pub name: String,
    pub text: String,
}

/// Finished pieces of content between the user and the others in the session; text only so far.
pub trait Messages {
    /// Splits a text into messages the messenger carries.
    fn split(&self, text: &str) -> Vec<String>;

    /// Sends one message of plain text to the user, and returns its id.
    fn send(&self, message: &str) -> Result<String>;

    /// The type of an entry in the messenger's own format, which `send_native` passes to it raw.
    fn native(&self) -> &'static str;

    /// Sends a message in the messenger's own format, as is, and returns its id.
    fn send_native(&self, message: &[u8]) -> Result<String>;

    /// Waits for what arrives next.
    /// It rides out passing failures itself, so an error is one that ends watching.
    fn receive(&mut self) -> Result<Vec<Arrived>>;

    /// Whether the user is connected yet.
    fn connected(&self) -> bool;
}

/// Reacting to a message of the chat with an emoji.
pub trait Reactions {
    /// The bot's reaction to a message is one emoji: this one replaces any earlier one,
    /// and an empty one removes it.
    fn react(&self, message: &str, emoji: &str) -> Result<()>;
}

/// A messenger: messages always, the other capabilities where it has them.
pub trait Messenger: Messages + Send {
    fn reactions(&self) -> Option<&dyn Reactions> {
        None
    }
}

/// The messenger set up, with its part of the config, a section named after it.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Settings {
    Telegram(telegram::Settings),
}

pub fn open(settings: &Settings, paths: &Paths) -> Result<Box<dyn Messenger>> {
    Ok(match settings {
        Settings::Telegram(settings) => Box::new(Telegram::open(settings, paths)?),
    })
}

/// What the user reads once they are connected.
pub const CONNECTED: &str = "Connected: your messages here now reach your agent.";

/// Splits a text into messages of at most `limit` UTF-16 code units,
/// at the last line break in the second half of each where there is one.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    let mut messages = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut length = 0;
        let mut cut = rest.len();
        let mut line_break = None;
        for (index, character) in rest.char_indices() {
            length += character.len_utf16();
            if length > limit {
                cut = index;
                break;
            }
            if character == '\n' && length > limit / 2 {
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
