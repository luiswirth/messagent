use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use messagent::{
    Config, Entry, Log, Party, Paths,
    messenger::{Messages, Messenger},
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
    /// Connect a Telegram bot. It prints a link: whoever opens it and taps Start,
    /// while `messagent watch` runs, becomes the user, the only one who gets through.
    Setup {
        /// A file holding the bot's token, from BotFather.
        #[arg(long)]
        token_file: PathBuf,
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
    /// The user sees 👀 on each, since the agent has now seen it.
    Read,
    /// Send the user a message.
    Send {
        text: String,
        /// The messenger's own formatting, passed to it as is, with the text unsplit.
        /// For Telegram, its parse mode: `HTML` or `MarkdownV2`,
        /// as https://core.telegram.org/bots/api#formatting-options describes.
        /// Without one, plain text.
        #[arg(long)]
        format: Option<String>,
    },
    /// React to a message of the user with an emoji, which replaces the 👀.
    /// Telegram takes only some emoji, such as 👍 ❤ 🔥 🎉 😁 🤔 👌.
    React {
        emoji: String,
        /// The message, by the `#` number `messagent read` shows; the user's latest without one.
        #[arg(long)]
        message: Option<i64>,
    },
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    match Cli::parse().action {
        Action::Setup { token_file, wake } => setup(&paths, &token_file, wake),
        Action::Watch => watch::watch(&paths),
        Action::Read => read(&paths),
        Action::Send { text, format } => send(&paths, &text, format.as_deref()),
        Action::React { emoji, message } => react(&paths, &emoji, message),
    }
}

fn setup(paths: &Paths, token_file: &PathBuf, wake: String) -> Result<()> {
    let token = fs::read_to_string(token_file)
        .with_context(|| format!("can't read {}", token_file.display()))?
        .trim()
        .to_string();
    let bot = telegram::Bot::new(&token).get_me()?;
    let username = bot.username.context("the bot has no username")?;
    let code = code()?;
    Config {
        token,
        user: None,
        code: Some(code.clone()),
        wake,
    }
    .save(paths)?;
    println!(
        "Connected the bot @{username}. While `messagent watch` runs, the user opens\n\
         https://t.me/{username}?start={code}\n\
         and taps Start, which makes them the user."
    );
    Ok(())
}

/// A code no one can guess, for the start link.
fn code() -> Result<String> {
    let mut bytes = [0; 12];
    use std::io::Read;
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read(paths: &Paths) -> Result<()> {
    let entries = Log::new(paths).read()?;
    for entry in &entries {
        println!("{}", entry.render());
    }
    // Without a connected user there is nothing to react in.
    let Ok(messenger) = messenger(paths) else {
        return Ok(());
    };
    if let Some(reactions) = messenger.reactions() {
        for message in entries.iter().filter_map(|entry| entry.message) {
            // The messages are read already, so a failed reaction only warns.
            if let Err(error) = reactions.react(message, "👀") {
                eprintln!("messagent: can't mark a message seen: {error:#}");
            }
        }
    }
    Ok(())
}

fn send(paths: &Paths, text: &str, format: Option<&str>) -> Result<()> {
    let messenger = messenger(paths)?;
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
            update: None,
            message: Some(id),
            text: message,
        }])?;
    }
    Ok(())
}

fn react(paths: &Paths, emoji: &str, message: Option<i64>) -> Result<()> {
    let messenger = messenger(paths)?;
    let reactions = messenger
        .reactions()
        .context("this messenger has no reactions")?;
    let message = match message {
        Some(message) => message,
        None => Log::new(paths)
            .entries()?
            .iter()
            .rev()
            .filter(|entry| entry.from == Party::User)
            .find_map(|entry| entry.message)
            .context("the user has sent no message yet")?,
    };
    reactions.react(message, emoji)
}

fn messenger(paths: &Paths) -> Result<impl Messenger> {
    let config = Config::load(paths)?;
    let user = config
        .user
        .context("no user is connected yet: they open the link `messagent setup` printed")?;
    Ok(Telegram {
        bot: telegram::Bot::new(&config.token),
        user,
    })
}
