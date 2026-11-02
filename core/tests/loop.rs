use jiff::{SignedDuration, Timestamp};
use messagent::{
    Config, Entry, Log, Party, Paths,
    telegram::{MESSAGE_LIMIT, Settings, State, Update, split, take},
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

fn entry(from: Party, text: &str) -> Entry {
    Entry {
        time: Timestamp::now(),
        from,
        name: None,
        message: None,
        text: text.into(),
    }
}

#[test]
fn read_returns_each_entry_once_without_the_agents() {
    let log = Log::new(&paths());
    assert!(log.read().unwrap().is_empty());
    log.append(&[entry(Party::User, "hi"), entry(Party::Agent, "hello")])
        .unwrap();
    let read: Vec<_> = log.read().unwrap().into_iter().map(|e| e.text).collect();
    assert_eq!(read, ["hi"]);
    assert!(log.read().unwrap().is_empty());
    log.append(&[entry(Party::Messagent, "gap")]).unwrap();
    assert_eq!(log.read().unwrap()[0].text, "gap");
    assert_eq!(log.entries().unwrap().len(), 3);
}

#[test]
fn only_the_users_private_messages_get_through() {
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
    let texts: Vec<_> = fetched.entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        texts,
        ["mine", "look\n[photo: messagent carries only text so far]"]
    );
    assert_eq!(fetched.entries[0].name.as_deref(), Some("Luis"));
    assert_eq!(fetched.entries[0].message.as_deref(), Some("1"));
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
    assert_eq!(fetched.entries.len(), 1);
    assert_eq!(fetched.entries[0].text, "new");
}

#[test]
fn a_gap_over_a_day_becomes_a_notice() {
    let now = Timestamp::now();
    let mut state = State {
        fetched: Some(now - SignedDuration::from_hours(23)),
        ..connected()
    };
    assert!(take(&mut state, now, vec![]).entries.is_empty());
    state.fetched = Some(now - SignedDuration::from_hours(25));
    let fetched = take(&mut state, now, vec![text(1, USER, "back")]);
    assert_eq!(fetched.entries[0].from, Party::Messagent);
    assert_eq!(fetched.entries[1].text, "back");
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
    let texts: Vec<_> = fetched.entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["Luis connected as the user.", "hi"]);
}

#[test]
fn split_keeps_messages_within_the_limit() {
    assert!(split("").is_empty());
    assert_eq!(split("short"), ["short"]);
    let long = "a".repeat(MESSAGE_LIMIT + 10);
    let parts = split(&long);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts.concat(), long);
    let lines = format!("{}\n{}", "a".repeat(3000), "b".repeat(3000));
    assert_eq!(split(&lines), ["a".repeat(3000), "b".repeat(3000)]);
    let wide = "😀".repeat(MESSAGE_LIMIT);
    assert!(
        split(&wide)
            .iter()
            .all(|part| part.encode_utf16().count() <= MESSAGE_LIMIT)
    );
}

#[test]
fn config_survives_a_round_trip() {
    let paths = paths();
    let config = Config {
        wake: "wake".into(),
        telegram: Some(Settings {
            token: "token".into(),
        }),
    };
    config.save(&paths).unwrap();
    assert_eq!(Config::load(&paths).unwrap(), config);
}
