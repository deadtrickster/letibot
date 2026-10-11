//! **One turn tail for every door** — `TODO.md` R68.4, as one assertion list run against each.
//!
//! The review of 2026-10-11 found the tails had diverged: the sweep was armed by the command and
//! wake arms of `Daemon::run` (at their call sites, so a door reached any other way had none) and
//! by the nag door by hand; the `!` doors armed neither the sweep nor the wake; a subagent ran a
//! second copy of the tail whose `Answer` arm dropped its error (`.is_ok()`), so a child's failed
//! turn published a `turn_failed` with no reason and no `TurnFailed` at all.
//!
//! Every door now runs `turn_tail::run_turn`. What this file asserts is what that buys, door by
//! door, with ONE list ([`the_tail_ran`]): the sweep is armed by the door itself, and a failed
//! turn reaches the session's log as `TurnFailed` carrying the reason plus a `turn_failed`
//! warning — the model is a stub that refuses every request with a body this file can find.
//!
//! MEASURED before the change: the prompt, message, wake and `!` doors driven through `Sessions`
//! (not through `Daemon::run`'s arms) left no sweep armed; the child's prompt published no
//! `TurnFailed`.
//!
//! Needs the vocabulary GGUF and no model.

use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::Outcome;
use letibot_harnessd::{Dialect, Harness, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoCondition, TodoItem, TodoStatus};
use letibot_transcript::ToolOutcome;
use letibot_turn::Endpoint;

/// What the stub says, so a failure's reason can be traced to it.
const REFUSAL: &str = "stub-refusal-r68-turn-tail";

/// A model that refuses every request with a 400 whose body is [`REFUSAL`].
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
            let _ = write!(
                s,
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL}",
                REFUSAL.len()
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
    cfg.http_retries = 0;
    cfg.endpoint = Endpoint::new("127.0.0.1", model_port);
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// The session's row, an unfinished plan (so the nag door has something to say) and a row whose
/// condition names a job no daemon holds (so a wake has something to say: an unknown job's fate
/// meets the condition, and a failed turn does not consume it).
fn seed(path: &std::path::Path, session_id: &str) {
    let s = Store::open(path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.into(),
        title: Some("the tail session".into()),
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
        &[
            TodoItem {
                content: "finish the migration".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "read what the build said".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: Some(TodoCondition::Job {
                    handle: "j-from-another-daemon".into(),
                }),
                needs: Vec::new(),
            },
        ],
    )
    .expect("the todo rows");
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{tag}-{}-{}-{}",
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
    dir
}

/// How many `TurnFailed` events carrying the stub's reason, and how many `turn_failed` warnings
/// carrying it, the hub holds.
fn failures(hub: &Hub) -> (usize, usize) {
    let mut turn_failed = 0;
    let mut warned = 0;
    for e in hub.retained() {
        match &e.event {
            SessionEvent::TurnFailed { error, .. } if error.contains(REFUSAL) => turn_failed += 1,
            SessionEvent::Warning { code, detail, .. }
                if code == "turn_failed" && detail.contains(REFUSAL) =>
            {
                warned += 1
            }
            _ => {}
        }
    }
    (turn_failed, warned)
}

/// **THE ONE LIST** — what every door's turn must leave behind, whatever the door. `before` is
/// [`failures`] taken before the door ran; `sweep` is whether the door armed the sweep (`None`
/// for an owner whose sweep this file cannot read: a child's clock is its own thread's).
fn the_tail_ran(door: &str, hub: &Hub, before: (usize, usize), sweep: Option<bool>) {
    if let Some(armed) = sweep {
        assert!(
            armed,
            "{door}: the door left no abandoned-call sweep armed — a call its turn stranded \
             would be looked at by nothing"
        );
    }
    let after = failures(hub);
    assert!(
        after.0 > before.0,
        "{door}: the failed turn published no `TurnFailed` with its reason — a head's spinner \
         stays on a dead turn: {:#?}",
        hub.retained().iter().map(|e| &e.event).collect::<Vec<_>>()
    );
    assert!(
        after.1 > before.1,
        "{door}: no `turn_failed` warning carried the reason"
    );
}

#[test]
fn every_door_of_a_session_the_daemon_holds_runs_one_tail() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = temp_dir("harnessd-turn-tail");
    let path = dir.join("sessions.db");
    let id = "s-turn-tail";
    seed(&path, id);
    let cfg = config(&path, id, refusing_model_stub());
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    registry
        .create(id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    let hub = registry.get(id).expect("the hub");
    let head = hub.attach("tui", "dead", Caps::default(), 0);

    // Each door: spend the sweep first (what the idle arm does), run the door, check the list.
    let mut door = |name: &str, sessions: &mut Sessions<'_>, run: &dyn Fn(&mut Sessions<'_>)| {
        sessions.sweep_abandoned_calls();
        assert!(
            sessions.sweep_due_at().is_none(),
            "{name}: the fixture spent the sweep"
        );
        let before = failures(&hub);
        run(sessions);
        the_tail_ran(name, &hub, before, Some(sessions.sweep_due_at().is_some()));
    };

    door("a prompt", &mut sessions, &|s| {
        hub.submit(
            &head.head_id,
            "p1",
            0,
            CommandKind::Prompt {
                text: "carry on".into(),
            },
        );
        let cmd = hub.try_command().expect("the prompt is queued");
        assert!(matches!(s.dispatch(id, &cmd), Outcome::Failed(_)));
    });
    door("a message", &mut sessions, &|s| {
        hub.submit(
            DAEMON_SUBMITTER,
            "m1",
            0,
            CommandKind::Message {
                from: "s-a-peer".into(),
                text: "the build is green".into(),
            },
        );
        let cmd = hub.try_command().expect("the message is queued");
        assert!(matches!(s.dispatch(id, &cmd), Outcome::Failed(_)));
    });
    door("a wake", &mut sessions, &|s| {
        assert!(
            matches!(s.wake(id), Outcome::Failed(_)),
            "the due row gave the wake a turn to run"
        );
    });
    door("the idle plan-check", &mut sessions, &|s| {
        s.set_nag_window(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(20));
        let _ = s.deliver_due_nags();
    });
    door("the turn after a `!` line", &mut sessions, &|s| {
        assert!(hub.submit_daemon(
            "dead",
            "bang-1",
            CommandKind::OperatorShellResult {
                line: "echo hi".into(),
                who: "dead".into(),
                call_id: "bang-1".into(),
                outcome: ToolOutcome::Ok,
                payload: "hi".into(),
                spill: None,
            },
        ));
        let cmd = hub.try_command().expect("the run's result is queued");
        assert!(matches!(s.dispatch(id, &cmd), Outcome::Failed(_)));
    });
    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A subagent's door is the same tail** — its own thread, `serve_child`, runs
/// `turn_tail::run_turn` with the child's owner. Before, the `Answer` arm was
/// `submit_as_a_normal_session(..).is_ok()`: the reason was dropped and no `TurnFailed` went out.
#[test]
fn a_subagents_door_runs_the_same_tail() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = temp_dir("harnessd-turn-tail-child");
    let path = dir.join("sessions.db");
    let child = "s-turn-tail-child";
    let cfg = config(&path, child, refusing_model_stub());
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    let hub = registry.new_hub(child.to_string());
    hub.owned_by_its_own_thread();
    registry
        .adopt(
            hub.clone(),
            "a child",
            Sessions::wiring(&cfg),
            Some("s-its-parent".into()),
        )
        .expect("adopted");
    let mut sub =
        Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, registry.clone())
            .expect("the child opens");
    let served = hub.clone();
    let serving = std::thread::spawn(move || {
        letibot_harnessd::harness::serve_child_under(
            &mut sub,
            &served,
            child,
            letibot_harnessd::sessions::TodoNagClock::new(),
        );
    });
    let before = failures(&hub);
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    hub.submit(
        &head.head_id,
        "p1",
        0,
        CommandKind::Prompt {
            text: "what did you find?".into(),
        },
    );
    let began = std::time::Instant::now();
    while failures(&hub).0 == before.0 && began.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(10));
    }
    the_tail_ran("a prompt to a subagent", &hub, before, None);
    registry.close();
    let _ = serving.join();
    let _ = std::fs::remove_dir_all(&dir);
}
