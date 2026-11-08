//! Reactions, an extension of Vibe Together for reacting to an entry with an emoji:
//! its spec is `reactions.md` at the root of the repository.

use serde::{Deserialize, Serialize};

pub const REACTION: &str = "application/vnd.agentwire.reactions.reaction+json";

/// The types a reaction had before, still taken for a while so that no agent breaks.
pub const REACTION_BEFORE: [&str; 2] = [
    "application/vnd.vibetogether.reactions.reaction+json",
    "application/vnd.vibe-together.reactions.reaction+json",
];

/// Whether an entry's type is a reaction's.
pub fn is_reaction(entry: &crate::agentwire::Entry) -> bool {
    entry.is(REACTION) || REACTION_BEFORE.iter().any(|kind| entry.is(kind))
}

/// A participant's reaction to an entry, which replaces its earlier one there;
/// an empty emoji takes it back.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Reaction {
    pub entry: i64,
    pub emoji: String,
}
