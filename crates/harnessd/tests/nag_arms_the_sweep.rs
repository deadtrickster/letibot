//! A nag turn arms the clock that closes out a call nothing answered - the third door.
//! `idle_clock.rs` is this file's sibling and carries the argument

use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoStatus};
use letibot_turn::Endpoint;

mod support;

/// A model server that refuses in the one way the retry rule respects: a 400 fails the turn at once
/// where a refused port is retried.
fn refusing_model_stub() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut acc: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => acc.extend_from_slice(&buf[..n]),
                }
                if let Some(head_end) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                    let body_have = acc.len() - (head_end + 4);
                    let body_want = acc[..head_end]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            let line = String::from_utf8_lossy(line);
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if body_have >= body_want {
                        break;
                    }
                }
                if acc.len() > (1 << 20) {
                    break;
                }
            }
            let _ = s.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    port
}

fn config(store: &std::path::Path, session_id: &str, model_port: u16) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // Never this box's model server: the nag's turn goes to the stub.
    cfg.endpoint = Endpoint::new("127.0.0.1", model_port);
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// A store holding one session with an unfinished plan - `todos.rs`'s fixture: the session row
/// first, because the todo row carries a foreign key to it.
fn seed(path: &std::path::Path, session_id: &str) {
    let s = Store::open(path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.into(),
        title: Some("the nag session".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/tmp".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    s.put_todos(
        session_id,
        &[TodoItem {
            content: "finish the migration".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    )
    .expect("the todo row");
}

#[test]
fn a_nag_turn_arms_the_sweep_that_closes_a_call_it_stranded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "harnessd-nag-sweep-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let session_id = "nag-sweep";
    seed(&path, session_id);

    let cfg = config(&path, session_id, refusing_model_stub());
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // The premise: nothing has armed the sweep - no command ran, no wake. What the idle arm does
    // before delivering is spend it (`sweep_abandoned_calls`), which is the state the nag door is
    // entered in.
    sessions.sweep_abandoned_calls();
    assert!(
        sessions.sweep_due_at().is_none(),
        "the fixture must start with no sweep armed, or the nag is not what arms it"
    );

    // The window, shortened so the open plan comes due now rather than in a minute.
    sessions.set_nag_window(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(20));
    let _ = sessions.deliver_due_nags();

    // The turn ran (and failed on the stub's 400): the `[todo check]` row is on the session's log.
    let hub = registry.get(session_id).expect("the session's hub");
    let nagged = hub.retained().iter().any(|e| {
        matches!(
            &e.event,
            letibot_sessionlog::SessionEvent::TranscriptContent { item, .. }
                if matches!(
                    &**item,
                    letibot_transcript::TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Agent,
                        ..
                    }
                )
        )
    });
    assert!(
        nagged,
        "the nag's turn never ran, so this test proved nothing"
    );

    assert!(
        sessions.sweep_due_at().is_some(),
        "**a nag turn left no sweep behind** — the idle arm spent `sweep_at` a line before \
         delivering, so a call this turn stranded would be looked at by nothing: the worker's \
         only deadline is the nag's own repeat, minutes away. The command and wake arms arm it \
         before their turn; the nag door must too."
    );
    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}
