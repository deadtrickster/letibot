//! The operator's own shell line, `ClientFrame::OperatorShell`: what the daemon decides before the
//! queue. A `!` line is queued with the identity its row's `CallOrigin` carries, anything else is
//! refused in a sentence, and the round-boundary picker takes the line so it runs mid-turn.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-opshell-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create("a", "", SessionWiring::default()).unwrap();
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

/// How long a wait for a frame may take, in milliseconds: a local socket round trip on a shared
/// box is the box's fact, and `next_frame`'s caller has nothing else to judge it by.
const PATIENCE_MS: u64 = 30_000;

/// Wait for the next frame that is not a resync or a `CommandIssued` announcement: two come back
/// per command - `Accepted` on this connection and `CommandIssued` on the session log.
fn next_frame(rx: &std::sync::mpsc::Receiver<Inbound>, ms: u64) -> Option<ServerFrame> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let f = inbound.frame();
        match f {
            ServerFrame::Resync { .. } => continue,
            ServerFrame::Event(env)
                if matches!(
                    env.event,
                    letibot_sessionlog::SessionEvent::CommandIssued { .. }
                ) =>
            {
                continue;
            }
            other => return Some(other),
        }
    }
    None
}

/// A `!` line reaches the queue as the operator's own shell line, verbatim, with the identity
/// attached - and stale-tolerant, because the operator who typed while the screen moved meant it.
#[test]
fn a_bang_line_reaches_the_queue_as_the_operators_own_shell_line() {
    let (r, handle) = start("queue");
    let hub = r.get("a").expect("session");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    // `expected_seq` 0 is the honest "I have seen nothing" spelling and skips the
    // staleness machinery; a second call below uses a deliberately stale number.
    client.operator_shell(0, "! ls .").expect("write the frame");
    let f = next_frame(&rx, PATIENCE_MS).expect("the daemon must answer");
    match f {
        ServerFrame::Accepted { note, .. } => {
            assert!(
                note.contains("! ls ."),
                "the note must name the line it queued: {note}"
            );
            assert!(
                note.contains("queued"),
                "an accepted `!` line is queued for this session to run: {note}"
            );
        }
        other => panic!("`! ls .` was not accepted: {other:?}"),
    }
    let cmd = hub
        .try_command()
        .expect("the worker's half is still in the queue");
    match cmd.kind {
        CommandKind::OperatorShell { line, who } => {
            // Verbatim, bang included: the `User` row the daemon appends is the operator's own
            // words, and `! ls .` is what they typed.
            assert_eq!(line, "! ls .");
            assert_eq!(who, "dead", "the row's `CallOrigin` names the actor");
        }
        other => panic!("the queue holds something else: {other:?}"),
    }

    // Stale-tolerant, and said so: the attach moved the seq, so 0 is now stale for a second
    // line - and the frame is queued anyway.
    client
        .operator_shell(1, "!cargo test -p letibot-sessionlog")
        .expect("write the second frame");
    let f = next_frame(&rx, PATIENCE_MS).expect("the daemon must answer the stale one");
    match f {
        ServerFrame::Accepted { note, .. } => {
            assert!(
                note.contains("queued anyway"),
                "a stale `!` line is queued anyway, and says so: {note}"
            );
        }
        other => panic!("a stale `!` line was not accepted: {other:?}"),
    }
    assert!(
        matches!(
            hub.try_command().map(|c| c.kind),
            Some(CommandKind::OperatorShell { .. })
        ),
        "the stale line never reached the queue"
    );
    let _ = head;
    handle.shutdown();
}

/// Nothing but the bang is refused by name, and nothing is queued: `!`, `!   ` and a line with no
/// bang all get a sentence saying what a `!` line is. The daemon re-checks because a frame is a
/// socket, not a keyboard.
#[test]
fn a_line_without_a_command_after_the_bang_is_refused_and_nothing_is_queued() {
    let (r, handle) = start("refuse");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    for line in ["!", "!   ", "ls .", "/not a bang either"] {
        let _ = client.operator_shell(0, line).expect("write");
        let f = next_frame(&rx, PATIENCE_MS).expect("the daemon must answer");
        match f {
            ServerFrame::Rejected { reason, .. } => {
                assert!(
                    reason.contains("not a `!` command") && reason.contains("Nothing ran"),
                    "the refusal must say what a `!` line is and that nothing ran: {reason}"
                );
            }
            other => panic!("`{line}` was not refused: {other:?}"),
        }
        assert!(
            hub.try_command().is_none(),
            "`{line}` was refused and still queued something"
        );
    }
    handle.shutdown();
}

/// `!!` and `!x` are commands, not errors: `!!` is not "repeat the last command" (there is no
/// history here), and `!x` needs no space to be a command.
#[test]
fn the_bang_rule_is_pinned_on_its_edges() {
    let (r, handle) = start("edges");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    // `!ls` - no space - carries the command `ls`, and `!!` carries `!`, which the shell answers
    // for itself: neither is refused and neither is rewritten.
    for (typed, command) in [("!ls .", "ls ."), ("!!", "!"), ("! echo '!!'", "echo '!!'")] {
        let _ = client.operator_shell(0, typed).expect("write");
        let _ = next_frame(&rx, PATIENCE_MS).expect("accepted");
        let got = hub
            .try_command()
            .unwrap_or_else(|| panic!("`{typed}` never reached the queue"));
        match got.kind {
            CommandKind::OperatorShell { line, .. } => {
                assert_eq!(line, typed, "the typed line travels verbatim");
                assert_eq!(
                    letibot_sessionlog::operator_shell_command(&line),
                    Some(command),
                    "`{typed}` must carry the command `{command}` and nothing else"
                );
            }
            other => panic!("`{typed}` queued something else: {other:?}"),
        }
    }
    handle.shutdown();
}

/// A queued `!` line is taken by the round-boundary picker, which is what makes it run mid-turn
/// rather than wait for the worker that is inside the turn - and the prompt behind it is left for
/// the worker, so a mid-turn `!` cannot eat a prompt.
#[test]
fn the_round_boundary_picker_takes_a_queued_bang_line() {
    let hub = letibot_sessionlog::hub::Hub::new("a");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    hub.submit(
        &head.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "a prompt ahead of it".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    hub.submit(
        &head.head_id,
        "c2",
        0,
        CommandKind::OperatorShell {
            line: "! ls .".into(),
            who: "dead".into(),
        },
    );
    let taken = hub
        .try_head_run_command()
        .expect("the picker must take the operator's shell line");
    assert!(matches!(taken.kind, CommandKind::OperatorShell { .. }));
    // And the prompt is still there for the worker: the picker took one thing, not the queue.
    assert!(matches!(
        hub.try_command().map(|c| c.kind),
        Some(CommandKind::Prompt { .. })
    ));
}

/// A recalled `!` line is taken back, not just its echo: the up arrow pulls every held line into
/// the composer, so the daemon must drop the queued `OperatorShell` too - a shell command that
/// runs after it was visibly taken back is work happening, not a message landing late.
#[test]
fn a_recall_takes_the_queued_bang_line_back_with_the_prompts() {
    let hub = letibot_sessionlog::hub::Hub::new("a");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    hub.submit(
        &head.head_id,
        "c1",
        0,
        CommandKind::OperatorShell {
            line: "! make".into(),
            who: "dead".into(),
        },
    );
    hub.submit(
        &head.head_id,
        "c2",
        0,
        CommandKind::Prompt {
            text: "and a prompt queued after it".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    hub.submit(&head.head_id, "c3", 0, CommandKind::WithdrawPrompts);
    assert!(
        hub.try_withdraw_command(),
        "the withdraw was queued and must be taken"
    );
    // The take-back removes itself with the head's queued lines, so nothing of them remains:
    // the bang line AND the prompt are gone, not just their echo.
    assert!(
        hub.try_command().is_none(),
        "nothing of the recalled head's queue remains, and the take-back does not linger"
    );
}
