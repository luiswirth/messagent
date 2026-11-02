use std::{env, ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};

/// Where messagent keeps its files on the user's machine.
pub struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    /// The XDG base directories, on every platform alike.
    pub fn from_env() -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        let base = |variable: &str, fallback: &str| {
            env::var_os(variable)
                .filter(|value: &OsString| !value.is_empty())
                .map_or_else(|| home.join(fallback), PathBuf::from)
                .join("messagent")
        };
        Ok(Self {
            config: base("XDG_CONFIG_HOME", ".config"),
            state: base("XDG_STATE_HOME", ".local/state"),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn log(&self) -> PathBuf {
        self.state.join("log.jsonl")
    }

    /// The byte offset in the log up to which the agent has read.
    pub fn cursor(&self) -> PathBuf {
        self.state.join("read")
    }

    pub fn fetch(&self) -> PathBuf {
        self.state.join("fetch.json")
    }

    pub fn watch_lock(&self) -> PathBuf {
        self.state.join("watch.lock")
    }
}
