use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::Paths;

/// What setup fixes. It holds the bot's token, so only its owner may read it.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Config {
    pub token: String,
    /// The Telegram user id of the user, the only one who gets through.
    /// Unset until the user opens the start link with the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A shell command run after new entries reach the log, empty for none.
    #[serde(default)]
    pub wake: String,
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        let text = fs::read_to_string(&file).with_context(|| {
            format!(
                "can't read {}; `messagent setup` connects a bot",
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
