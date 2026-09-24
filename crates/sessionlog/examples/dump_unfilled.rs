//! **Name the rows a snapshot announced and did not fill** — the `2 row(s)` sentence.
//!
//! Read-only, one shot: attach as a `remote` head, take the snapshot, and print every row
//! whose `item` is `None` with its **id, kind, ledger head and ts**. The head's own line
//! prints a count and no ids, which is the least useful form of the fact; this prints the
//! fact.
//!
//!     cargo run -q --example dump_unfilled -- /run/user/1000/letibot/<hash>.sock <session>
//!
//! Two things it answers that the head's sentence cannot:
//!
//! * **WHICH rows**, by id, so a claim can be checked rather than counted.
//! * **Whether the daemon is still announcing them.** This is a FRESH attach, so a body-less
//!   row in its snapshot is the daemon's view as of now; a complete snapshot means the
//!   unfilled rows belong to a head's live session and not to the log.

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

fn main() {
    let path = std::env::args().nth(1).expect("socket path");
    let session = std::env::args().nth(2).expect("session id");
    let (client, hello, _reader) = HeadClient::attach(
        std::path::Path::new(&path),
        &session,
        0,
        "remote",
        "dump-unfilled",
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
        println!("session {session_id}: no snapshot (served from scrollback)");
        return;
    };

    let mut unfilled: Vec<&letibot_sessionlog::view::SnapshotItem> =
        s.items.iter().filter(|i| i.item.is_none()).collect();
    // Oldest first, which is the order a reader counts them in.
    unfilled.sort_by_key(|i| i.ts);

    println!(
        "session {session_id}: {} row(s) in the snapshot, {} with no body",
        s.items.len(),
        unfilled.len()
    );
    if unfilled.is_empty() {
        println!(
            "\n**The daemon is not announcing any unfilled row right now.** A snapshot taken\n\
             at this instant is complete, so a head's `N row(s) announced and never filled in`\n\
             is a claim about what THAT head is missing — not about what the log holds."
        );
    } else {
        println!(
            "\n{:<28} {:<12} {:>15}  {}",
            "item id", "kind", "ts", "ledger head"
        );
        for i in unfilled {
            println!(
                "{:<28} {:<12} {:>15}  {}",
                i.item_id,
                i.kind,
                i.ts,
                i.ledger_head.chars().take(20).collect::<String>()
            );
        }
    }
    // The two numbers a head draws its sentence from, computed the same way it computes
    // them: rows with a body, out of all rows. If this disagreement is nonzero, the
    // mismatch is the head's accounting and not the log's content.
    let filled = s.items.iter().filter(|i| i.item.is_some()).count();
    println!(
        "\nthe head's own arithmetic on this snapshot: {} items - {} arrived = {} outstanding",
        s.items.len(),
        filled,
        s.items.len() - filled
    );
    drop(client);
}
