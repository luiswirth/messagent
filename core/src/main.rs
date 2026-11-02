use std::{fs, io::Read, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use messagent::{
    Config, Entry, Log, Party, Paths,
    discord::{self, Discord},
    messenger::{self, Settings},
    telegram::{self, Telegram},
    watch,
};

/// The user's messenger chat with their agent.
/// Every message of the chat is kept in a log on this machine.
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
        /// A shell command `messagent watch` runs after new messages arrive, to wake the agent.
        /// It runs one at a time: messages arriving during a run lead to one more run after it.
        #[arg(long, default_value = "")]
        wake: String,
    },
    /// Fetch the messages and write them to the log, then run the wake command.
    /// It runs in the background, and must run at least once a day,
    /// since Telegram keeps a message for 24 hours.
    Watch,
    /// Print the messages not read yet, and mark them read.
    /// Where the messenger has reactions, the user sees 👀 on each, since the agent has now seen it.
    Read,
    /// Send the user a message.
    Send {
        text: String,
        /// The messenger's own formatting, passed to it as is, with the text unsplit.
        /// For Telegram, its parse mode: `HTML` or `MarkdownV2`,
        /// as https://core.telegram.org/bots/api#formatting-options describes.
        /// Discord takes none: its one format, Markdown, is always on.
        /// Without one, plain text.
        #[arg(long)]
        format: Option<String>,
    },
    /// React to a message of the user with an emoji, which replaces the agent's earlier one, such as the 👀.
    /// Telegram takes only some emoji, such as 👍 ❤ 🔥 🎉 😁 🤔 👌.
    React {
        emoji: String,
        /// The message, by the `#` id `messagent read` shows; the user's latest without one.
        #[arg(long)]
        message: Option<String>,
    },
}

/// The messenger to connect, one of them.
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct Choice {
    /// Telegram, through a bot: a file holding the bot's token, from BotFather.
    /// It prints a link: whoever opens it and taps Start, while `messagent watch` runs, becomes the user.
    #[arg(long, value_name = "TOKEN_FILE")]
    telegram: Option<PathBuf>,
    /// Discord, through a bot: a file holding the bot's token, from the Developer Portal.
    /// The owner of the bot's application is the user. It prints a link that adds the bot to a server:
    /// once the bot shares a server with the user, while `messagent watch` runs, it opens a DM with them.
    #[arg(long, value_name = "TOKEN_FILE")]
    discord: Option<PathBuf>,
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    match Cli::parse().action {
        Action::Setup { messenger, wake } => setup(&paths, messenger, wake),
        Action::Watch => watch::watch(&paths),
        Action::Read => read(&paths),
        Action::Send { text, format } => send(&paths, &text, format.as_deref()),
        Action::React { emoji, message } => react(&paths, &emoji, message),
    }
}

fn setup(paths: &Paths, choice: Choice, wake: String) -> Result<()> {
    let token = |file: &PathBuf| -> Result<String> {
        Ok(fs::read_to_string(file)
            .with_context(|| format!("can't read {}", file.display()))?
            .trim()
            .to_string())
    };
    let (messenger, next) = match (choice.telegram, choice.discord) {
        (Some(file), _) => {
            let settings = telegram::Settings {
                token: token(&file)?,
            };
            let link = Telegram::open(&settings, paths)?.setup(code()?)?;
            (
                Settings::Telegram(settings),
                format!(
                    "While `messagent watch` runs, the user opens\n{link}\nand taps Start, which makes them the user."
                ),
            )
        }
        (None, Some(file)) => {
            let settings = discord::Settings {
                token: token(&file)?,
            };
            let (link, owner) = Discord::open(&settings, paths)?.setup()?;
            (
                Settings::Discord(settings),
                format!(
                    "The user is {owner}, the owner of the bot's application. They add the bot to a server of theirs with\n{link}\n\
                     While `messagent watch` runs, the bot then opens a DM with them."
                ),
            )
        }
        (None, None) => unreachable!("clap requires one"),
    };
    Config { wake, messenger }.save(paths)?;
    println!("{next}");
    Ok(())
}

/// A code no one can guess, for the start link.
fn code() -> Result<String> {
    let mut bytes = [0; 12];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read(paths: &Paths) -> Result<()> {
    let entries = Log::new(paths).read()?;
    for entry in &entries {
        println!("{}", entry.render());
    }
    // The messages are read already, so a messenger that fails here only warns.
    let messenger =
        Config::load(paths).and_then(|config| messenger::open(&config.messenger, paths));
    if let Err(error) = &messenger {
        eprintln!("messagent: {error:#}");
    }
    if let Some(reactions) = messenger.as_ref().ok().and_then(|m| m.reactions()) {
        for message in entries.iter().filter_map(|entry| entry.message.as_deref()) {
            // The messages are read already, so a failed reaction only warns.
            if let Err(error) = reactions.react(message, "👀") {
                eprintln!("messagent: can't mark a message seen: {error:#}");
            }
        }
    }
    Ok(())
}

fn send(paths: &Paths, text: &str, format: Option<&str>) -> Result<()> {
    let messenger = messenger::open(&Config::load(paths)?.messenger, paths)?;
    let log = Log::new(paths);
    // Formatted text is the messenger's to parse, so only plain text is split.
    let messages = match format {
        Some(_) => vec![text.to_string()],
        None => messenger.split(text),
    };
    for message in messages {
        let id = messenger.send(&message, format)?;
        log.append(&[Entry {
            time: Timestamp::now(),
            from: Party::Agent,
            name: None,
            message: Some(id),
            text: message,
        }])?;
    }
    Ok(())
}

fn react(paths: &Paths, emoji: &str, message: Option<String>) -> Result<()> {
    let messenger = messenger::open(&Config::load(paths)?.messenger, paths)?;
    let reactions = messenger
        .reactions()
        .context("this messenger has no reactions")?;
    let message = match message {
        Some(message) => message,
        None => Log::new(paths)
            .entries()?
            .into_iter()
            .rev()
            .filter(|entry| entry.from == Party::User)
            .find_map(|entry| entry.message)
            .context("the user has sent no message yet")?,
    };
    reactions.react(&message, emoji)
}
