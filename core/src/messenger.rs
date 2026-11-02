//! What a messenger can do, as capabilities: each messenger connects with the ones it has.

use anyhow::Result;

/// Finished pieces of content to the user; text only so far.
pub trait Messages {
    /// Splits a text into messages the messenger carries.
    fn split(&self, text: &str) -> Vec<String>;

    /// Sends one message to the user, and returns its id in the chat.
    fn send(&self, message: &str) -> Result<i64>;
}

/// Reacting to a message of the chat with an emoji.
pub trait Reactions {
    fn react(&self, message: i64, emoji: &str) -> Result<()>;
}

/// A messenger: messages always, the other capabilities where it has them.
pub trait Messenger: Messages {
    fn reactions(&self) -> Option<&dyn Reactions> {
        None
    }
}
