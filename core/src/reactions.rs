//! Reactions, an extension of polylogue for reacting to an entry with an emoji:
//! its spec is `reactions.md` at the root of the repository.

use serde::{Deserialize, Serialize};

pub const REACTION: &str = "application/vnd.polylogue.reactions.reaction+json";

/// A participant's reaction to an entry, which replaces its earlier one there;
/// an empty emoji takes it back.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Reaction {
    pub entry: i64,
    pub emoji: String,
}
