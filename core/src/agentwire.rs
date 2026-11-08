//! agentwire, the transport Vibe Together is built on, through its Rust participant
//! library: `https://agentwire.agentics.lwirth.com/agentwire.md`.
//! The bridge runs on threads, so this blocks on each call, on a runtime of its own.

use std::{sync::Arc, time::Duration};

use agentwire::{Client, Receive};
pub use agentwire::{Entry, Error, JOIN, Joined, LEAVE, Payload, RELAY, Received};
use anyhow::Result;

/// How long a receive waits at most, before it is made again.
const WAIT: Duration = Duration::from_secs(50);

/// The relays the bridge talks to, blocking on each call.
#[derive(Clone)]
pub struct Relay {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
}

/// The status of a refusal, if the error is one.
pub fn status(error: &anyhow::Error) -> Option<u16> {
    error.downcast_ref::<Error>().and_then(Error::status)
}

/// `Retry-After`, in seconds, if the error is a refusal that gives one.
pub fn retry(error: &anyhow::Error) -> Option<u64> {
    match error.downcast_ref::<Error>() {
        Some(Error::Refused(refusal)) => refusal.retry,
        _ => None,
    }
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl Relay {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime starts");
        Self {
            client: Client::new(),
            runtime: Arc::new(runtime),
        }
    }

    /// Creates a session on the relay at `relay`, and returns its link.
    pub fn create(&self, relay: &str) -> Result<String> {
        Ok(self.runtime.block_on(self.client.create(relay))?)
    }

    /// Joins the session at `link` as `name`.
    pub fn join(&self, link: &str, name: &str) -> Result<Joined> {
        Ok(self.runtime.block_on(self.client.join(link, name))?)
    }

    /// Joins again after leaving, with the caller's own link.
    pub fn rejoin(&self, me: &str) -> Result<Joined> {
        Ok(self.runtime.block_on(self.client.rejoin(me))?)
    }

    /// Sends a payload of type `kind`, named as a file where it has a name,
    /// and returns its entry's number.
    pub fn send(&self, me: &str, kind: &str, filename: Option<&str>, body: &[u8]) -> Result<u64> {
        let send = self.client.send(me, kind, filename, body.to_vec());
        Ok(self.runtime.block_on(send)?)
    }

    /// Receives the entries after `after`, its own included, waiting a while for them,
    /// and returning for a change of presence too.
    /// It moves the caller's read position to the last entry it returns.
    pub fn receive(&self, me: &str, after: u64) -> Result<Received> {
        let receive = Receive {
            after: Some(after),
            wait: Some(WAIT),
            own: true,
            presence: true,
            ..Receive::default()
        };
        Ok(self.runtime.block_on(self.client.receive(me, &receive))?)
    }

    /// The payload of entry `n`, with its type and, where it has one, its file name.
    pub fn fetch(&self, me: &str, n: u64) -> Result<Payload> {
        Ok(self.runtime.block_on(self.client.fetch(me, n))?)
    }
}
