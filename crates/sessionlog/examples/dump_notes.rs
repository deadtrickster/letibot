//! Dump the notes a session's log holds, with the key `/notes dismiss` would store.
//!
//! Read-only, and one shot: attach as a `remote` head, take the snapshot, print every
//! warning as `code | ts | fnv1a(detail) | the composed key`, and detach.
//!
//! This exists because the operator's screen and `head.toml` disagreed and there was no
//! way to see the middle term. `/notes` shows a note's code and detail; `head.toml`
//! shows a key; the `ts` that joins them is on neither. It is the one number that
//! decides whether R10's key identifies a delivery or an incident, so it gets an
//! instrument rather than an argument.
//!
//!     cargo run -q --example dump_notes -- /run/user/1000/letibot/<hash>.sock

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// The head's own hash, copied deliberately: a dump that computed a *different* hash
/// from the one `note_key` uses would answer a question about this file rather than
/// about the head. Same constants, same order, same `{:016x}`.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn key(code: &str, ts: u64, detail: &str) -> String {
    format!("w|{code}|{ts}|{:016x}", fnv1a(detail))
}

fn main() {
    let path = std::env::args().nth(1).expect("socket path");
    let session = std::env::args().nth(2).expect("session id");
    let (client, hello, _reader) = HeadClient::attach(
        std::path::Path::new(&path),
        &session,
        0,
        "remote",
        "dump-notes",
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

    println!("session {session_id} — {} warning(s), {} head(s) attached", s.warnings.len(), s.heads.len());
    for h in &s.heads {
        println!("  head: {} kind={} identity={}", h.head_id, h.kind, h.identity);
    }
    println!("{:<44} {:>15}  {}", "code", "ts", "key (as /notes dismiss would store it)");
    let mut keys: Vec<String> = Vec::new();
    for w in &s.warnings {
        let k = key(&w.code, w.ts, &w.detail);
        println!("{:<44} {:>15}  {}", w.code, w.ts, k);
        keys.push(k);
    }

    // The comparison the operator asked for, done here rather than by eye.
    let cfg = std::env::var("HOME").unwrap_or_else(|_| "/home/dead".into());
    let toml = std::fs::read_to_string(format!("{cfg}/.config/letibot/head.toml")).unwrap_or_default();
    let retired: Vec<String> = toml
        .lines()
        .find(|l| l.trim_start().starts_with("retired"))
        .and_then(|l| l.split_once('=').map(|(_, v)| v))
        .map(|v| v.trim().trim_matches('"').to_string())
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    println!("\nhead.toml holds {} retired key(s):", retired.len());
    for k in &retired {
        let hit = keys.iter().position(|x| x == k);
        match hit {
            Some(i) => println!("  MATCH  → live note {}   {k}", i + 1),
            None => {
                // The useful half: which note it *wanted* to be. Code + hash, ignoring
                // the ts, is what says "same incident, different delivery".
                let f: Vec<&str> = k.split('|').collect();
                let (code, hash) = (f.get(1).copied().unwrap_or(""), f.get(3).copied().unwrap_or(""));
                let near = s.warnings.iter().position(|w| {
                    w.code == code && format!("{:016x}", fnv1a(&w.detail)) == hash
                });
                match near {
                    Some(i) => println!(
                        "  MISS   → live note {} has the SAME code and detail, ts {} against {k}'s {}",
                        i + 1,
                        s.warnings[i].ts,
                        f.get(2).copied().unwrap_or("?")
                    ),
                    None => println!("  MISS   → nothing live matches even code+detail   {k}"),
                }
            }
        }
    }
    drop(client);
}
