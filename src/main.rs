//! messagent: message your agent in a messaging app, like a friend.

mod channel;
mod maildir;
mod markdown;
mod telegram;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

/// Links agents' inboxes and outboxes to one Telegram bot, each agent in a topic of its own in each
/// person's chat with the bot: what a person writes in an agent's topic is delivered into its inbox
/// as from `telegram:<name>`, and what the agent writes into its outbox for `telegram:<name>`
/// reaches its topic in that person's chat.
#[derive(Parser)]
#[command(version)]
struct Cli {
  /// A file holding the bot's token, from BotFather.
  #[arg(long, value_name = "FILE")]
  bot_token: PathBuf,
  /// An agent the bot serves, by its name, which its topic takes; repeated for several.
  #[arg(long, value_name = "NAME", required = true)]
  agent: Vec<String>,
  /// The directory of the agents' inboxes, each `<dir>/<agent>`; by default `~/.agents/inbox`.
  #[arg(long, value_name = "DIR")]
  inboxes: Option<PathBuf>,
  /// The directory of the agents' outboxes, each `<dir>/<agent>`; by default `~/.agents/outbox`.
  #[arg(long, value_name = "DIR")]
  outboxes: Option<PathBuf>,
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
  let agents_dir = || -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".agents"))
  };
  let inboxes = match cli.inboxes {
    Some(dir) => dir,
    None => agents_dir()?.join("inbox"),
  };
  let outboxes = match cli.outboxes {
    Some(dir) => dir,
    None => agents_dir()?.join("outbox"),
  };
  let mut agents = Vec::new();
  for name in cli.agent {
    let valid = !name.is_empty()
      && name.len() <= 128
      && name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
      return Err(anyhow!(
        "an agent's name is letters, digits, `-` and `_`, not `{name}`"
      ));
    }
    if agents.iter().any(|a: &channel::Agent| a.name == name) {
      return Err(anyhow!("the agent `{name}` is given twice"));
    }
    agents.push(channel::Agent {
      inbox: maildir::Maildir::new(inboxes.join(&name)),
      outbox: maildir::Maildir::new(outboxes.join(&name)),
      name,
    });
  }
  channel::serve(channel::Settings {
    bot: telegram::Bot::new(&cli.telegram_api, token.trim()),
    agents,
    people,
    state: cli.state,
  })
  .await
}
