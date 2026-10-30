//! **Measure the `queue` status shapes against a store** — the operator's report of 2026-10-30 was
//! about the SIZE of the answer (*"it is more than 60 rows for queue status tool call now - and you
//! guys defocus and get confused"*), so the number that has to be measured is lines, on the box's
//! own rows rather than a fixture's.
//!
//! ```sh
//! cargo run --offline -p letibot-harnessd --example queue_status -- <store.db> <workspace> [name]
//! ```
//!
//! `<workspace>` is the folder a session would be seated in, so the default scope is the one a
//! model there gets; `[name]` is what to ask for in the third shape. It goes through the real
//! `SessionQueue`, so the shapes are the ones the tool returns and not a second copy of them. The
//! last block is what the tool printed BEFORE the scoping — the whole box in full — which is the
//! number this change is measured against.

use std::path::PathBuf;

use letibot_harnessd::mergequeue::desk::{SessionQueue, status};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::Store;
use letibot_tools::builtins::queue::QueueDesk;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(store), Some(workspace)) = (args.first(), args.get(1)) else {
        eprintln!("usage: queue_status <store.db> <workspace> [name]");
        std::process::exit(2);
    };
    let desk = SessionQueue {
        store_path: PathBuf::from(store),
        session: "s-measure".into(),
        workspace: PathBuf::from(workspace),
        registry: Registry::new(),
    };
    let named = args.get(2).map(String::as_str);
    for (what, all, repo) in [
        ("status", false, None),
        ("status all", true, None),
        ("status repository", false, named),
    ] {
        if what == "status repository" && repo.is_none() {
            continue;
        }
        match desk.status(all, repo) {
            Ok(said) => println!(
                "===== {what}{} — {} line(s) =====\n{said}",
                repo.map(|r| format!(" ({r})")).unwrap_or_default(),
                said.lines().count()
            ),
            Err(why) => println!("===== {what} — refused =====\n{why}"),
        }
    }
    // **And the answer this replaces**: every repository, every open or parked row, in full.
    match Store::open(&PathBuf::from(store))
        .map_err(|e| e.to_string())
        .and_then(|s| status(&s, None))
    {
        Ok(said) => println!(
            "===== the whole box in full, as `status` printed before this change — {} line(s) \
             =====\n{said}",
            said.lines().count()
        ),
        Err(why) => println!("===== the whole box in full — refused =====\n{why}"),
    }
}
