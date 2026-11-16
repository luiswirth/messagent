//! The bridge end to end: messagent's binary between a fake Telegram and a Vibe Together backend on
//! loopback, with a person who links, bridges their chat and talks with their agent.

use std::{
  collections::VecDeque,
  process::{Child, Command},
  sync::{Arc, Mutex},
  time::Duration,
};

use axum::{
  Router,
  body::Bytes,
  extract::{Path, State},
  routing::{get, post},
};
use serde_json::{Value, json};
use vibetogether_backend::{App, Config};
use vibetogether_client::{
  Client,
  protocol::{CreateSession, LogQuery, LoginKind, ReceiveQuery},
};

/// A call the bot made: its method and its body, as text.
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
}

type Shared = Arc<Mutex<Fake>>;

async fn bot_api(
  State(fake): State<Shared>,
  Path((_bot, method)): Path<(String, String)>,
  body: Bytes,
) -> String {
  let mut fake_state = fake.lock().unwrap();
  let body = String::from_utf8_lossy(&body).into_owned();
  fake_state.calls.push(Call {
    method: method.clone(),
    body: body.clone(),
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
    "getChatMember" => json!({"status": "member"}),
    "sendMessage" | "sendPhoto" | "sendDocument" => {
      fake_state.next_message += 1;
      json!({"message_id": 1000 + fake_state.next_message})
    }
    _ => json!(true),
  };
  json!({"ok": true, "result": result}).to_string()
}

impl Fake {
  fn push(fake: &Shared, update: Value) {
    let mut fake = fake.lock().unwrap();
    fake.next_update += 1;
    let mut update = update;
    update["update_id"] = json!(fake.next_update);
    fake.updates.push_back(update);
  }
}

/// Waits until the bot made a call of `method` whose body holds `needle`, after the first `from`.
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
  let calls = fake.lock().unwrap().calls.clone();
  panic!("no {method} with {needle:?}; calls: {calls:#?}");
}

fn message(id: i64, chat: i64, user: i64, text: &str) -> Value {
  json!({"message": {
    "message_id": id,
    "chat": {"id": chat, "type": "private"},
    "from": {"id": user, "is_bot": false, "first_name": "Luis", "username": "luis"},
    "text": text,
  }})
}

/// A client as the person's browser on the site, which alone approves an OTP.
fn browser(server: &str, cookie: &str) -> Client {
  use reqwest::header::{COOKIE, HeaderMap, HeaderValue, ORIGIN};
  let mut headers = HeaderMap::new();
  let cookie = format!("vibetogether.session_token={cookie}");
  headers.insert(COOKIE, HeaderValue::from_str(&cookie).unwrap());
  headers.insert(ORIGIN, HeaderValue::from_str(server).unwrap());
  headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
  let http = reqwest::Client::builder()
    .default_headers(headers)
    .build()
    .unwrap();
  Client::with_http(http, server, None)
}

struct Killed(Child);

impl Drop for Killed {
  fn drop(&mut self) {
    let _ = self.0.kill();
  }
}

fn scratch(name: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("messagent-test-{}-{name}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[tokio::test(flavor = "multi_thread")]
async fn bridges_a_private_chat() {
  let dir = scratch("bridge");
  // Vibe Together, on loopback.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let server = format!("http://{}", listener.local_addr().unwrap());
  let app = App::new(Config {
    url: server.clone(),
    database: dir.join("backend.db"),
    github_client_id: "test".into(),
    github_client_secret: "test".into(),
  })
  .await
  .unwrap();
  let router = app.router();
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  let person = |github: i64, login: &'static str| {
    let app = app.clone();
    let server = server.clone();
    async move {
      let id = app.sign_up(github, login, login, None).await.unwrap();
      let token = app
        .new_login(&id, LoginKind::Person, "laptop")
        .await
        .unwrap();
      let cookie = app
        .new_login(&id, LoginKind::Browser, "a sign-in")
        .await
        .unwrap();
      (Client::new(&server, Some(token)), browser(&server, &cookie))
    }
  };
  let (luis, luis_browser) = person(1, "luis").await;
  let (bob, bob_browser) = person(2, "bob").await;
  let machine = Client::new(
    &server,
    Some(
      app
        .new_login(&luis.me().await.unwrap().id, LoginKind::Agent, "machine")
        .await
        .unwrap(),
    ),
  );
  let agent = machine.create_agent("claude").await.unwrap();
  let agent = machine.acting_as(Some(agent.id));
  let own = luis
    .groups()
    .await
    .unwrap()
    .into_iter()
    .find(|g| g.own)
    .unwrap();
  let session = luis
    .create_session(&CreateSession {
      name: "work".into(),
      project: None,
      group: Some(own.id),
    })
    .await
    .unwrap();
  agent.join(&session.id).await.unwrap();

  // Telegram, fake.
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

  let token_file = dir.join("bot-token");
  std::fs::write(&token_file, "123:abc\n").unwrap();
  let _messagent = Killed(
    Command::new(env!("CARGO_BIN_EXE_messagent"))
      .args(["--bot-token", token_file.to_str().unwrap()])
      .args(["--state", dir.join("state").to_str().unwrap()])
      .args([
        "--server",
        &server,
        "--telegram-api",
        &telegram,
        "--allow",
        "luis",
      ])
      .spawn()
      .unwrap(),
  );
  let code = |call: &Call| -> String {
    let text: Value = serde_json::from_str(&call.body).unwrap();
    let text = text["text"].as_str().unwrap();
    let after = text.split("shows the code ").nth(1).unwrap();
    after.split('.').next().unwrap().to_string()
  };

  // Someone not allowed links: their token is revoked at once.
  Fake::push(&fake, message(1, 9, 9, "/start"));
  let asked = called(&fake, "sendMessage", "shows the code").await;
  bob_browser.approve_otp(&code(&asked)).await.unwrap();
  called(&fake, "sendMessage", "serves only luis").await;
  for _ in 0..50 {
    if bob
      .logins()
      .await
      .unwrap()
      .iter()
      .all(|l| !l.name.contains("messagent"))
    {
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  assert!(
    bob
      .logins()
      .await
      .unwrap()
      .iter()
      .all(|l| !l.name.contains("messagent"))
  );

  // Luis links, and bridges his chat into the session.
  fake.lock().unwrap().calls.clear();
  Fake::push(&fake, message(2, 7, 7, "/start"));
  let asked = called(&fake, "sendMessage", "shows the code").await;
  luis_browser.approve_otp(&code(&asked)).await.unwrap();
  called(&fake, "sendMessage", "Linked as luis.").await;
  let data = format!("b 7 {}", session.id);
  called(&fake, "sendMessage", &data).await;
  Fake::push(
    &fake,
    json!({"callback_query": {
      "id": "q",
      "from": {"id": 7, "is_bot": false, "first_name": "Luis"},
      "message": {"message_id": 1001, "chat": {"id": 7, "type": "private"}},
      "data": data,
    }}),
  );
  called(&fake, "editMessageText", "goes into work now").await;

  // What Luis writes reaches the session as him, and the agent reading it marks it seen.
  Fake::push(&fake, message(3, 7, 7, "hello @claude"));
  let mut entry = None;
  for _ in 0..100 {
    let log = agent.log(&session.id, &LogQuery::default()).await.unwrap();
    entry = log
      .entries
      .into_iter()
      .find(|e| e.text.as_deref() == Some("hello @claude"));
    if entry.is_some() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  let entry = entry.expect("Luis's message reached the session");
  assert_eq!(entry.from, "luis");
  let received = agent
    .receive(&ReceiveQuery {
      wait: Some(0),
      ..Default::default()
    })
    .await
    .unwrap();
  let last = received.items.iter().map(|i| i.n).max().unwrap();
  agent
    .receive(&ReceiveQuery {
      received: Some(last),
      read: Some(last),
      wait: Some(0),
      ..Default::default()
    })
    .await
    .unwrap();
  called(&fake, "setMessageReaction", "👀").await;

  // What the agent writes reaches the chat: markdown as Telegram's HTML, without its name.
  agent
    .send_entry(
      &session.id,
      "text/markdown",
      b"**Hi** <Luis>".to_vec(),
      None,
    )
    .await
    .unwrap();
  let sent = called(&fake, "sendMessage", "<b>Hi</b> &lt;Luis&gt;").await;
  assert!(!sent.body.contains("luis/claude"), "{}", sent.body);
  // Someone else's agent would be named; an image goes as a file, a JPEG as a photo.
  agent
    .send_entry(&session.id, "image/png", b"png".to_vec(), Some("plot.png"))
    .await
    .unwrap();
  called(&fake, "sendDocument", "plot.png").await;
  // An image for the bridge alone doesn't reach the chat, and Telegram's own format goes as given,
  // with the fields `telegram.md` names alone.
  let image = agent
    .send_entry(
      &session.id,
      "image/jpeg; ping=\"messagent\"",
      b"jpg".to_vec(),
      None,
    )
    .await
    .unwrap();
  let native = json!({"photo": image.n, "caption": "<i>A plot</i>", "parse_mode": "HTML", "reply_markup": {"x": 1}});
  agent
    .send_entry(
      &session.id,
      "application/vnd.messagent.telegram+json",
      native.to_string().into_bytes(),
      None,
    )
    .await
    .unwrap();
  let photo = called(&fake, "sendPhoto", "A plot").await;
  assert!(!photo.body.contains("reply_markup"));
  assert_eq!(
    fake
      .lock()
      .unwrap()
      .calls
      .iter()
      .filter(|c| c.method == "sendPhoto")
      .count(),
    1
  );
  // A reaction shows on Luis's message.
  agent
    .send_entry(
      &session.id,
      "application/vnd.messagent.reaction+json",
      json!({"entry": entry.n, "emoji": "👍"})
        .to_string()
        .into_bytes(),
      None,
    )
    .await
    .unwrap();
  called(&fake, "setMessageReaction", "👍").await;

  // /stop ends the bridge: what Luis writes then goes nowhere.
  Fake::push(&fake, message(4, 7, 7, "/stop"));
  called(&fake, "sendMessage", "bridge ended").await;
  Fake::push(&fake, message(5, 7, 7, "after"));
  called(&fake, "sendMessage", "goes nowhere yet").await;
  let log = agent.log(&session.id, &LogQuery::default()).await.unwrap();
  assert!(
    log
      .entries
      .iter()
      .all(|e| e.text.as_deref() != Some("after"))
  );

  // /unlink revokes Luis's token.
  Fake::push(&fake, message(6, 7, 7, "/unlink"));
  called(&fake, "sendMessage", "Unlinked").await;
  assert!(
    luis
      .logins()
      .await
      .unwrap()
      .iter()
      .all(|l| !l.name.contains("messagent"))
  );
}
