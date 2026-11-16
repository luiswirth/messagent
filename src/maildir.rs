//! The agent's inbox and outbox, by their file convention alone
//! (`~/dev/vibetogether/inbox/AGENTS.md`, and the outbox drafted beside it):
//! each a directory like a maildir, `new/`, `cur/` and `tmp/`, one file per item, named
//! `<time>.<name>.<ext>`, the time in UTC to the microsecond, and the name, the inbox's sender or
//! the outbox's recipient, encoded one to one: a `/` as `_`, and every other byte but ASCII
//! letters, digits and `-` as `%XX`.

use std::{
  fs,
  io::{self, Write},
  os::unix::fs::{DirBuilderExt, OpenOptionsExt},
  path::PathBuf,
  sync::atomic::{AtomicU64, Ordering},
};

const TIME_FORMAT: &str = "%Y%m%dT%H%M%S.%6fZ";
const TIME_LEN: usize = "20261115T153012.123456Z".len();

/// An inbox or an outbox.
#[derive(Clone, Debug)]
pub struct Maildir {
  pub root: PathBuf,
}

/// An item's name, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Name {
  pub file: String,
  /// The sender, in an inbox; the recipient, in an outbox.
  pub who: String,
  pub ext: String,
}

impl Maildir {
  pub fn new(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn new_dir(&self) -> PathBuf {
    self.root.join("new")
  }

  pub fn cur_dir(&self) -> PathBuf {
    self.root.join("cur")
  }

  fn tmp_dir(&self) -> PathBuf {
    self.root.join("tmp")
  }

  pub fn create(&self) -> io::Result<()> {
    for dir in [self.new_dir(), self.cur_dir(), self.tmp_dir()] {
      fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    }
    Ok(())
  }

  /// Writes an item into `tmp/`, then links it into `new/` under a name no other item has, so
  /// that `new/` only ever holds whole items; its file name.
  pub fn deliver(&self, content: &[u8], who: &str, ext: &str) -> io::Result<String> {
    self.create()?;
    let who = encode(who);
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let tmp = self.tmp_dir().join(format!(
      "{}.{}-{}.{who}.{ext}",
      now(),
      std::process::id(),
      COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&tmp)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    let mut last = String::new();
    loop {
      let time = now();
      if time == last {
        continue;
      }
      let name = format!("{time}.{who}.{ext}");
      match fs::hard_link(&tmp, self.new_dir().join(&name)) {
        Ok(()) => {
          fs::remove_file(&tmp)?;
          return Ok(name);
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = time,
        Err(e) => return Err(e),
      }
    }
  }

  /// Whether an item rings the bell: its sender proposes, and the agent's do-not-disturb
  /// settings, the inbox's file `do-not-disturb`, dispose, as the inbox's convention says: each
  /// line fields to match and `ring` or `quiet`, the last match deciding.
  pub fn rings(&self, fields: &[(&str, &str)], proposed: bool) -> io::Result<bool> {
    let rules = match fs::read_to_string(self.root.join("do-not-disturb")) {
      Ok(rules) => rules,
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(proposed),
      Err(e) => return Err(e),
    };
    let mut rings = proposed;
    for line in rules.lines() {
      let line = line.split('#').next().unwrap_or_default();
      let mut words: Vec<_> = line.split_whitespace().collect();
      let verdict = match words.pop() {
        Some("ring") => true,
        Some("quiet") => false,
        // A line that is no rule leaves the decision to the inbox's own reader, which reports it.
        _ => continue,
      };
      let matches = words.iter().all(|word| {
        word
          .split_once('=')
          .is_some_and(|(key, value)| fields.iter().any(|&(k, v)| k == key && v == value))
      });
      if matches {
        rings = verdict;
      }
    }
    Ok(rings)
  }

  /// Rings the bell: the file `wake`.
  pub fn ring(&self) -> io::Result<()> {
    fs::OpenOptions::new()
      .create(true)
      .append(true)
      .mode(0o600)
      .open(self.root.join("wake"))?;
    Ok(())
  }

  /// The items in `new/` whose name is `who` or starts with it, oldest first.
  pub fn untaken(&self, who: impl Fn(&str) -> bool) -> io::Result<Vec<Name>> {
    self.create()?;
    let mut names: Vec<Name> = fs::read_dir(self.new_dir())?
      .filter_map(|entry| entry.ok())
      .filter_map(|entry| parse(&entry.file_name().to_string_lossy()))
      .filter(|name| who(&name.who))
      .collect();
    names.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(names)
  }

  /// Takes an item, by moving it into `cur/` first, so that of several takers exactly one gets
  /// it; where it lies now, or `None` where another took it.
  pub fn take(&self, name: &Name) -> io::Result<Option<PathBuf>> {
    let path = self.cur_dir().join(&name.file);
    match fs::rename(self.new_dir().join(&name.file), &path) {
      Ok(()) => Ok(Some(path)),
      Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(e),
    }
  }

  /// Whether an item is still in `new/`, not taken in.
  pub fn is_new(&self, file: &str) -> bool {
    self.new_dir().join(file).exists()
  }
}

fn now() -> String {
  jiff::Timestamp::now().strftime(TIME_FORMAT).to_string()
}

/// An item's name, where it is one.
pub fn parse(file: &str) -> Option<Name> {
  if file.starts_with('.') || file.len() < TIME_LEN + 3 {
    return None;
  }
  let (_, rest) = file.split_at(TIME_LEN);
  let rest = rest.strip_prefix('.')?;
  let (who, ext) = rest.split_once('.')?;
  if who.is_empty() || ext.is_empty() {
    return None;
  }
  Some(Name {
    file: file.to_string(),
    who: decode(who),
    ext: ext.to_string(),
  })
}

/// A name as it stands in a file name, one to one: no `.`, no `/`.
pub fn encode(name: &str) -> String {
  name
    .bytes()
    .map(|b| match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' => (b as char).to_string(),
      b'/' => "_".to_string(),
      b => format!("%{b:02X}"),
    })
    .collect()
}

/// The name a file name holds, as `encode` wrote it.
pub fn decode(encoded: &str) -> String {
  let bytes = encoded.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    let hex = bytes
      .get(i + 1..i + 3)
      .and_then(|h| std::str::from_utf8(h).ok())
      .and_then(|h| u8::from_str_radix(h, 16).ok());
    match (bytes[i], hex) {
      (b'%', Some(byte)) => {
        out.push(byte);
        i += 3;
      }
      (b'_', _) => {
        out.push(b'/');
        i += 1;
      }
      (byte, _) => {
        out.push(byte);
        i += 1;
      }
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_round_trip() {
    assert_eq!(encode("telegram/luis.w"), "telegram_luis%2Ew");
    assert_eq!(decode("telegram_luis%2Ew"), "telegram/luis.w");
    let name = parse("20261115T153012.123456Z.telegram_luis.md").unwrap();
    assert_eq!(name.who, "telegram/luis");
    assert_eq!(name.ext, "md");
    assert!(parse(".hidden").is_none());
  }

  #[test]
  fn delivers_and_takes() {
    let dir = std::env::temp_dir().join(format!("messagent-maildir-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let outbox = Maildir::new(&dir);
    let file = outbox.deliver(b"hi", "telegram/luis", "md").unwrap();
    let untaken = outbox.untaken(|w| w.starts_with("telegram/")).unwrap();
    assert_eq!(untaken.len(), 1);
    assert_eq!(untaken[0].file, file);
    assert!(outbox.take(&untaken[0]).unwrap().is_some());
    assert!(outbox.take(&untaken[0]).unwrap().is_none());
    assert!(!outbox.is_new(&file));
    fs::write(dir.join("do-not-disturb"), "from=telegram/luis quiet\n").unwrap();
    assert!(!outbox.rings(&[("from", "telegram/luis")], true).unwrap());
    assert!(outbox.rings(&[("from", "other")], true).unwrap());
    let _ = fs::remove_dir_all(&dir);
  }
}
