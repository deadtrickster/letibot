//! The chat layer's policy, tested where it lives. The head's parity test says this layer's stream
//! draws what the native adapter draws; these say the stream is the one the layer describes, and they
//! run natively, because none of the logic below knows a wasm ABI exists.

use super::*;
use letibot_plugin::pane::{ChatAlias, ChatChannel, ChatMember, ChatVerb};
use serde_json::{Value, json};

/// 2026-11-03 19:56:34 UTC. The tests are written in UTC (`off = 0`) so a day and an hour are
/// arithmetic rather than the zone of whichever machine runs them.
const NOW: u64 = 1_793_735_794_000;

/// A message with the wire's own fields and nothing else.
fn msg(seq: u64, ts: u64, author: &str, text: &str) -> ChatMessage {
    ChatMessage {
        seq,
        ts,
        room: "host".into(),
        author: author.into(),
        kind: "session".into(),
        text: text.into(),
        ..ChatMessage::default()
    }
}

/// One minute after another message, so a run of them is a burst and not a coincidence.
fn after(prev: u64) -> u64 {
    prev + 60_000
}

/// The feed a head hands this layer: a channel, its window, and what the head knows about it.
fn feed(messages: Vec<ChatMessage>) -> Chat {
    Chat {
        selected: Some("host".into()),
        heard: true,
        messages,
        ..Chat::default()
    }
}

/// The stream as the host parses it - the same JSON, read back as a person reads a screen.
fn drawn(chat: &Chat) -> Value {
    serde_json::to_value(stream(chat, NOW, 0)).expect("the stream serialises")
}

/// The ids of the entries, in order - `[today]` / `[unread]` for a divider.
fn ids(stream: &Value) -> Vec<String> {
    stream["entries"]
        .as_array()
        .map(|e| {
            e.iter()
                .map(|entry| match entry["divider"].as_str() {
                    Some(kind) => format!("[{kind}]"),
                    None => entry["id"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One entry, by the `seq` it names.
fn entry(stream: &Value, seq: u64) -> &Value {
    stream["entries"]
        .as_array()
        .and_then(|e| e.iter().find(|entry| entry["id"] == json!(seq.to_string())))
        .unwrap_or(&Value::Null)
}

// ─────────────────────────────── the stream ───────────────────────────────

/// A run. Two messages from one speaker inside ten minutes are one burst: the second carries no
/// header and no facts, because what it has to say is its body.
#[test]
fn two_messages_from_one_author_within_ten_minutes_share_a_header() {
    let one = NOW - 120_000;
    let two = after(one);
    let chat = feed(vec![
        msg(1, one, "fix-room-push", "the first line"),
        msg(2, two, "fix-room-push", "the second line"),
    ]);
    let s = drawn(&chat);
    assert!(entry(&s, 1)["head"].is_array(), "{s}");
    assert_eq!(
        entry(&s, 2).get("head"),
        None,
        "a continuation has no header: {s}"
    );
    assert_eq!(entry(&s, 2).get("facts"), None, "and no facts: {s}");
    // The second still says when it was, because its minute is not the first's.
    assert_eq!(entry(&s, 2)["time"], json!("19:55"), "{s}");

    // Ten minutes later the run is over and a header comes back.
    let three = two + RUN_MS;
    let chat = feed(vec![
        msg(1, one, "fix-room-push", "the first line"),
        msg(2, three, "fix-room-push", "much later"),
    ]);
    let s = drawn(&chat);
    assert!(entry(&s, 2)["head"].is_array(), "a new run: {s}");

    // And so is a message that names the reader: it is news, not a continuation.
    let chat = feed(vec![
        msg(1, one, "fix-room-push", "the first line"),
        msg(2, after(one), "fix-room-push", "hey @operator look"),
    ]);
    let s = drawn(&chat);
    assert!(entry(&s, 2)["head"].is_array(), "a mention ends a run: {s}");
}

/// A hue. The speaker's colour is its handle's, so a rename keeps it and two speakers do not share
/// one by accident - and the reader is never a hue.
#[test]
fn the_same_handle_always_gets_the_same_hue() {
    let chat = feed(vec![
        msg(1, NOW - 60_000, "fix-room-push", "one"),
        msg(2, NOW - 30_000, "s-other", "two"),
        msg(3, NOW, "fix-room-push", "three"),
    ]);
    let s = drawn(&chat);
    let first = &entry(&s, 1)["head"][0];
    let third = &entry(&s, 3)["head"][0];
    assert_eq!(first["role"], third["role"], "one handle, one colour: {s}");
    assert_eq!(
        first["role"],
        json!("hue:3"),
        "FNV-1a of the handle mod 8: {s}"
    );
    assert_eq!(entry(&s, 2)["head"][0]["role"], json!("hue:5"), "{s}");

    // The reader is `strong`, not a hue: `you` is not a colour.
    let chat = feed(vec![msg(1, NOW, ME, "mine")]);
    let s = drawn(&chat);
    assert_eq!(entry(&s, 1)["head"][0]["role"], json!("strong"), "{s}");
}

/// A mention. A session addressed by its handle reads as the name the head has for it, in that
/// session's own hue; the reader's own handle is the one mention that is not quiet.
#[test]
fn a_session_mention_is_mapped_to_its_title() {
    let mut chat = feed(vec![msg(1, NOW, "s-a", "ping @s-b and @operator about it")]);
    chat.names.insert("s-b".into(), "fix-room-push".into());
    let s = drawn(&chat);
    assert_eq!(
        s["mentions"]["@s-b"],
        json!({"text": "@fix-room-push", "hue": 5}),
        "{s}"
    );
    assert_eq!(s["mentions"]["@operator"], json!({"me": true}), "{s}");
    // A handle nobody named is not in the map: the token is already its own word.
    let chat = feed(vec![msg(1, NOW, "s-a", "ping @s-unknown")]);
    assert!(drawn(&chat).get("mentions").is_none(), "{}", drawn(&chat));
}

/// The unread mark. The divider sits before the first message newer than `opened_seen`, and a
/// channel opened with nothing read yet still puts it above the first message.
#[test]
fn the_unread_divider_precedes_the_first_unseen_message() {
    let chat = Chat {
        opened_seen: Some(2),
        ..feed(vec![
            msg(1, NOW - 180_000, "s-a", "read"),
            msg(2, NOW - 120_000, "s-a", "also read"),
            msg(3, NOW - 60_000, "s-b", "new"),
            msg(4, NOW, "s-b", "newer"),
        ])
    };
    let s = drawn(&chat);
    assert_eq!(
        ids(&s),
        vec!["[today]", "1", "2", "[unread]", "3", "4"],
        "the mark is before the first unseen message: {s}"
    );

    // Opened with nothing read yet: the mark goes above the first message.
    let chat = Chat {
        opened_seen: Some(0),
        ..feed(vec![msg(1, NOW, "s-a", "the first thing said")])
    };
    assert_eq!(ids(&drawn(&chat)), vec!["[today]", "[unread]", "1"]);

    // Never opened by this head: no mark at all, which is not the same fact.
    let chat = feed(vec![msg(1, NOW, "s-a", "the first thing said")]);
    assert_eq!(ids(&drawn(&chat)), vec!["[today]", "1"]);
}

/// The day. A divider where the calendar turns, named the way a person reads a date: `today`,
/// `yesterday`, the month and day inside this year, and the year too when the talk is older.
#[test]
fn a_day_divider_names_the_day_the_way_a_person_reads_it() {
    let chat = feed(vec![
        msg(1, NOW - 86_400_000, "s-a", "yesterday's talk"),
        msg(2, 1_792_497_600_000, "s-a", "last month"),
        msg(3, 1_765_013_400_000, "s-a", "last year"),
    ]);
    let s = drawn(&chat);
    assert_eq!(
        ids(&s),
        vec!["[today]", "1", "[today]", "2", "[today]", "3"],
        "{s}"
    );
    let words: Vec<&str> = s["entries"]
        .as_array()
        .map(|e| e.iter().filter_map(|x| x["text"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(words, vec!["yesterday", "Oct 20", "Dec 6, 2025"], "{s}");

    // A message the wire carried no timestamp for is not midnight 1970: no divider, no hour.
    let chat = feed(vec![msg(1, 0, "s-a", "no stamp")]);
    let s = drawn(&chat);
    assert_eq!(ids(&s), vec!["1"], "{s}");
    assert_eq!(entry(&s, 1).get("time"), None, "{s}");
}

/// A quote. A reply names the message it answers and as much of it as a quote line holds.
#[test]
fn a_reply_quotes_the_message_it_answers() {
    let long = "a line that runs on, and on, past the sixty columns a quote line has room for";
    let mut answer = msg(2, NOW, "s-b", "yes");
    answer.thread = Some(1);
    let chat = feed(vec![msg(1, NOW - 60_000, "s-a", long), answer]);
    let s = drawn(&chat);
    let quote = entry(&s, 2)["quote"].as_str().unwrap_or_default();
    assert!(quote.starts_with("↳ 1 s-a: a line that runs on"), "{quote}");
    assert!(quote.ends_with('…'), "the cut is marked: {quote}");
    assert_eq!(width(quote.trim_start_matches("↳ 1 s-a: ")), QUOTE_COLS);

    // A first line that is blank is skipped: a quote of nothing says nothing.
    let chat = feed(vec![
        msg(1, NOW - 60_000, "s-a", "\n\n  the real first line"),
        {
            let mut m = msg(2, NOW, "s-b", "yes");
            m.thread = Some(1);
            m
        },
    ]);
    let s = drawn(&chat);
    assert_eq!(
        entry(&s, 2)["quote"],
        json!("↳ 1 s-a: the real first line"),
        "{s}"
    );
}

/// A system line. A todo or a queue row is one quiet row: no header, no hue, and its refs bracketed
/// and escaped so the markdown renderer draws them.
#[test]
fn a_system_line_is_one_quiet_row_with_its_ref_escaped() {
    let mut m = msg(1, NOW, "letibot", "wrote the doc");
    m.kind = "system".into();
    m.refs = vec![letibot_plugin::pane::ChatRef {
        kind: "todo".into(),
        id: "R150".into(),
    }];
    let chat = feed(vec![m]);
    let s = drawn(&chat);
    assert_eq!(entry(&s, 1)["role"], json!("faint"), "{s}");
    assert_eq!(
        entry(&s, 1)["body"],
        json!("· letibot: wrote the doc \\[todo R150\\]"),
        "{s}"
    );
}

/// An empty channel says which of the two empty facts it is.
#[test]
fn an_empty_channel_says_whether_it_is_empty_or_unanswered() {
    let mut chat = feed(Vec::new());
    chat.heard = false;
    assert_eq!(drawn(&chat)["empty"], json!("asking the coordinator..."));
    chat.heard = true;
    assert_eq!(drawn(&chat)["empty"], json!("nothing here yet"));
}

/// The view as the host parses it - the answer, read back as the head reads one.
fn the_view(chat: &Chat) -> Value {
    serde_json::to_value(view(chat, stream(chat, NOW, 0))).expect("the view serialises")
}

/// The answer is a view whose panel is the stream, and whose input binds the composer to the
/// channel - and a host with no clock gets nothing at all, so the native adapter draws.
#[test]
fn the_view_answers_a_stream_and_binds_the_composers_key() {
    let mut chat = feed(vec![msg(1, NOW, "s-a", "hello")]);
    let first = the_view(&chat);
    assert_eq!(first["input"]["key"], json!("host"), "{first}");
    assert_eq!(
        first["panels"][0]["summary"][0]["stream"]["entries"][0]["divider"],
        json!("today")
    );
    assert_eq!(
        first["panels"][0]["summary"][0]["stream"]["entries"][1]["id"],
        json!("1")
    );
    chat.selected = None;
    assert_eq!(the_view(&chat)["input"]["key"], json!(NO_CHANNEL));

    // Through the hook itself: a host that hands no `now_ms` is answered *unchanged*, and the
    // head's own adapter draws.
    let asked = json!({"v": 3, "feeds": {"chat": serde_json::to_value(feed(Vec::new())).unwrap()}});
    assert_eq!(answer(asked.to_string().as_bytes()), None);
    let answered = json!({"v": 3, "now_ms": NOW, "utc_offset_s": 0,
        "feeds": {"chat": serde_json::to_value(feed(vec![msg(1, NOW, "s-a", "hello")])).unwrap()}});
    let said: Value =
        serde_json::from_str(&answer(answered.to_string().as_bytes()).expect("an answer"))
            .expect("json");
    assert_eq!(
        said["panels"][0]["summary"][0]["stream"]["entries"][1]["body"],
        json!("hello")
    );
}

// ──────────────────────────── the line Enter took ────────────────────────────

/// One line, as the head hands it over: the text, the reply target and the feed.
fn line(text: &str, chat: &Chat) -> Value {
    let asked = json!({"line": text, "key": "host", "reply_to": Value::Null, "chips": [],
                       "feeds": {"chat": serde_json::to_value(chat).unwrap()}});
    serde_json::from_str(&command(asked.to_string().as_bytes()).expect("an answer")).expect("json")
}

/// `/reply N text` is a room message that answers message N.
#[test]
fn slash_reply_12_answers_a_room_say_with_thread_12() {
    let chat = feed(Vec::new());
    let said = line("/reply 12 the build is green", &chat);
    assert_eq!(said["acts"][0]["name"], json!(SAY), "{said}");
    assert_eq!(said["acts"][0]["args"]["thread"], json!(12), "{said}");
    assert_eq!(said["acts"][0]["args"]["text"], json!("the build is green"));
    assert_eq!(said["acts"][0]["args"]["room"], json!("host"));

    // A number with no words is refused rather than sent as an empty message.
    let said = line("/reply 12", &chat);
    assert!(said["said"].is_string(), "{said}");
    assert!(said.get("acts").is_none(), "{said}");
}

/// `/dm NAME text` is a direct message to that peer, and so is a plain line typed in a DM channel:
/// the peer is the channel's, not the head's selected room.
#[test]
fn slash_dm_answers_a_room_dm() {
    let chat = feed(Vec::new());
    let said = line("/dm s-b psst", &chat);
    assert_eq!(said["acts"][0]["name"], json!(DM), "{said}");
    assert_eq!(said["acts"][0]["args"]["to"], json!("s-b"));
    assert_eq!(said["acts"][0]["args"]["text"], json!("psst"));

    let mut dm = feed(Vec::new());
    dm.selected = Some("dm:s-b".into());
    let said = line("in the dm", &dm);
    assert_eq!(said["acts"][0]["name"], json!(DM), "{said}");
    assert_eq!(said["acts"][0]["args"]["to"], json!("s-b"), "{said}");
}

/// An unknown verb is a sentence, not a silent nothing: the person typed something the layer does
/// not do, and a screen that says nothing looks like a send that worked.
#[test]
fn an_unknown_verb_is_refused_with_a_sentence() {
    let chat = feed(Vec::new());
    let said = line("/banish s-b", &chat);
    let words = said["said"].as_str().unwrap_or_default();
    assert!(words.contains("/banish"), "the word is named: {words}");
    assert!(words.contains("/reply"), "and what it does know: {words}");
    assert!(said.get("acts").is_none(), "nothing was done: {said}");
    // An empty line is not a refusal: the layer had nothing to say and the key falls through.
    let asked = json!({"line": "   ", "feeds": {"chat": serde_json::to_value(&chat).unwrap()}});
    assert_eq!(command(asked.to_string().as_bytes()), None);
}

/// A plain line is a `room_say`, `>N text` is one that answers N, and the composer's own reply
/// target rides along when there is one.
#[test]
fn a_plain_line_is_a_say_and_the_reply_target_rides_with_it() {
    let chat = feed(Vec::new());
    let said = line("the build is green", &chat);
    assert_eq!(said["acts"][0]["name"], json!(SAY));
    assert_eq!(said["acts"][0]["args"]["text"], json!("the build is green"));
    assert!(
        said["acts"][0]["args"].get("thread").is_none(),
        "a plain line answers nothing: {said}"
    );

    let said = line(">9 and so is this", &chat);
    assert_eq!(said["acts"][0]["args"]["thread"], json!(9), "{said}");
    assert_eq!(said["acts"][0]["args"]["text"], json!("and so is this"));

    let asked = json!({"line": "answered", "key": "host", "reply_to": 4,
                       "feeds": {"chat": serde_json::to_value(&chat).unwrap()}});
    let said: Value =
        serde_json::from_str(&command(asked.to_string().as_bytes()).expect("an answer")).unwrap();
    assert_eq!(said["acts"][0]["args"]["thread"], json!(4), "{said}");
}

/// A picture pasted into the line goes with it: the composer's chips are blob ids the head holds,
/// and a say without their references would post the words and lose the picture.
#[test]
fn a_pasted_picture_rides_with_the_line_as_a_reference() {
    let chat = feed(Vec::new());
    let asked = json!({"line": "look", "key": "host", "reply_to": Value::Null,
                       "chips": ["b1", "b2"],
                       "feeds": {"chat": serde_json::to_value(&chat).unwrap()}});
    let said: Value =
        serde_json::from_str(&command(asked.to_string().as_bytes()).expect("an answer")).unwrap();
    assert_eq!(
        said["acts"][0]["args"]["text"],
        json!("look\n![blob](blob:b1)\n![blob](blob:b2)")
    );
}

// ─────────────────────────────── Tab's row ───────────────────────────────

/// One completion, as the head asks it: the line, the caret and the feed.
fn candidates(text: &str, chat: &Chat) -> Completion {
    let asked = json!({"v": 3, "text": text, "cursor": text.len(), "key": "host",
                       "feeds": {"chat": serde_json::to_value(chat).unwrap()},
                       "now_ms": NOW, "utc_offset_s": 0});
    serde_json::from_str(&complete(asked.to_string().as_bytes()).expect("an answer"))
        .expect("a completion")
}

/// A room with a roster in two rooms, one member away, and a project next to the host room.
fn a_room() -> Chat {
    let mut chat = feed(Vec::new());
    chat.selected = Some("project/letibot".into());
    chat.channels = vec![
        ChatChannel {
            room: "host".into(),
            label: "host".into(),
            unread: 0,
        },
        ChatChannel {
            room: "project/letibot".into(),
            label: "#letibot".into(),
            unread: 2,
        },
    ];
    chat.roster.insert(
        "project/letibot".into(),
        vec![
            ChatMember {
                name: "fix-room-push".into(),
                connected: true,
            },
            ChatMember {
                name: "fix-room-away".into(),
                connected: false,
            },
        ],
    );
    chat.roster.insert(
        "host".into(),
        vec![ChatMember {
            name: "elsewhere-bee".into(),
            connected: true,
        }],
    );
    chat.names
        .insert("fix-room-push".into(), "fix the room push".into());
    chat.verbs = vec![
        ChatVerb {
            name: "dm".into(),
            hint: "NAME TEXT — a direct message".into(),
        },
        ChatVerb {
            name: "reply".into(),
            hint: "N TEXT — answer a message".into(),
        },
        ChatVerb {
            name: "health".into(),
            hint: String::new(),
        },
    ];
    chat.aliases = vec![
        ChatAlias {
            insert: "@cc".into(),
            note: "every folder".into(),
        },
        ChatAlias {
            insert: "@cc@letibot:*".into(),
            note: "every session in letibot".into(),
        },
    ];
    chat
}

/// The roster first. `@` ranks the members of the room being typed to ahead of the others, a prefix
/// ahead of a substring, and six is the row's whole width.
#[test]
fn at_completion_ranks_the_rooms_roster_first() {
    let chat = a_room();
    let answer = candidates("@fix", &chat);
    assert_eq!((answer.from, answer.to), (0, 4), "the fragment's span");
    let inserts: Vec<&str> = answer
        .candidates
        .iter()
        .map(|c| c.insert.as_str())
        .collect();
    assert_eq!(
        inserts,
        vec!["@fix-room-push", "@fix-room-away"],
        "the room's own members, in this room first"
    );
    // A member of this room carries no note, an absent one says so, a stranger says where.
    assert_eq!(answer.candidates[0].note, None);
    assert_eq!(answer.candidates[1].note.as_deref(), Some("away"));
    assert_eq!(
        answer.candidates[0].label.as_deref(),
        Some("fix the room push"),
        "the name the head resolved, not the handle"
    );
    assert_eq!(answer.candidates[0].hue, Some(hue("fix-room-push")));

    // A prefix beats a substring wherever it appears in the list.
    let answer = candidates("@bee", &chat);
    assert_eq!(
        answer.candidates[0].insert, "@elsewhere-bee",
        "a stranger, but the only match"
    );
    assert_eq!(
        answer.candidates[0].note.as_deref(),
        Some("elsewhere"),
        "and the row says where"
    );

    // Six is the row: eight members match and six are offered.
    let mut many = chat.clone();
    many.roster.insert(
        "host".into(),
        (0..8)
            .map(|i| ChatMember {
                name: format!("s-{i}"),
                connected: true,
            })
            .collect(),
    );
    assert_eq!(candidates("@s-", &many).candidates.len(), TOP);

    // Nothing typed but a bare `@` offers the whole roster, best first by handle.
    let answer = candidates("@", &chat);
    let inserts: Vec<&str> = answer
        .candidates
        .iter()
        .map(|c| c.insert.as_str())
        .collect();
    assert_eq!(
        inserts,
        vec![
            "@fix-room-push",
            "@fix-room-away",
            "@elsewhere-bee",
            "@cc",
            "@cc@letibot:*"
        ],
        "this room, then the others, then the bridge's aliases: {inserts:?}"
    );
    // The project's alias is spelled with the label the switcher draws, minus its `#`.
    let alias = candidates("@cc", &chat);
    assert_eq!(alias.candidates[0].insert, "@cc");
    assert_eq!(alias.candidates[1].insert, "@cc@letibot:*");
    assert_eq!(
        alias.candidates[1].note.as_deref(),
        Some("every session in letibot")
    );
}

/// The verbs. A `/` word ranks the composer's own list: prefix before substring, the room's verbs
/// ahead of the head's, and the hint becomes the row's note.
#[test]
fn slash_completion_ranks_the_verbs_with_their_hints() {
    let chat = a_room();
    let answer = candidates("/re", &chat);
    assert_eq!(answer.from, 0);
    assert_eq!(answer.candidates[0].insert, "/reply ");
    assert_eq!(
        answer.candidates[0].note.as_deref(),
        Some("N TEXT — answer a message")
    );
    let answer = candidates("/", &chat);
    let inserts: Vec<&str> = answer
        .candidates
        .iter()
        .map(|c| c.insert.as_str())
        .collect();
    assert_eq!(inserts, vec!["/dm ", "/reply ", "/health "], "{inserts:?}");
    assert_eq!(answer.candidates[2].note, None, "a verb with no hint");
    // A caret in the middle of a word takes the word before it, and a slash that is not the
    // line's first character is prose: exactly the rule the head's own completion keeps.
    let asked = json!({"v": 3, "text": "say /dm", "cursor": 7, "key": "host",
                       "feeds": {"chat": serde_json::to_value(&chat).unwrap()}});
    let said: Completion =
        serde_json::from_str(&complete(asked.to_string().as_bytes()).expect("an answer")).unwrap();
    assert!(said.candidates.is_empty(), "a mid-line slash is prose");
    let said = candidates("/d", &chat);
    assert_eq!(
        said.candidates[0].insert, "/dm ",
        "the span starts at the slash"
    );
    // No `@` and no `/` is nothing to offer, which is not a refusal.
    assert!(candidates("just some words", &chat).candidates.is_empty());
}

#[test]
fn a_completion_is_a_span_of_the_line_the_host_can_apply() {
    let chat = a_room();
    for text in ["@fix", "/rep", "hi @", ""] {
        let answer = candidates(text, &chat);
        answer.validate(text).expect("a span the host can apply");
        assert!(
            answer.candidates.len() <= letibot_plugin::complete::MAX_CANDIDATES,
            "the row draws at most eight"
        );
        assert!(
            answer.candidates.iter().all(|c| !c.insert.is_empty()),
            "every candidate inserts something"
        );
    }
}

// ──────────────────────────── the pieces of the text ────────────────────────────

#[test]
fn a_handle_keeps_one_hue_and_it_is_one_of_the_palette() {
    for handle in ["fix-room-push", "s-a", "cc@letibot:reviewer", "operator"] {
        assert_eq!(hue(handle), hue(handle));
        assert!(hue(handle) < HUES, "{handle}: {}", hue(handle));
    }
    assert_eq!(hue("Fix-Room-Push"), hue("fix-room-push"), "case-blind");
}

#[test]
fn a_body_control_byte_or_an_escape_never_reaches_a_name() {
    assert_eq!(
        clean_line("clean\u{1b}[2Jline\u{7}and\ttabbed").as_ref(),
        "cleanline and tabbed"
    );
    assert_eq!(clean_line("two\nlines").as_ref(), "two lines");
    assert_eq!(clean_line("plain").as_ref(), "plain");
    // Nothing to remove is borrowed: a window of two hundred names allocates none of them.
    assert!(matches!(clean_line("plain"), Cow::Borrowed(_)));
    assert!(matches!(clean_line("tab\there"), Cow::Owned(_)));
}

#[test]
fn the_at_tokens_are_the_room_s_own_words() {
    assert_eq!(mentions("ping @s-a now"), vec!["@s-a"]);
    assert_eq!(mentions("mail a@b is not a token"), Vec::<String>::new());
    assert_eq!(
        mentions("hey @cc@letibot:* look"),
        vec!["@cc@letibot:*"],
        "a bridge's address is one token"
    );
    assert_eq!(mentions("@operator"), vec!["@operator"]);
    // A mention of the reader ends a run and wears its mark; a mention of anyone else does not.
    let mut m = msg(1, NOW, "s-a", "hey @operator");
    assert!(mentions_operator(&m));
    m.text = "hey @operatorials".into();
    assert!(!mentions_operator(&m), "a longer handle is not the reader");
}
