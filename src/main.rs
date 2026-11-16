//! messagent: message your agent in a messaging app, like a friend.

mod channel;
mod maildir;
mod markdown;
mod telegram;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

/// Links one agent's inbox and outbox to a Telegram bot of its own: what its people write to the
/// bot is delivered into the inbox as from `telegram/<name>`, and what the agent writes into its
/// outbox for `telegram/<name>` reaches that person's chat.
#[derive(Parser)]
#[command(version)]
struct Cli {
  /// A file holding the bot's token, from BotFather.
  #[arg(long, value_name = "FILE")]
  bot_token: PathBuf,
  /// The agent's inbox.
  #[arg(long, env = "AGENT_INBOX", value_name = "DIR")]
  inbox: PathBuf,
  /// The agent's outbox.
  #[arg(long, env = "AGENT_OUTBOX", value_name = "DIR")]
  outbox: PathBuf,
  /// A person who may write to the bot, as `<telegram id>=<name>`, such as `9118774471=luis`;
  /// repeated for several. Everyone else is ignored.
  #[arg(long, value_name = "ID=NAME", required = true)]
  person: Vec<String>,
  /// Where its own state is kept.
  #[arg(long, value_name = "DIR")]
  state: PathBuf,
  /// The Bot API, if not Telegram's own, such as a fake one for tests.
  #[arg(long, default_value = telegram::API)]
  telegram_api: String,
}

#[tokio::main]
async fn main() -> Result<()> {
  let cli = Cli::parse();
  let token = std::fs::read_to_string(&cli.bot_token)
    .with_context(|| format!("can't read {}", cli.bot_token.display()))?;
  let people = cli
    .person
    .iter()
    .map(|p| {
      let (id, name) = p
        .split_once('=')
        .ok_or_else(|| anyhow!("--person is `<telegram id>=<name>`, not `{p}`"))?;
      let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
      if !valid {
        return Err(anyhow!("a name is letters, digits and `-`, not `{name}`"));
      }
      Ok((
        id.parse().context("a Telegram id is a number")?,
        name.to_string(),
      ))
    })
    .collect::<Result<_>>()?;
  channel::serve(channel::Settings {
    bot: telegram::Bot::new(&cli.telegram_api, token.trim()),
    inbox: maildir::Maildir::new(cli.inbox),
    outbox: maildir::Maildir::new(cli.outbox),
    people,
    state: cli.state,
  })
  .await
}
