//! The few operations of agentwire, the transport Vibe Together is built on, that the bridge uses:
//! `https://agentwire.agentics.lwirth.com/agentwire.md`.

use std::{collections::BTreeMap, fmt, time::Duration};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// The public relay, the default.
pub const RELAY: &str = "https://agentwire.agentics.lwirth.com";

pub const JOIN: &str = "application/vnd.agentwire.join";
pub const LEAVE: &str = "application/vnd.agentwire.leave";

/// The largest payload the relay takes, in bytes.
const MAX_PAYLOAD: u64 = 10 * 1024 * 1024;

/// An entry's payload, as fetched.
pub struct Payload {
    pub kind: String,
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
}

/// How long a receive waits at most, in seconds, before it is made again.
const WAIT: u64 = 50;

#[derive(Clone)]
pub struct Relay {
    agent: ureq::Agent,
}

/// The relay refused a request, as opposed to the network failing.
#[derive(Debug)]
pub struct Refusal {
    pub status: u16,
    pub reason: String,
    /// `Retry-After`, in seconds, where the relay gives one.
    pub retry: Option<u64>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "the relay refused ({}): {}", self.status, self.reason)
    }
}

impl std::error::Error for Refusal {}

/// The status of a refusal, if the error is one.
pub fn status(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<Refusal>()
        .map(|refusal| refusal.status)
}

#[derive(Deserialize, Debug)]
pub struct Joined {
    pub n: i64,
    pub link: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Entry {
    pub n: i64,
    pub from: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Entry {
    /// Whom it pings, by chat's `ping` parameter of its type: `None` for everyone,
    /// which is also whom an entry without `ping` pings, and no name for no one.
    /// `to`, which `ping` replaced, is still taken for a while so that no agent breaks.
    pub fn ping(&self) -> Option<Vec<String>> {
        let ping = self.kind.split(';').skip(1).find_map(|parameter| {
            let (key, value) = parameter.split_once('=')?;
            let key = key.trim();
            (key.eq_ignore_ascii_case("ping") || key.eq_ignore_ascii_case("to"))
                .then(|| value.trim())
        })?;
        let names: Vec<String> = ping
            .trim_matches('"')
            .split_whitespace()
            .map(str::to_string)
            .collect();
        (!names.iter().any(|name| name == "*")).then_some(names)
    }

    /// Whether it pings `name`.
    pub fn pings(&self, name: &str) -> bool {
        self.ping()
            .is_none_or(|names| names.iter().any(|pinged| pinged == name))
    }

    /// Whether its type is `kind`, whatever the parameters.
    pub fn is(&self, kind: &str) -> bool {
        self.kind
            .split(';')
            .next()
            .is_some_and(|base| base.trim().eq_ignore_ascii_case(kind))
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Presence {
    pub read: i64,
    pub receiving: bool,
}

/// What one receive brought: the entries, and everyone's presence.
#[derive(Debug, Default)]
pub struct Received {
    pub entries: Vec<Entry>,
    pub presence: BTreeMap<String, Presence>,
}

#[derive(Deserialize)]
struct PresenceLine {
    presence: BTreeMap<String, Presence>,
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl Relay {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(WAIT + 30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }

    fn answer(mut response: ureq::http::Response<ureq::Body>) -> Result<String> {
        let status = response.status().as_u16();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()?.parse().ok());
        let text = response.body_mut().read_to_string()?;
        if status != 200 {
            return Err(Refusal {
                status,
                reason: text.trim().to_string(),
                retry,
            }
            .into());
        }
        Ok(text)
    }

    /// Creates a session on the relay at `relay`, and returns its link.
    pub fn create(&self, relay: &str) -> Result<String> {
        let url = format!("{}/v2/s", relay.trim_end_matches('/'));
        let response = self.agent.post(&url).send_empty()?;
        Ok(Self::answer(response)?.trim().to_string())
    }

    /// Joins the session at `link` as `name`.
    pub fn join(&self, link: &str, name: &str) -> Result<Joined> {
        let url = format!("{link}/join?as={}", encode(name));
        let text = Self::answer(self.agent.post(&url).send_empty()?)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Joins again after leaving, with the caller's own link.
    pub fn rejoin(&self, me: &str) -> Result<Joined> {
        let text = Self::answer(self.agent.post(&format!("{me}/join")).send_empty()?)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Sends a payload of type `kind`, and returns its entry's number.
    /// Sends a payload of type `kind`, named as a file where it has a name,
    /// and returns its entry's number.
    pub fn send(&self, me: &str, kind: &str, filename: Option<&str>, body: &[u8]) -> Result<i64> {
        #[derive(Deserialize)]
        struct Sent {
            n: i64,
        }
        let url = match filename {
            Some(name) => format!("{me}?filename={}", encode(name)),
            None => me.to_string(),
        };
        let response = self
            .agent
            .post(&url)
            .header("Content-Type", kind)
            .send(body)?;
        let sent: Sent = serde_json::from_str(&Self::answer(response)?)?;
        Ok(sent.n)
    }

    /// Receives the entries after `after`, its own included, waiting a while for them,
    /// and returning for a change of presence too.
    /// It moves the caller's read position to the last entry it returns.
    pub fn receive(&self, me: &str, after: i64) -> Result<Received> {
        let url = format!("{me}?after={after}&own=1&presence=1&wait={WAIT}");
        let text = Self::answer(self.agent.get(&url).call()?)?;
        let mut received = Received::default();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value =
                serde_json::from_str(line).with_context(|| format!("a bad line: {line}"))?;
            if value.get("presence").is_some() {
                received.presence = serde_json::from_value::<PresenceLine>(value)?.presence;
            } else {
                received.entries.push(serde_json::from_value(value)?);
            }
        }
        Ok(received)
    }

    /// The payload of entry `n`, with its type and, where it has one, its file name.
    pub fn fetch(&self, me: &str, n: i64) -> Result<Payload> {
        let mut response = self.agent.get(&format!("{me}/{n}")).call()?;
        if response.status() != 200 {
            Self::answer(response)?;
            anyhow::bail!("the relay answered with no payload");
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let kind = header("content-type").unwrap_or_else(|| "application/octet-stream".into());
        let filename = header("content-disposition").and_then(|value| filename(&value));
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_PAYLOAD)
            .read_to_vec()?;
        Ok(Payload {
            kind,
            filename,
            bytes,
        })
    }
}

/// The file name in a `Content-Disposition`, as the relay gives it: `filename*=UTF-8''<encoded>`.
fn filename(disposition: &str) -> Option<String> {
    let (_, encoded) = disposition.split_once("filename*=UTF-8''")?;
    let encoded = encoded.split(';').next()?.trim().as_bytes();
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < encoded.len() {
        if encoded[i] == b'%'
            && let Some(byte) = encoded
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok())
        {
            bytes.push(byte);
            i += 3;
        } else {
            bytes.push(encoded[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// URL-encoded for a query.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'~' | b'-' | b'/' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_name_is_decoded() {
        assert_eq!(
            super::filename("inline; filename*=UTF-8''page%20one.png").as_deref(),
            Some("page one.png")
        );
        assert_eq!(super::filename("inline"), None);
    }
}
