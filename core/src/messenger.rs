//! What a messenger can do, as capabilities: each messenger connects with the ones it has.
//! A message's id is the messenger's own, kept as an opaque string.

use std::sync::mpsc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    Entry, Paths,
    calls::Call,
    discord::{self, Discord},
    telegram::{self, Telegram},
};

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
    /// The agent's reaction to a message is one emoji: this one replaces any earlier one.
    fn react(&self, message: &str, emoji: &str) -> Result<()>;
}

/// A messenger: messages always, the other capabilities where it has them.
pub trait Messenger: Messages {
    fn reactions(&self) -> Option<&dyn Reactions> {
        None
    }

    /// Calls with the user, each handed over as it starts; taken once, by `watch`.
    fn calls(&mut self) -> Option<mpsc::Receiver<Call>> {
        None
    }
}

/// The messenger set up, with its part of the config, a section named after it.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Settings {
    Telegram(telegram::Settings),
    Discord(discord::Settings),
}

pub fn open(settings: &Settings, paths: &Paths) -> Result<Box<dyn Messenger>> {
    Ok(match settings {
        Settings::Telegram(settings) => Box::new(Telegram::open(settings, paths)?),
        Settings::Discord(settings) => Box::new(Discord::open(settings, paths)?),
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
