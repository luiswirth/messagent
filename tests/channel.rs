//! The channel end to end: messagent's binary between a fake Telegram and two agents' inboxes and
//! outboxes in a scratch directory, each agent in a topic of its own.

use std::{
  collections::VecDeque,
  fs,
  path::{Path, PathBuf},
  process::{Child, Command},
  sync::{Arc, Mutex},
  time::Duration,
};

use axum::{
  Router,
  body::Bytes,
  extract::{Path as UrlPath, State},
  routing::{get, post},
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Call {
  method: String,
  body: String,
}

#[derive(Default)]
struct Fake {
  updates: VecDeque<Value>,
  next_update: i64,
  calls: Vec<Call>,
  next_message: i64,
  next_topic: i64,
  /// The topics the person deleted: sending into one is refused.
  deleted: Vec<i64>,
}

type Shared = Arc<Mutex<Fake>>;

async fn bot_api(
  State(fake): State<Shared>,
  UrlPath((_bot, method)): UrlPath<(String, String)>,
  body: Bytes,
) -> String {
  let mut fake_state = fake.lock().unwrap();
  let body = String::from_utf8_lossy(&body).into_owned();
  fake_state.calls.push(Call {
    method: method.clone(),
    body,
  });
  let result = match method.as_str() {
    "getMe" => json!({"id": 1, "is_bot": true, "first_name": "bot", "username": "test_bot"}),
    "getUpdates" => {
      let updates: Vec<Value> = fake_state.updates.drain(..).collect();
      if updates.is_empty() {
        drop(fake_state);
        std::thread::sleep(Duration::from_millis(100));
      }
      json!(updates)
    }
    "getFile" => json!({"file_id": "f", "file_path": "photos/f.jpg"}),
    "createForumTopic" => {
      fake_state.next_topic += 1;
      json!({"message_thread_id": 100 + fake_state.next_topic, "name": "x", "icon_color": 0})
    }
    "sendMessage" | "sendPhoto" | "sendDocument"
      if fake_state.deleted.iter().any(|t| {
        fake_state
          .calls
          .last()
          .unwrap()
          .body
          .contains(&format!("\"message_thread_id\":{t}"))
      }) =>
    {
      return json!({"ok": false, "error_code": 400, "description": "Bad Request: message thread not found"})
        .to_string();
    }
    "sendMessage" | "sendPhoto" | "sendDocument" => {
      fake_state.next_message += 1;
      json!({"message_id": 1000 + fake_state.next_message})
    }
    _ => json!(true),
  };
  json!({"ok": true, "result": result}).to_string()
}

fn push(fake: &Shared, update: Value) {
  let mut fake = fake.lock().unwrap();
  fake.next_update += 1;
  let mut update = update;
  update["update_id"] = json!(fake.next_update);
  fake.updates.push_back(update);
}

async fn called(fake: &Shared, method: &str, needle: &str) -> Call {
  for _ in 0..200 {
    let found = fake
      .lock()
      .unwrap()
      .calls
      .iter()
      .find(|c| c.method == method && c.body.contains(needle))
      .cloned();
    if let Some(call) = found {
      return call;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  let calls: Vec<_> = fake
    .lock()
    .unwrap()
    .calls
    .iter()
    .filter(|c| c.method != "getUpdates")
    .cloned()
    .collect();
  panic!("no {method} with {needle:?}; calls: {calls:#?}");
}

fn message(id: i64, user: i64, extra: Value) -> Value {
  let mut message = json!({
    "message_id": id,
    "chat": {"id": user, "type": "private"},
    "from": {"id": user, "is_bot": false, "first_name": "Luis", "username": "luiswirth"},
  });
  for (k, v) in extra.as_object().unwrap() {
    message[k] = v.clone();
  }
  json!({ "message": message })
}

/// The files in a directory, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
  let mut files: Vec<_> = fs::read_dir(dir)
    .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
    .unwrap_or_default();
  files.sort();
  files
}

async fn until(what: &str, check: impl Fn() -> bool) {
  for _ in 0..200 {
    if check() {
      return;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  panic!("never: {what}");
}

/// Writes an outbox item as an agent does: into `tmp/`, then linked into `new/`.
fn write_outbox(outbox: &Path, name: &str, content: &[u8]) {
  fs::create_dir_all(outbox.join("tmp")).unwrap();
  fs::create_dir_all(outbox.join("new")).unwrap();
  let tmp = outbox.join("tmp").join(name);
  fs::write(&tmp, content).unwrap();
  fs::hard_link(&tmp, outbox.join("new").join(name)).unwrap();
  fs::remove_file(tmp).unwrap();
}

struct Killed(Child);

impl Drop for Killed {
  fn drop(&mut self) {
    let _ = self.0.kill();
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn carries_both_ways() {
  let dir = std::env::temp_dir().join(format!("messagent-test-{}", std::process::id()));
  let _ = fs::remove_dir_all(&dir);
  fs::create_dir_all(&dir).unwrap();
  // The agents `lead`, the first topic, 101, and `news`, the second, 102.
  let (inbox, outbox) = (dir.join("inbox/lead"), dir.join("outbox/lead"));
  let (news_inbox, news_outbox) = (dir.join("inbox/news"), dir.join("outbox/news"));

  let fake: Shared = Default::default();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let telegram = format!("http://{}", listener.local_addr().unwrap());
  let router = Router::new()
    .route("/{bot}/{method}", post(bot_api))
    .route(
      "/file/{bot}/{*path}",
      get(|| async { vec![0xFFu8, 0xD8, 0xFF] }),
    )
    .with_state(fake.clone());
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

  let token = dir.join("bot-token");
  fs::write(&token, "123:abc\n").unwrap();
  let _messagent = Killed(
    Command::new(env!("CARGO_BIN_EXE_messagent"))
      .args(["--bot-token", token.to_str().unwrap()])
      .args(["--agent", "lead", "--agent", "news"])
      .args(["--inboxes", dir.join("inbox").to_str().unwrap()])
      .args(["--outboxes", dir.join("outbox").to_str().unwrap()])
      .args(["--state", dir.join("state").to_str().unwrap()])
      .args(["--person", "7=luis", "--telegram-api", &telegram])
      .spawn()
      .unwrap(),
  );

  // Each agent's topic in Luis's chat is created at the start.
  called(&fake, "createForumTopic", "\"name\":\"lead\"").await;
  called(&fake, "createForumTopic", "\"name\":\"news\"").await;
  // A stranger is ignored.
  push(&fake, message(1, 8, json!({"text": "hi"})));
  // A message outside the topics is told where the agents are, and reaches no one.
  push(&fake, message(9, 7, json!({"text": "anyone?"})));
  called(
    &fake,
    "sendMessage",
    "Each agent has a topic of its own here: lead, news.",
  )
  .await;
  // Luis's message in the news topic reaches the news agent alone.
  push(
    &fake,
    message(
      10,
      7,
      json!({"text": "what's new?", "message_thread_id": 102}),
    ),
  );
  until("a news item", || !files(&news_inbox.join("new")).is_empty()).await;
  let text = fs::read_to_string(&files(&news_inbox.join("new"))[0]).unwrap();
  assert!(text.contains("what's new?"), "{text}");
  assert!(text.contains(news_outbox.to_str().unwrap()), "{text}");
  // The news agent's answer goes into its topic.
  write_outbox(
    &news_outbox,
    "20261116T170000.000000Z.telegram%3Aluis.md",
    b"Nothing yet.",
  );
  let sent = called(&fake, "sendMessage", "Nothing yet.").await;
  assert!(sent.body.contains("\"message_thread_id\":102"), "{sent:?}");
  // Luis's message in the lead's topic reaches the lead's inbox as his, with what it replies to,
  // and rings.
  push(
    &fake,
    message(
      2,
      7,
      json!({"text": "how is it going?", "message_thread_id": 101, "reply_to_message": {
        "message_id": 1, "chat": {"id": 7, "type": "private"}, "text": "Tests pass."}}),
    ),
  );
  let new = inbox.join("new");
  until("an inbox item", || !files(&new).is_empty()).await;
  let items = files(&new);
  assert_eq!(items.len(), 1, "{items:?}");
  let name = items[0].file_name().unwrap().to_string_lossy().into_owned();
  assert!(name.ends_with(".telegram%3Aluis.txt"), "{name}");
  let text = fs::read_to_string(&items[0]).unwrap();
  assert!(
    text.contains("from telegram:luis (@luiswirth), replying to: \"Tests pass.\""),
    "{text}"
  );
  assert!(text.contains("how is it going?"), "{text}");
  assert!(inbox.join("wake").exists());
  assert!(
    fake
      .lock()
      .unwrap()
      .calls
      .iter()
      .all(|c| !c.body.contains("\"chat_id\":8"))
  );

  // The agent takes it in: the message gets 👀.
  fs::rename(&items[0], inbox.join("cur").join(&name)).unwrap();
  called(&fake, "setMessageReaction", "👀").await;

  // A photo arrives as an image item, named by the text that follows it.
  push(
    &fake,
    message(
      3,
      7,
      json!({"photo": [{"file_id": "p"}], "caption": "look", "message_thread_id": 101}),
    ),
  );
  until("two more items", || files(&new).len() == 2).await;
  let items = files(&new);
  assert!(items[0].to_string_lossy().ends_with(".telegram%3Aluis.jpg"));
  let text = fs::read_to_string(&items[1]).unwrap();
  assert!(text.contains("[a photo: the inbox item"), "{text}");

  // The agent answers through its outbox: markdown as Telegram's HTML, an image as a file.
  write_outbox(
    &outbox,
    "20261116T170000.000001Z.telegram%3Aluis.md",
    b"**Fine**, thanks.",
  );
  let sent = called(&fake, "sendMessage", "<b>Fine</b>, thanks.").await;
  assert!(sent.body.contains("\"message_thread_id\":101"), "{sent:?}");
  write_outbox(
    &outbox,
    "20261116T170000.000002Z.telegram%3Aluis.png",
    b"png",
  );
  called(&fake, "sendDocument", "file.png").await;
  until("the outbox items taken", || {
    files(&outbox.join("new")).is_empty()
  })
  .await;
  assert_eq!(files(&outbox.join("cur")).len(), 2);
  // An item for someone else stays where it is.
  write_outbox(
    &outbox,
    "20261116T170000.000003Z.discord%3Aluis.md",
    b"elsewhere",
  );
  // One for someone the bot doesn't know comes back as a notice.
  write_outbox(
    &outbox,
    "20261116T170000.000004Z.telegram%3Abob.md",
    b"hi bob",
  );
  until("a notice", || {
    files(&new)
      .iter()
      .any(|f| f.to_string_lossy().ends_with(".messagent.txt"))
  })
  .await;
  assert!(
    outbox
      .join("new/20261116T170000.000003Z.discord%3Aluis.md")
      .exists()
  );
  // Luis deleted the lead's topic: the next answer creates it anew and goes there.
  fake.lock().unwrap().deleted.push(101);
  write_outbox(
    &outbox,
    "20261116T170000.000005Z.telegram%3Aluis.md",
    b"Still here.",
  );
  let sent = called(&fake, "sendMessage", "\"message_thread_id\":103").await;
  assert!(sent.body.contains("Still here."), "{sent:?}");
  let _ = fs::remove_dir_all(&dir);
}
