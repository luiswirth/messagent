use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use messagent::{Config, Entry, Log, Party, Paths, telegram, watch};

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
    Read,
    /// Send the user a message.
    Send { text: String },
}

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    match Cli::parse().action {
        Action::Setup { token_file, wake } => setup(&paths, &token_file, wake),
        Action::Watch => watch::watch(&paths),
        Action::Read => read(&paths),
        Action::Send { text } => send(&paths, &text),
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
    for entry in Log::new(paths).read()? {
        println!("{}", entry.render());
    }
    Ok(())
}

fn send(paths: &Paths, text: &str) -> Result<()> {
    let config = Config::load(paths)?;
    let user = config
        .user
        .context("no user is connected yet: they open the link `messagent setup` printed")?;
    let bot = telegram::Bot::new(&config.token);
    let log = Log::new(paths);
    for message in telegram::split(text) {
        bot.send_message(user, &message)?;
        log.append(&[Entry {
            time: Timestamp::now(),
            from: Party::Agent,
            name: None,
            update: None,
            text: message,
        }])?;
    }
    Ok(())
}
