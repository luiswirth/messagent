use std::{env, ffi::OsString, path::PathBuf};

use anyhow::{Context, Result, bail};

/// Where messagent keeps its files on the user's machine.
#[derive(Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
}

/// The variable that names a setup, where a machine has several, one per messenger.
pub const SETUP: &str = "MESSAGENT_SETUP";

/// The setup named in `MESSAGENT_SETUP`, if any.
pub fn setup() -> Result<Option<String>> {
    match env::var(SETUP) {
        Ok(name) if name.is_empty() => Ok(None),
        Ok(name) if name.contains('/') || name.starts_with('.') => {
            bail!("{SETUP} is a name, not a path: {name}")
        }
        Ok(name) => Ok(Some(name)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context(SETUP),
    }
}

impl Paths {
    /// The XDG base directories, on every platform alike,
    /// in a directory of the setup's own where `MESSAGENT_SETUP` names one.
    pub fn from_env() -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        let setup = setup()?;
        let base = |variable: &str, fallback: &str| {
            let base = env::var_os(variable)
                .filter(|value: &OsString| !value.is_empty())
                .map_or_else(|| home.join(fallback), PathBuf::from)
                .join("messagent");
            match &setup {
                Some(name) => base.join(name),
                None => base,
            }
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

    /// What a messenger's part keeps across runs, such as how far it has fetched.
    pub fn messenger_state(&self, messenger: &str) -> PathBuf {
        self.state.join(format!("{messenger}.json"))
    }

    /// Where `messagent watch` takes what the agent says into a call.
    pub fn speak_socket(&self) -> PathBuf {
        self.state.join("speak.sock")
    }

    pub fn watch_lock(&self) -> PathBuf {
        self.state.join("watch.lock")
    }
}
