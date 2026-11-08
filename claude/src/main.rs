//! Wakes a Claude Code session for messagent, until impulse does:
//! through its channel while the session runs, by resuming it while it doesn't.
//! A second process on a running session would fork its transcript, so it never starts one.

use std::{
    env,
    io::{self, BufRead, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use messagent::{Config, Paths};
use serde::Deserialize;
use serde_json::{Value, json};

/// Wakes a Claude Code session when entries for it arrive in the messenger chat's session.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// The wake command: tells the session that entries wait for it,
    /// through its channel while it runs, by resuming it in this directory while it doesn't.
    /// The agent receives them itself, so that a message counts as seen only once it has,
    /// and a wake that fails leaves them unread.
    Wake {
        session: String,
        /// Further arguments for `claude` when it resumes the session, such as `--permission-mode`.
        #[arg(last = true)]
        claude: Vec<String>,
    },
    /// The channel, an MCP server the session starts,
    /// with `--dangerously-load-development-channels server:<its name>` while channels are a preview.
    Channel,
}

const INSTRUCTIONS: &str = "The user's messenger chat is a Vibe Together room, \
    whose link `messagent session` prints: what the user writes arrives as their speech \
    (https://vibetogether.agentics.lwirth.com/speech.md), and what you say reaches them \
    in the messenger, plain text always. \
    The room is an agentwire session, and this Claude Code session is resumed at each wake, \
    in a new process, so take part with something that keeps your own link on disk, \
    such as the agentwire script: \
    `curl -so agentwire https://agentwire.agentics.lwirth.com/agentwire`, then `sh agentwire help`. \
    Images go both ways as image entries: fetch one the user sends by its number. \
    Telegram's own formatting, captions, and reacting to a message, are entries of their own: \
    https://raw.githubusercontent.com/luiswirth/messagent/main/telegram.md and \
    https://raw.githubusercontent.com/luiswirth/messagent/main/reactions.md. \
    You are told here when entries wait for you.";

/// What the agent is told when entries wait: the room's link, and its name there,
/// so that it needs nothing more to take part.
fn waiting_message() -> String {
    let paths = Paths::from_env().ok();
    let link = paths
        .as_ref()
        .and_then(|paths| messagent::bridge::Session::load(paths).ok().flatten())
        .map(|session| session.link);
    let agent = paths
        .as_ref()
        .and_then(|paths| Config::load(paths).ok())
        .map(|config| config.agent);
    match (link, agent) {
        (Some(link), Some(agent)) => format!(
            "New entries wait for you in the messenger chat's room, {link}, \
             where you are {agent}: receive them."
        ),
        _ => "New entries wait for you in the messenger chat's room: receive them.".into(),
    }
}

fn main() -> Result<()> {
    match Cli::parse().action {
        Action::Wake { session, claude } => wake(&session, &claude),
        Action::Channel => channel(),
    }
}

fn socket() -> Result<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let state = env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".local/state"), PathBuf::from);
    let directory = state.join("messagent-claude");
    // Each setup's agent has a channel of its own.
    Ok(match messagent::paths::setup()? {
        Some(name) => directory.join(name),
        None => directory,
    }
    .join("channel.sock"))
}

/// Whether entries wait that the agent has not read.
fn waiting() -> Result<bool> {
    let paths = Paths::from_env()?;
    Ok(messagent::bridge::Session::load(&paths)?.is_some_and(|session| session.waiting()))
}

#[derive(Deserialize)]
struct Session {
    #[serde(rename = "sessionId")]
    session_id: String,
}

fn running(session: &str) -> Result<bool> {
    let output = Command::new("claude")
        .args(["agents", "--json"])
        .output()
        .context("can't run `claude agents --json`")?;
    if !output.status.success() {
        bail!("`claude agents --json` failed");
    }
    let sessions: Vec<Session> = serde_json::from_slice(&output.stdout)?;
    Ok(sessions.iter().any(|running| running.session_id == session))
}

fn wake(session: &str, claude: &[String]) -> Result<()> {
    if running(session)? {
        let mut stream = UnixStream::connect(socket()?).context(
            "the session runs without the messagent channel, \
             so the entries wait until it receives them",
        )?;
        stream.write_all(b"wake\n")?;
        return Ok(());
    }
    if !waiting()? {
        return Ok(());
    }
    let prompt = format!("{INSTRUCTIONS}\n\n{}", waiting_message());
    let status = Command::new("claude")
        .args(["--print", "--resume", session])
        .env(RESUMED, "1")
        .args(claude)
        .arg(prompt)
        .status()
        .context("can't run `claude`")?;
    if !status.success() {
        bail!("resuming the session failed ({status})");
    }
    Ok(())
}

/// Writes one JSON-RPC message per line, as MCP's stdio transport does.
fn emit(stdout: &Mutex<io::Stdout>, message: Value) -> Result<()> {
    let mut stdout = stdout.lock().expect("no writer panics");
    writeln!(stdout, "{message}")?;
    stdout.flush()?;
    Ok(())
}

fn deliver(stdout: &Mutex<io::Stdout>) -> Result<()> {
    if !waiting()? {
        return Ok(());
    }
    emit(
        stdout,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": { "content": waiting_message() },
        }),
    )
}

/// Set for a resumed session, which starts the channel too but ignores what it sends:
/// the prompt already tells the agent that entries wait.
const RESUMED: &str = "MESSAGENT_CLAUDE_RESUMED";

fn channel() -> Result<()> {
    let delivering = env::var_os(RESUMED).is_none();
    let socket = socket()?;
    std::fs::create_dir_all(socket.parent().context("the socket has a parent")?)?;
    let stdout = Arc::new(Mutex::new(io::stdout()));
    if delivering {
        if UnixStream::connect(&socket).is_ok() {
            bail!("another session holds the messagent channel");
        }
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket)?;
        let writer = stdout.clone();
        thread::spawn(move || {
            for _ in listener.incoming() {
                if let Err(error) = deliver(&writer) {
                    eprintln!("messagent channel: {error:#}");
                }
            }
        });
    }

    for line in io::stdin().lock().lines() {
        let Ok(request) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default();
        if delivering && method == "notifications/initialized" {
            // Messages that came while the session was closed.
            let writer = stdout.clone();
            thread::spawn(move || {
                if let Err(error) = deliver(&writer) {
                    eprintln!("messagent channel: {error:#}");
                }
            });
        }
        let Some(id) = request.get("id") else {
            continue;
        };
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": request["params"]["protocolVersion"],
                    "capabilities": { "experimental": { "claude/channel": {} } },
                    "serverInfo": { "name": "messagent", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                },
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no method {method}") },
            }),
        };
        emit(&stdout, response)?;
    }
    if delivering {
        std::fs::remove_file(&socket).ok();
    }
    Ok(())
}
