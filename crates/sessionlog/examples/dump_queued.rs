//! Read a live session's user rows and say what shape their text is: the line count, the
//! character count and the first line of each, so the shape is read rather than assumed.
//! Read-only - it attaches as a `remote` head called `dump-queued` and says nothing.
//!
//!     cargo run -q --example dump_queued -- <socket> <session>

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_transcript::{TranscriptItem, UserPart};

fn main() {
    let path = std::env::args().nth(1).expect("socket path");
    let session = std::env::args().nth(2).expect("session id");
    let (client, hello, _reader) = HeadClient::attach(
        std::path::Path::new(&path),
        &session,
        0,
        "remote",
        "dump-queued",
        Caps::default(),
    )
    .expect("attach");

    let ServerFrame::Hello {
        session_id,
        snapshot,
        ..
    } = &hello
    else {
        panic!("the first frame was not a Hello: {hello:?}");
    };
    let Some(s) = snapshot else {
        println!("session {session_id}: no snapshot");
        return;
    };
    println!(
        "session {session_id}: {} row(s) in the snapshot",
        s.items.len()
    );

    let mut users = 0usize;
    let mut bodyless_users = 0usize;
    for i in &s.items {
        if i.kind != "user" {
            continue;
        }
        users += 1;
        let Some(item) = &i.item else {
            bodyless_users += 1;
            println!("  {:<26} NO BODY (announced, unfilled)", i.item_id);
            continue;
        };
        let TranscriptItem::User { parts, .. } = item else {
            println!("  {:<26} kind says user, item is something else", i.item_id);
            continue;
        };
        let texts: Vec<&str> = parts
            .iter()
            .filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let joined = texts.join("\u{0}");
        let lines = joined.lines().count();
        println!(
            "  {:<26} {} part(s), {} char(s), {} line(s)  first: {:?}",
            i.item_id,
            texts.len(),
            joined.chars().count(),
            lines,
            joined
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(70)
                .collect::<String>()
        );
    }
    println!(
        "\nuser rows: {users}  ({} of them announced and never filled)",
        bodyless_users
    );
    drop(client);
}
