//! Many sessions on one socket, driven the way a head drives them: two sessions share nothing,
//! a head joining mid-turn gets its text with no gap or duplicate, and an unwatched session
//! keeps going.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};
use letibot_sessionlog::testing::*;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Short, because macOS caps a socket path at 104 bytes and its temp dir alone is about
    // 50; the tag is for a human reading `ls`, so ten characters do.
    letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-ses-{:.10}-{}-{:x}.sock",
        tag,
        std::process::id(),
        n as u32
    ))
}

fn wiring() -> SessionWiring {
    SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: "/home/dead/Projects/letibot".into(),
    }
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create("a", "the cache question", wiring()).unwrap();
    r.create("b", "", wiring()).unwrap();
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

/// Read frames until `f` says stop, or time out with what was seen.
fn until(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    mut f: impl FnMut(&ServerFrame) -> bool,
) -> Vec<ServerFrame> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let frame = frame.frame();
                let done = f(&frame);
                seen.push(frame);
                if done {
                    return seen;
                }
            }
            // A quiet 500 ms is not a dead channel: only a sender that is gone ends the wait
            // early, and a timeout goes round again until the deadline. Breaking on any error
            // made the deadline above decorative and the real tolerance half a second.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!(
        "the frame never arrived; saw {:?}",
        seen.iter().map(frame_kind).collect::<Vec<_>>()
    );
}

fn frame_kind(f: &ServerFrame) -> String {
    match f {
        ServerFrame::Hello { session_id, .. } => format!("Hello({session_id})"),
        ServerFrame::Event(e) => format!("Event({})", e.event.kind()),
        ServerFrame::Sessions { current, .. } => format!("Sessions(current={current})"),
        ServerFrame::Todos { session_id, .. } => format!("Todos({session_id})"),
        ServerFrame::Jobs {
            session_id, jobs, ..
        } => {
            format!("Jobs({session_id}, {} entries)", jobs.len())
        }
        ServerFrame::StandingNotes {
            session_id, notes, ..
        } => {
            format!("StandingNotes({session_id}, {} notes)", notes.len())
        }
        ServerFrame::Skills { session_id, skills } => {
            format!("Skills({session_id}, {} rows)", skills.len())
        }
        ServerFrame::RowFetched {
            row, body, total, ..
        } => format!(
            "RowFetched(row {row}, {} of {total})",
            body.as_ref().map(|b| b.len()).unwrap_or(0)
        ),
        ServerFrame::RowsFetched { from, rows, .. } => {
            format!("RowsFetched(from {from}, {} rows)", rows.len())
        }
        // The verdicts are counted too: a queue pane draws an entry's review beside it, so a
        // frame that arrived with one half of the pair and not the other is worth seeing in
        // the diagnostic rather than only in the pane.
        ServerFrame::Queues { snapshot } => format!(
            "Queues({} reviews, {} merges)",
            snapshot.reviews.len(),
            snapshot.merges.len()
        ),
        ServerFrame::RoomChat {
            client_request_id, ..
        } => format!("RoomChat({client_request_id})"),
        // The pushed message is named by its seq: a diagnostic showing two of the same number was
        // the double-merge this frame has to avoid.
        ServerFrame::RoomChatPushed { message, names_you } => format!(
            "RoomChatPushed(#{}, names_you={names_you})",
            message
                .get("seq")
                .and_then(|s| s.as_u64())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        ServerFrame::Peeked {
            session_id, events, ..
        } => format!("Peeked({}, {} events)", session_id, events.len()),
        ServerFrame::Diagnostic {
            request_id,
            body,
            total,
            ..
        } => format!(
            "Diagnostic({request_id}, {} of {total})",
            body.as_ref().map(|b| b.len()).unwrap_or(0)
        ),
        ServerFrame::Settings { rows } => format!("Settings({} rows)", rows.len()),
        ServerFrame::ShellSuggestions { prefix, lines, .. } => {
            format!("ShellSuggestions({prefix}, {} lines)", lines.len())
        }
        ServerFrame::TermAttached { pane, command, .. } => {
            format!("TermAttached({pane} {command})")
        }
        ServerFrame::TermOutput { pane, bytes } => {
            format!("TermOutput({pane}, {} bytes)", bytes.len())
        }
        ServerFrame::TermEnded { reason, .. } => format!("TermEnded({reason})"),
        // The read's answer, named by what it says rather than by the frame: an empty list
        // is a session with no pane, and that is the half a diagnostic wants to see.
        ServerFrame::TermStatus { panes } if panes.is_empty() => "TermStatus(none)".into(),
        ServerFrame::TermStatus { panes } => format!(
            "TermStatus({})",
            panes
                .iter()
                .map(|b| format!("{} {}", b.pane, b.command))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ServerFrame::Resync { .. } => "Resync".into(),
        // The two of a mode confirmation, named by their ids: a diagnostic that saw one
        // without the other is exactly what this function exists to show.
        ServerFrame::ModeConfirm { req_id, name, .. } => {
            format!("ModeConfirm({req_id}, {name})")
        }
        ServerFrame::ModeSettled {
            req_id,
            applied,
            answer,
            ..
        } => format!("ModeSettled({req_id}, {answer}, applied={applied})"),
        ServerFrame::Accepted { .. } => "Accepted".into(),
        ServerFrame::Rejected { reason, .. } => format!("Rejected({reason})"),
        ServerFrame::Bye { reason } => format!("Bye({reason})"),
        // Never the secret itself, not even in a test's diagnostic: the whole
        // point of the frame is that the password goes to the waiting askpass
        // connection and nowhere else, and a panic message is somewhere else.
        ServerFrame::Secret { secret, .. } => {
            format!("Secret(given={})", secret.is_some())
        }
    }
}

#[test]
fn a_head_switches_session_on_one_connection_and_is_reseated() {
    let (reg, server) = start("switch");
    let a = reg.get("a").unwrap();
    let b = reg.get("b").unwrap();
    a.publish(turn_started("t-a"));
    a.publish(delta("t-a", "this is A"));
    b.publish(turn_started("t-b"));
    b.publish(delta("t-b", "this is B"));

    let (mut client, hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    let ServerFrame::Hello {
        session_id,
        head_id: first_head,
        snapshot,
        wiring: w,
        sessions,
        ..
    } = hello
    else {
        panic!("the first frame must be Hello")
    };
    assert_eq!(session_id, "a");
    assert_eq!(snapshot.unwrap().turn.unwrap().text, "this is A");
    // The head can name what it is talking to before any turn of its own: the model renders
    // in the session header, while the dialect and the endpoint are carried and rendered
    // nowhere.
    assert_eq!(w.model, "qwen-3.8-flash-next");
    // And it is handed the picker's contents by the attach, not by a second trip.
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].title, "the cache question");

    client.switch("b", 0).unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Hello { .. }));
    // The switch does not look like a shutdown. A `Bye` here would close the window, which
    // is what the pump's `switching` flag exists to prevent.
    assert!(
        !frames.iter().any(|f| matches!(f, ServerFrame::Bye { .. })),
        "a switch must not read as a daemon going away: {:?}",
        frames.iter().map(frame_kind).collect::<Vec<_>>()
    );
    let ServerFrame::Hello {
        session_id,
        head_id: second_head,
        snapshot,
        ..
    } = frames.last().unwrap().clone()
    else {
        unreachable!()
    };
    assert_eq!(session_id, "b");
    assert_eq!(snapshot.unwrap().turn.unwrap().text, "this is B");
    // The head really moved: registered in B and gone from A. Not an assertion that the id
    // changed, since head ids are minted per hub and B's first head is `h1` as A's was, but
    // the client is still handed the new one.
    assert_eq!(a.attached_heads(), 0, "the head is still registered in A");
    assert_eq!(b.attached_heads(), 1, "the head never arrived in B");
    assert!(!second_head.is_empty());
    let _ = first_head;

    // Session A carried on with nobody watching, and coming back proves it.
    a.publish(delta("t-a", " — and it kept going"));
    client.switch("a", 0).unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Hello { .. }));
    let ServerFrame::Hello { snapshot, .. } = frames.last().unwrap().clone() else {
        unreachable!()
    };
    assert_eq!(
        snapshot.unwrap().turn.unwrap().text,
        "this is A — and it kept going"
    );

    let _ = client.detach();
    server.shutdown();
    let _ = pumping.join();
}

#[test]
fn events_in_one_session_never_reach_a_head_in_another() {
    let (reg, server) = start("isolate");
    let a = reg.get("a").unwrap();
    let b = reg.get("b").unwrap();

    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "b", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    for i in 0..50 {
        a.publish(SessionEvent::Delta {
            turn_id: "t-a".into(),
            target: DeltaTarget::Text,
            text: format!("a{i} "),
        });
    }
    // One event in B, after fifty in A. If anything leaked, it arrives first.
    b.publish(warn("the only thing this head should see"));

    let frames = until(
        &rx,
        |f| matches!(f, ServerFrame::Event(e) if matches!(&e.event, SessionEvent::Warning { .. })),
    );
    let deltas = frames
        .iter()
        .filter(
            |f| matches!(f, ServerFrame::Event(e) if matches!(e.event, SessionEvent::Delta { .. })),
        )
        .count();
    assert_eq!(deltas, 0, "a head in B was delivered A's stream");
    for f in &frames {
        if let ServerFrame::Event(e) = f {
            assert_eq!(e.session_id, "b", "an envelope from the wrong session");
        }
    }

    let _ = client.detach();
    server.shutdown();
    let _ = pumping.join();
}

#[test]
fn a_head_that_names_a_session_this_daemon_does_not_hold_is_refused_by_name() {
    // Not seated in the default session: a typo that puts you in somebody else's
    // conversation looks exactly like a working attach to an empty one, and you
    // find out by prompting into it.
    let (_reg, server) = start("unknown");
    let stream = std::os::unix::net::UnixStream::connect(server.path()).unwrap();
    let mut reader = FrameReader::new(stream.try_clone().unwrap());
    let mut writer = FrameWriter::new(stream);
    writer
        .write(&ClientFrame::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: "typo".into(),
            since_seq: 0,
            kind: "tui".into(),
            identity: "dead".into(),
            caps: Caps::default(),
        })
        .unwrap();
    match reader.read::<ServerFrame>().unwrap() {
        ServerFrame::Bye { reason } => {
            assert!(reason.contains("no such session"), "{reason}");
            // And it says what it does hold, so the next attempt is informed.
            assert!(reason.contains('a') && reason.contains('b'), "{reason}");
        }
        other => panic!("expected a refusal, got {}", frame_kind(&other)),
    }
    server.shutdown();
}

#[test]
fn a_head_can_make_a_session_and_the_daemon_mints_the_id() {
    let (reg, server) = start("new");
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    client
        .new_session("a third thing", "/home/dead/Projects/letibot")
        .unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions {
        sessions,
        current,
        created,
    } = frames.last().unwrap().clone()
    else {
        unreachable!()
    };
    // The head is told which id was minted and stays where it is: create and go are separate
    // acts, so a head that wants one ready for later does not have to leave the session it is in.
    let id = created.expect("a NewSession is answered with the id it made");
    assert_eq!(current, "a");
    assert_eq!(sessions.len(), 3);
    assert!(reg.get(&id).is_some(), "the session is really there");
    assert_eq!(reg.brief(&id).unwrap().title, "a third thing");
    // A brand-new session inherits the daemon's wiring rather than showing blanks.
    assert_eq!(reg.wiring(&id).model, "qwen-3.8-flash-next");

    let _ = client.detach();
    server.shutdown();
    let _ = pumping.join();
}

#[test]
fn one_head_resuming_another_session_leaves_the_head_that_was_there() {
    // The daemon cannot move a head that did not ask: the seat is the connection's, so a
    // second connection's resume and switch leave the first where it was. The DEFAULT does
    // move, which is why this is worth pinning - a bare attach lands where the last switch left it.
    let (reg, server) = start("twoheads");
    let a = reg.get("a").unwrap();
    let b = reg.get("b").unwrap();
    a.publish(turn_started("t-a"));
    a.publish(delta("t-a", "this is A"));

    // The head the operator is looking at, in A.
    let (mut first, hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let ServerFrame::Hello { session_id, .. } = hello else {
        panic!("the first frame must be Hello")
    };
    assert_eq!(session_id, "a");
    let (tx1, rx1) = std::sync::mpsc::channel();
    let pumping1 = std::thread::spawn(move || pump(reader, tx1));

    // The second invocation: a bare attach (the launcher's head sends no session id), the
    // resume, and the switch to what the daemon named.
    let (mut second, hello2, reader2) =
        HeadClient::attach(server.path(), "", 0, "tui", "dead", Caps::default()).unwrap();
    let ServerFrame::Hello { session_id, .. } = hello2 else {
        panic!("the first frame must be Hello")
    };
    assert_eq!(
        session_id, "a",
        "a bare attach lands on the daemon's current session"
    );
    let (tx2, rx2) = std::sync::mpsc::channel();
    let pumping2 = std::thread::spawn(move || pump(reader2, tx2));

    second.resume_session("b").unwrap();
    let frames = until(&rx2, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions {
        created, current, ..
    } = frames.last().unwrap().clone()
    else {
        unreachable!()
    };
    assert_eq!(created.as_deref(), Some("b"));
    assert_eq!(
        current, "a",
        "the resume alone does not move the head that asked"
    );
    second.switch("b", 0).unwrap();
    until(&rx2, |f| matches!(f, ServerFrame::Hello { .. }));

    // What the second head did to the daemon: the default moved to B, and B has one head.
    assert_eq!(
        reg.default_id(),
        "b",
        "the last switch is what a bare attach lands on"
    );
    assert_eq!(a.attached_heads(), 1, "A kept the head it had");
    assert_eq!(b.attached_heads(), 1, "and B has the one that asked for it");

    // And the first head is where it was: no `Hello` was written down its socket, so it was
    // not reseated. It does hear the second head's arrival in A, which is a different fact.
    a.publish(delta("t-a", " — and still mine"));
    let frames = until(
        &rx1,
        |f| matches!(f, ServerFrame::Event(e) if matches!(&e.event, SessionEvent::Delta { text, .. } if text.ends_with("still mine"))),
    );
    for f in &frames {
        if let ServerFrame::Event(e) = f {
            assert_eq!(e.session_id, "a", "an envelope from the wrong session");
        }
        assert!(
            !matches!(f, ServerFrame::Hello { .. }),
            "a second Hello is a head being reseated"
        );
    }

    let _ = first.detach();
    let _ = second.detach();
    server.shutdown();
    let _ = pumping1.join();
    let _ = pumping2.join();
}

#[test]
fn a_session_a_head_made_is_recorded_as_the_operator_s_or_a_package_s_by_who_asked() {
    // The one fact a session's row cannot otherwise say: a conversation a person's terminal
    // opened and one a package opened are the same shape of row, and the picker groups them
    // apart by this. The `kind` and `identity` the `ATTACH` carried already decide it.
    let (reg, server) = start("origins");

    // A package's head.
    let (mut mcp, _hello, reader) = HeadClient::attach(
        server.path(),
        "a",
        0,
        "remote",
        "letibot-mcp",
        Caps::default(),
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    mcp.new_session("a landing page", "/w").unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = frames.last().unwrap().clone() else {
        unreachable!()
    };
    let package = created.expect("the id it minted");
    assert_eq!(
        reg.origin_of(&package),
        letibot_sessionlog::SessionOrigin::Package,
        "a session `letim` started is a package's, and its row is what a picker tomorrow reads"
    );
    let row = reg
        .list()
        .into_iter()
        .find(|b| b.session_id == package)
        .expect("the row in the list");
    assert_eq!(row.origin, letibot_sessionlog::SessionOrigin::Package);

    // The operator's own, from the same daemon a moment later.
    let (mut tui, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping2 = std::thread::spawn(move || pump(reader, tx));
    tui.new_session("a question", "/w").unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = frames.last().unwrap().clone() else {
        unreachable!()
    };
    let mine = created.expect("the id it minted");
    assert_eq!(
        reg.origin_of(&mine),
        letibot_sessionlog::SessionOrigin::Operator,
        "and a terminal's is the operator's — the default, and what every row was before this"
    );

    let _ = mcp.detach();
    let _ = tui.detach();
    server.shutdown();
    let _ = pumping.join();
    let _ = pumping2.join();
}

#[test]
fn switching_into_a_session_whose_turn_is_running_loses_no_byte_and_repeats_none() {
    // The mid-turn attach, reached by a switch rather than a connect: the snapshot carries
    // the accumulated text once, and the very next event is `snapshot.seq + 1`.
    let (reg, server) = start("midturn");
    let b = reg.get("b").unwrap();
    b.publish(turn_started("t-b"));

    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    for i in 0..200 {
        b.publish(delta("t-b", &format!("{i} ")));
    }
    client.switch("b", 0).unwrap();
    let frames = until(&rx, |f| matches!(f, ServerFrame::Hello { .. }));
    let ServerFrame::Hello { snapshot, .. } = frames.last().unwrap().clone() else {
        unreachable!()
    };
    let snapshot = *snapshot.expect("a switch with since_seq 0 gets a snapshot");
    let so_far = snapshot.turn.unwrap().text;
    let at = snapshot.seq;

    b.publish(delta("t-b", "END"));
    let frames = until(
        &rx,
        |f| matches!(f, ServerFrame::Event(e) if matches!(&e.event, SessionEvent::Delta { text, .. } if text == "END")),
    );
    let first = frames
        .iter()
        .find_map(|f| match f {
            ServerFrame::Event(e) => Some(e.seq),
            _ => None,
        })
        .expect("an event after the switch");
    assert_eq!(first, at + 1, "a gap or a duplicate across the switch");

    let expected: String = (0..200).map(|i| format!("{i} ")).collect();
    assert_eq!(
        so_far, expected,
        "the accumulated text was not handed over whole"
    );

    let _ = client.detach();
    server.shutdown();
    let _ = pumping.join();
}

#[test]
fn one_worker_serving_two_sessions_takes_them_in_the_order_they_were_prompted() {
    // The bell's whole reason for carrying ids rather than being a bare wake: a
    // session further down the registry must not be structurally later than one
    // with a faster head.
    let (reg, server) = start("order");
    let (mut a_head, _h1, ra) =
        HeadClient::attach(server.path(), "a", 0, "tui", "alice", Caps::default()).unwrap();
    let (mut b_head, _h2, rb) =
        HeadClient::attach(server.path(), "b", 0, "tui", "bob", Caps::default()).unwrap();
    let (txa, _rxa) = std::sync::mpsc::channel();
    let (txb, _rxb) = std::sync::mpsc::channel();
    let pa = std::thread::spawn(move || pump(ra, txa));
    let pb = std::thread::spawn(move || pump(rb, txb));

    b_head.prompt(0, "for b").unwrap();
    // Wait for it to land before sending the second, so the assertion is about the
    // registry's ordering and not about two sockets racing.
    let deadline = Instant::now() + Duration::from_secs(5);
    while reg.get("b").unwrap().head_seq() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    a_head.prompt(0, "for a").unwrap();
    while reg.get("a").unwrap().head_seq() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let (first, _) = reg.next_command().expect("a command");
    let (second, _) = reg.next_command().expect("a command");
    assert_eq!((first.as_str(), second.as_str()), ("b", "a"));

    let _ = a_head.detach();
    let _ = b_head.detach();
    server.shutdown();
    let _ = pa.join();
    let _ = pb.join();
}

/// A stop closes the daemon while the worker is busy, which is why it does not travel through
/// the command queue: one worker drains that queue and a running turn owns it, so a stop asked
/// for mid-turn would sit behind the turn. A queued command nobody drains IS the busy case.
#[test]
fn a_stop_closes_the_registry_even_with_a_command_queued_and_nobody_draining() {
    let (reg, server) = start("stopnow");
    let _a = reg.get("a").unwrap();

    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));

    // A command nobody will ever drain: this is the worker being busy.
    client
        .prompt(0, "a question that will never be answered")
        .unwrap();
    assert!(!reg.is_closed(), "still open with work queued");

    // The other head, which must be told before the socket goes.
    let (_other, _h2, reader2) =
        HeadClient::attach(server.path(), "a", 0, "tui", "someone", Caps::default()).unwrap();
    let (tx2, rx2) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader2, tx2));

    client.stop(0, "dead").unwrap();

    // The head that asked is acknowledged by name, with the NOTE and not a bare acceptance,
    // and before anything closes: the note is what says its request was READ rather than
    // merely written, and after the close there is no socket to answer on.
    let acked = until(&rx, |f| {
        matches!(f, ServerFrame::Accepted { note, .. }
            if note == letibot_sessionlog::NOTE_STOPPING)
    });
    assert!(
        acked
            .iter()
            .any(|f| matches!(f, ServerFrame::Accepted { note, .. }
            if note == letibot_sessionlog::NOTE_STOPPING)),
        "the asking head was never told `stopping`: {acked:#?}"
    );

    // The registry closes, and it does not wait for the queue.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !reg.is_closed() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(reg.is_closed(), "the stop did not reach the registry");

    // And the other head was told who did it, while the hub was still up.
    let seen = until(&rx2, |f| {
        matches!(f, ServerFrame::Event(e)
            if matches!(&e.event, SessionEvent::Warning { code, detail, .. }
                if code == "daemon_stopping" && detail.contains("dead")))
    });
    assert!(
        seen.iter().any(|f| matches!(f, ServerFrame::Event(e)
            if matches!(&e.event, SessionEvent::Warning { code, .. } if code == "daemon_stopping"))),
        "the other head never heard why the daemon went: {seen:#?}"
    );
    let _ = rx;
}

#[test]
fn the_session_a_package_opened_a_fresh_daemon_on_is_the_packages_whichever_door_it_was() {
    // The daemon opens its first session eagerly as the operator's; the first attach is what says
    // who it is for. A later attach by the bridge to a session a terminal opened first changes nothing.
    let (reg, server) = start("first-attach");
    let (mut mcp, _h, _r) = HeadClient::attach(
        server.path(),
        "",
        0,
        "remote",
        "letibot-mcp",
        Caps::default(),
    )
    .unwrap();
    assert_eq!(
        reg.origin_of("a"),
        letibot_sessionlog::SessionOrigin::Package,
        "the default session opened by the bridge is a package's"
    );
    let _ = mcp.detach();
    server.shutdown();

    let (reg, server) = start("first-attach-tui");
    let (_tui, _h, _r) =
        HeadClient::attach(server.path(), "", 0, "tui", "dead", Caps::default()).unwrap();
    let (_mcp, _h2, _r2) = HeadClient::attach(
        server.path(),
        "a",
        0,
        "remote",
        "letibot-mcp",
        Caps::default(),
    )
    .unwrap();
    assert_eq!(
        reg.origin_of("a"),
        letibot_sessionlog::SessionOrigin::Operator,
        "a terminal's session stays the operator's when the bridge joins it later"
    );
    server.shutdown();
}
