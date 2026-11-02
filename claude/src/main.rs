//! Wakes a Claude Code session for messagent:
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
use serde::Deserialize;
use serde_json::{Value, json};

/// Wakes a Claude Code session when messages arrive through messagent.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// The wake command: delivers the unread messages to the session,
    /// through its channel while it runs, by resuming it in this directory while it doesn't.
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

const INSTRUCTIONS: &str = "Messages from the user's messenger chat arrive here, \
    as `messagent read` prints them, and count as read. \
    Only what you send with `messagent send` reaches the user.";

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
    Ok(state.join("messagent-claude/channel.sock"))
}

/// The unread messages, marked read, empty where there are none.
fn unread() -> Result<String> {
    let output = Command::new("messagent")
        .arg("read")
        .output()
        .context("can't run `messagent read`")?;
    if !output.status.success() {
        bail!(
            "`messagent read` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim_end().to_string())
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
             so the messages wait until it runs `messagent read`",
        )?;
        stream.write_all(b"wake\n")?;
        return Ok(());
    }
    let messages = unread()?;
    if messages.is_empty() {
        return Ok(());
    }
    let prompt = format!("{INSTRUCTIONS}\n\n{messages}");
    let status = Command::new("claude")
        .args(["--print", "--resume", session])
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
    let messages = unread()?;
    if messages.is_empty() {
        return Ok(());
    }
    emit(
        stdout,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": { "content": messages },
        }),
    )
}

fn channel() -> Result<()> {
    let socket = socket()?;
    std::fs::create_dir_all(socket.parent().context("the socket has a parent")?)?;
    if UnixStream::connect(&socket).is_ok() {
        bail!("another session holds the messagent channel");
    }
    std::fs::remove_file(&socket).ok();
    let listener = UnixListener::bind(&socket)?;
    let stdout = Arc::new(Mutex::new(io::stdout()));

    let writer = stdout.clone();
    thread::spawn(move || {
        for _ in listener.incoming() {
            if let Err(error) = deliver(&writer) {
                eprintln!("messagent channel: {error:#}");
            }
        }
    });

    for line in io::stdin().lock().lines() {
        let Ok(request) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default();
        if method == "notifications/initialized" {
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
    std::fs::remove_file(&socket).ok();
    Ok(())
}
