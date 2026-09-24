//! **Read a live session's user rows and say what shape their text is.**
//!
//! Written for the `queued ·` defect (2026-09-23): a head's echo of a prompt it sent
//! is retired when a landing row's text matches it, and the question that decides
//! whether the rule can ever fire is *what the landing row's text actually is* — one
//! prompt, or several joined by newlines. This prints, per user row, the line count,
//! the character count and the first line, so the shape is read rather than assumed.
//!
//!     cargo run -q --example dump_queued -- <socket> <session>
//!
//! **It needs a daemon of this head's own protocol version**, because it attaches as a
//! head — and on 2026-09-23 it could not, which is why the measurement that answered the
//! question for the running session reads the STORE instead
//! (`docs/evidence/queued-echoes-2026-09-23.py`): the running daemon spoke 23 and the tree
//! 25, so nothing built from this tree could attach to it. Kept because it is the direct
//! probe once both halves are the same build, and because the store-side script answers
//! the same question with no daemon at all.
//!
//! Read-only: it attaches as a `remote` head called `dump-queued` and says nothing.

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
