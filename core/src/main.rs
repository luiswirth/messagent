use std::{fs, io::Read, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use messagent::{
    Config, Paths, bridge,
    bridge::Session,
    messenger::Settings,
    telegram::{self, Telegram},
};

/// The user's messenger chat with their agent, as a polylogue room:
/// messagent takes part in it as the user, and carries the chat in and out as polylogue speech.
/// Each messenger is a setup of its own: where a machine has several,
/// `MESSAGENT_SETUP` names the one a command acts on.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Connect a messenger, and print what the user does next.
    /// Only the user gets through.
    Setup {
        #[command(flatten)]
        messenger: Choice,
        /// The user's name in the session, such as `luis`.
        #[arg(long)]
        name: String,
        /// The agent's name in the session, such as `luis/claude`, which it joins under.
        #[arg(long)]
        agent: String,
        /// The agentwire relay, if not the public one.
        #[arg(long)]
        relay: Option<String>,
        /// A shell command `messagent watch` runs to wake the agent,
        /// when an entry for it arrives while it is not receiving.
        /// It runs one at a time: entries arriving during a run lead to one more run after it.
        #[arg(long, default_value = "")]
        wake: String,
    },
    /// The bridge: carry the chat into its room and back, and wake the agent.
    /// It runs in the background, and must run at least once a day,
    /// since Telegram keeps a message for 24 hours.
    Watch,
    /// Print the link of the room's agentwire session, which the agent joins with,
    /// once the user is connected.
    Session,
}

/// The messenger to connect, one of them.
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct Choice {
    /// Telegram, through a bot: a file holding the bot's token, from BotFather.
    /// It prints a link: whoever opens it and taps Start, while `messagent watch` runs, becomes the user.
    #[arg(long, value_name = "TOKEN_FILE")]
    telegram: Option<PathBuf>,
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    match Cli::parse().action {
        Action::Setup {
            messenger,
            name,
            agent,
            relay,
            wake,
        } => setup(&paths, messenger, name, agent, relay, wake)?,
        Action::Watch => bridge::watch(&paths)?,
        Action::Session => {
            let session = Session::load(&paths)?
                .context("no session yet: it starts once the user is connected")?;
            println!("{}", session.link);
        }
    }
    Ok(())
}

fn setup(
    paths: &Paths,
    choice: Choice,
    name: String,
    agent: String,
    relay: Option<String>,
    wake: String,
) -> Result<()> {
    let token = |file: &PathBuf| -> Result<String> {
        Ok(fs::read_to_string(file)
            .with_context(|| format!("can't read {}", file.display()))?
            .trim()
            .to_string())
    };
    let (messenger, next) = match choice.telegram {
        Some(file) => {
            let settings = telegram::Settings {
                token: token(&file)?,
                api: None,
            };
            let link = Telegram::open(&settings, paths)?.setup(code()?)?;
            (
                Settings::Telegram(settings),
                format!(
                    "While `messagent watch` runs, the user opens\n{link}\nand taps Start, which makes them the user."
                ),
            )
        }
        None => unreachable!("clap requires one"),
    };
    // A new user starts a new session.
    fs::remove_file(paths.session()).ok();
    Config {
        name,
        agent,
        relay,
        wake,
        messenger,
    }
    .save(paths)?;
    println!("{next}");
    Ok(())
}

/// A code no one can guess, for the start link.
fn code() -> Result<String> {
    let mut bytes = [0; 12];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
