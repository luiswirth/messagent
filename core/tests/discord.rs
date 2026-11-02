use messagent::{
    Config, Party, Paths,
    discord::{Settings, State, take},
    messenger,
};
use serde_json::json;
use twilight_model::{channel::Message, id::Id};

const USER: u64 = 42;
const CHAT: u64 = 7;

fn message(id: u64, channel: u64, author: u64, extra: serde_json::Value) -> Message {
    let mut message = json!({
        "id": id.to_string(),
        "channel_id": channel.to_string(),
        "author": {
            "id": author.to_string(),
            "username": "luis",
            "global_name": "Luis",
            "discriminator": "0",
            "avatar": null,
        },
        "content": "",
        "timestamp": "2026-11-02T16:00:00.000000+00:00",
        "edited_timestamp": null,
        "tts": false,
        "mention_everyone": false,
        "mentions": [],
        "mention_roles": [],
        "attachments": [],
        "embeds": [],
        "pinned": false,
        "type": 0,
    });
    for (key, value) in extra.as_object().unwrap() {
        message[key] = value.clone();
    }
    serde_json::from_value(message).unwrap()
}

fn text(id: u64, channel: u64, author: u64, text: &str) -> Message {
    message(id, channel, author, json!({ "content": text }))
}

fn connected() -> State {
    State {
        user: Some(Id::new(USER)),
        channel: Some(Id::new(CHAT)),
        guild: Some(Id::new(1)),
        last: None,
    }
}

#[test]
fn only_the_users_messages_in_the_dm_get_through_each_once() {
    let mut state = connected();
    let messages = vec![
        text(13, CHAT, USER, "second"),
        text(11, CHAT, USER, "first"),
        text(12, CHAT, 99, "the bot's own"),
        text(14, 8, USER, "elsewhere"),
        message(
            15,
            CHAT,
            USER,
            json!({
                "content": "look",
                "attachments": [{
                    "id": "1", "filename": "a.png", "size": 1,
                    "url": "u", "proxy_url": "p", "content_type": "image/png",
                }],
            }),
        ),
    ];
    let entries = take(&mut state, messages);
    let texts: Vec<_> = entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "first",
            "second",
            "look\n[image: messagent carries only text so far]"
        ]
    );
    assert_eq!(entries[0].from, Party::User);
    assert_eq!(entries[0].name.as_deref(), Some("Luis"));
    assert_eq!(entries[0].message.as_deref(), Some("11"));
    assert_eq!(state.last, Some(Id::new(15)));
    assert!(take(&mut state, vec![text(13, CHAT, USER, "second")]).is_empty());
}

#[test]
fn nothing_gets_through_before_the_dm_is_open() {
    let mut state = State {
        user: Some(Id::new(USER)),
        ..State::default()
    };
    assert!(take(&mut state, vec![text(1, CHAT, USER, "hi")]).is_empty());
}

#[test]
fn config_survives_a_round_trip() {
    let base = std::env::temp_dir().join(format!("messagent-discord-test-{}", std::process::id()));
    let paths = Paths {
        config: base.join("config"),
        state: base.join("state"),
    };
    let config = Config {
        wake: "wake".into(),
        speech: None,
        messenger: messenger::Settings::Discord(Settings {
            token: "token".into(),
        }),
    };
    config.save(&paths).unwrap();
    assert!(
        std::fs::read_to_string(paths.config_file())
            .unwrap()
            .contains("[discord]")
    );
    assert_eq!(Config::load(&paths).unwrap(), config);
}
