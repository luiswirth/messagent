use jiff::{SignedDuration, Timestamp};
use messagent::{
    Config, Paths,
    bridge::Waker,
    messenger::{self, Arrived, split},
    telegram::{MESSAGE_LIMIT, Settings, State, Update, take},
};
use serde_json::json;

const USER: i64 = 42;

fn paths() -> Paths {
    let base = std::env::temp_dir().join(format!(
        "messagent-test-{}-{}",
        std::process::id(),
        Timestamp::now().as_nanosecond()
    ));
    Paths {
        config: base.join("config"),
        state: base.join("state"),
    }
}

fn connected() -> State {
    State {
        user: Some(USER),
        ..State::default()
    }
}

fn update(id: i64, from: i64, chat: &str, message: serde_json::Value) -> Update {
    let mut message = message;
    message["message_id"] = json!(id);
    message["date"] = json!(1_793_000_000);
    message["chat"] = json!({ "id": from, "type": chat });
    message["from"] = json!({ "id": from, "first_name": "Luis", "is_bot": false });
    serde_json::from_value(json!({ "update_id": id, "message": message })).unwrap()
}

fn text(id: i64, from: i64, text: &str) -> Update {
    update(id, from, "private", json!({ "text": text }))
}

/// The texts of what arrived, each marked by its kind.
fn texts(arrived: &[Arrived]) -> Vec<String> {
    arrived
        .iter()
        .map(|arrived| match arrived {
            Arrived::Message { text, .. } => text.clone(),
            Arrived::Notice(text) => format!("notice: {text}"),
            Arrived::Stranger(stranger) => format!("stranger {}: {}", stranger.id, stranger.text),
        })
        .collect()
}

#[test]
fn only_the_users_private_messages_get_through_and_strangers_are_kept_apart() {
    let mut state = connected();
    let updates = vec![
        text(1, USER, "mine"),
        text(2, 7, "a stranger's"),
        update(3, USER, "group", json!({ "text": "in a group" })),
        update(
            4,
            USER,
            "private",
            json!({ "photo": [], "caption": "look" }),
        ),
    ];
    let fetched = take(&mut state, Timestamp::now(), updates);
    assert_eq!(
        texts(&fetched.arrived),
        [
            "mine",
            "stranger 7: a stranger's",
            "look\n[photo: messagent carries only text so far]"
        ]
    );
    assert!(matches!(&fetched.arrived[0], Arrived::Message { id, .. } if id == "1"));
    assert_eq!(state.offset, 5);
}

#[test]
fn an_update_already_fetched_is_skipped() {
    let mut state = State {
        offset: 2,
        ..connected()
    };
    let fetched = take(
        &mut state,
        Timestamp::now(),
        vec![text(1, USER, "old"), text(2, USER, "new")],
    );
    assert_eq!(texts(&fetched.arrived), ["new"]);
}

#[test]
fn a_gap_over_a_day_becomes_a_notice() {
    let now = Timestamp::now();
    let mut state = State {
        fetched: Some(now - SignedDuration::from_hours(23)),
        ..connected()
    };
    assert!(take(&mut state, now, vec![]).arrived.is_empty());
    state.fetched = Some(now - SignedDuration::from_hours(25));
    let fetched = take(&mut state, now, vec![text(1, USER, "back")]);
    assert!(matches!(&fetched.arrived[0], Arrived::Notice(text) if text.contains("may be lost")));
    assert_eq!(texts(&fetched.arrived)[1], "back");
    assert_eq!(state.fetched, Some(now));
}

#[test]
fn the_start_link_connects_the_user_once() {
    let mut state = State {
        code: Some("secret".into()),
        ..State::default()
    };
    let fetched = take(
        &mut state,
        Timestamp::now(),
        vec![
            text(1, 7, "/start wrong"),
            text(2, USER, "/start secret"),
            text(3, 7, "/start secret"),
            text(4, USER, "hi"),
        ],
    );
    assert_eq!(fetched.connected, Some(USER));
    assert_eq!(state.user, Some(USER));
    assert_eq!(state.code, None);
    assert_eq!(
        texts(&fetched.arrived),
        [
            "stranger 7: /start wrong",
            "notice: Luis connected as the user.",
            "stranger 7: /start secret",
            "hi"
        ]
    );
}

#[test]
fn split_keeps_messages_within_the_limit() {
    assert!(split("", MESSAGE_LIMIT).is_empty());
    assert_eq!(split("short", MESSAGE_LIMIT), ["short"]);
    let long = "a".repeat(MESSAGE_LIMIT + 10);
    let parts = split(&long, MESSAGE_LIMIT);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts.concat(), long);
    let lines = format!("{}\n{}", "a".repeat(3000), "b".repeat(3000));
    assert_eq!(
        split(&lines, MESSAGE_LIMIT),
        ["a".repeat(3000), "b".repeat(3000)]
    );
    let wide = "😀".repeat(MESSAGE_LIMIT);
    assert!(
        split(&wide, MESSAGE_LIMIT)
            .iter()
            .all(|part| part.encode_utf16().count() <= MESSAGE_LIMIT)
    );
}

#[test]
fn config_survives_a_round_trip() {
    let paths = paths();
    let config = Config {
        name: "luis".into(),
        relay: Some("http://localhost:8791".into()),
        wake: "wake".into(),
        messenger: messenger::Settings::Telegram(Settings {
            token: "token".into(),
            api: None,
        }),
    };
    config.save(&paths).unwrap();
    assert_eq!(Config::load(&paths).unwrap(), config);
}

#[test]
fn a_failing_wake_is_reported_once_per_streak() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let flag = paths().state;
    std::fs::create_dir_all(&flag).unwrap();
    let flag = flag.join("fail");
    let reported = Arc::new(AtomicUsize::new(0));
    let counter = reported.clone();
    let waker = Waker::spawn(format!("test ! -e {}", flag.display()), move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let settle = || std::thread::sleep(std::time::Duration::from_millis(200));
    std::fs::write(&flag, "").unwrap();
    for _ in 0..3 {
        waker.wake();
        settle();
    }
    assert_eq!(reported.load(Ordering::SeqCst), 1);
    std::fs::remove_file(&flag).unwrap();
    waker.wake();
    settle();
    std::fs::write(&flag, "").unwrap();
    waker.wake();
    settle();
    assert_eq!(reported.load(Ordering::SeqCst), 2);
}
