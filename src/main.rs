//! messagent: message your agent in a messaging app, like a friend.

mod markdown;
mod service;
mod telegram;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// The Telegram bridge of Vibe Together: one bot, which mirrors each Telegram chat bridged,
/// private or a group, and one Vibe Together session onto each other.
#[derive(Parser)]
#[command(version)]
struct Cli {
  /// A file holding the bot's token, from BotFather.
  #[arg(long, value_name = "FILE")]
  bot_token: PathBuf,
  /// Where the linked people's tokens and the bridged chats are kept.
  #[arg(long, value_name = "DIR")]
  state: PathBuf,
  /// Vibe Together.
  #[arg(long, default_value = "https://vibetogether.potentialhedge.com")]
  server: String,
  /// An account that may link, by name; repeated for several. Anyone, where none is given.
  #[arg(long, value_name = "ACCOUNT")]
  allow: Vec<String>,
  /// The Bot API, if not Telegram's own, such as a fake one for tests.
  #[arg(long, default_value = telegram::API)]
  telegram_api: String,
}

#[tokio::main]
async fn main() -> Result<()> {
  let cli = Cli::parse();
  let token = std::fs::read_to_string(&cli.bot_token)
    .with_context(|| format!("can't read {}", cli.bot_token.display()))?;
  service::serve(service::Settings {
    bot: telegram::Bot::new(&cli.telegram_api, token.trim()),
    server: cli.server,
    state: cli.state,
    allow: cli.allow,
  })
  .await
}
