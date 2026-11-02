use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};

use crate::{Paths, config::write_private};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Party {
    User,
    Agent,
    /// messagent itself, telling the agent about the chat, such as a gap in fetching.
    Messagent,
}

/// One message of the chat, as one line of the log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub time: Timestamp,
    pub from: Party,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The Telegram update it came in, so that none is logged twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<i64>,
    /// The message's id in its Telegram chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<i64>,
    pub text: String,
}

impl Entry {
    pub fn notice(time: Timestamp, text: String) -> Self {
        Self {
            time,
            from: Party::Messagent,
            name: None,
            update: None,
            message: None,
            text,
        }
    }

    /// How the agent reads it, in the machine's time zone.
    pub fn render(&self) -> String {
        let name = self.name.as_deref().unwrap_or(match self.from {
            Party::User => "user",
            Party::Agent => "agent",
            Party::Messagent => "messagent",
        });
        format!("[{}] {name}: {}", local(self.time), self.text)
    }
}

pub fn local(time: Timestamp) -> String {
    time.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// The log of the chat: an append-only file of JSON lines, and the agent's read cursor.
/// Every access holds an exclusive lock on the log file,
/// so a reader never sees half of an append.
pub struct Log {
    pub path: PathBuf,
    pub cursor: PathBuf,
}

impl Log {
    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths.log(),
            cursor: paths.cursor(),
        }
    }

    fn open(&self) -> Result<File> {
        fs::create_dir_all(self.path.parent().context("the log has a parent")?)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&self.path)
            .with_context(|| format!("can't open {}", self.path.display()))?;
        file.lock()?;
        Ok(file)
    }

    pub fn append(&self, entries: &[Entry]) -> Result<()> {
        let mut lines = String::new();
        for entry in entries {
            lines += &serde_json::to_string(entry)?;
            lines.push('\n');
        }
        self.open()?.write_all(lines.as_bytes())?;
        Ok(())
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut text = String::new();
        self.open()?.read_to_string(&mut text)?;
        parse(&text)
    }

    /// The entries for the agent past its cursor, which then moves past them.
    pub fn read(&self) -> Result<Vec<Entry>> {
        let mut file = self.open()?;
        let start = match fs::read_to_string(&self.cursor) {
            Ok(text) => text.trim().parse().context("the read cursor is corrupt")?,
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        file.seek(SeekFrom::Start(start))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let entries = parse(&text)?;
        let end = start + text.len() as u64;
        write_private(&self.cursor, end.to_string().as_bytes())?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.from != Party::Agent)
            .collect())
    }
}

fn parse(text: &str) -> Result<Vec<Entry>> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("bad log line: {line}")))
        .collect()
}
