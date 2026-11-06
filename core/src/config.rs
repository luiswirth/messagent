use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{Paths, messenger};

/// What setup fixes: the user's name, the relay, the wake command, and the messenger with its settings.
/// It holds a messenger's credentials, so only its owner may read it.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Config {
    /// The user's name in the session, such as `luis`.
    pub name: String,
    /// The agent's name in the session, such as `luis/claude`:
    /// only its reading marks a message seen, and only it is woken.
    pub agent: String,
    /// The relay the session is on, if not the public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// A shell command that wakes the agent, run when an entry for it arrives while it is not
    /// receiving, empty for none.
    #[serde(default)]
    pub wake: String,
    #[serde(flatten)]
    pub messenger: messenger::Settings,
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        let text = fs::read_to_string(&file).with_context(|| {
            format!(
                "can't read {}; `messagent setup` connects a messenger",
                file.display()
            )
        })?;
        toml::from_str(&text).with_context(|| format!("can't parse {}", file.display()))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        write_private(&paths.config_file(), toml::to_string(self)?.as_bytes())
    }
}

/// Replaces the file at once, readable by its owner only.
pub fn write_private(file: &Path, content: &[u8]) -> Result<()> {
    let directory = file.parent().context("a file has a parent")?;
    fs::create_dir_all(directory)?;
    let temporary = file.with_extension("tmp");
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?
        .write_all(content)?;
    fs::rename(&temporary, file)?;
    Ok(())
}
