//! What a monitor firing becomes: one delivery, folded per monitor, typed as news and never as
//! somebody's words - the half a `Woken` ring does not decide.

use super::*;

fn fired(name: &str, why: &str) -> letibot_tools::exec::monitor::Firing {
    use letibot_tools::exec::monitor::{Fired, Firing};
    Firing {
        name: name.into(),
        watch: "a path".into(),
        declared_by: "t-1".into(),
        fired: Fired::Fired {
            why: why.into(),
            at: std::time::SystemTime::UNIX_EPOCH,
            after: std::time::Duration::from_secs(2),
        },
        at: std::time::SystemTime::UNIX_EPOCH,
    }
}

/// A burst is one delivery, not a wall: a session with a watch that holds every tick would
/// otherwise be handed a line a firing, and the model pays for every one of them in a turn it did
/// not ask for. One line a monitor, the newest words, and how many the line stands for.
#[test]
fn a_burst_of_firings_is_one_line_a_monitor_that_counts_the_rest() {
    let mut burst: Vec<_> = (0..20)
        .map(|n| fired("chatty", &format!("the file changed ({n})")))
        .collect();
    burst.push(fired("build", "j1 exited 0"));
    let text = monitor_notice(&burst).expect("a firing is news");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "one line a monitor, not one a firing: {text}"
    );
    assert!(
        lines[0].contains("`chatty`") && lines[0].contains("(and 19 firings since)"),
        "the burst says what it stands for: {text}"
    );
    assert!(
        lines[0].contains("the file changed (19)"),
        "and the newest words, not the first: {text}"
    );
    assert!(
        lines[1].contains("`build`") && !lines[1].contains("firings"),
        "a monitor that fired once is not told it fired a burst: {text}"
    );
}

/// An ending keeps its own line, because it says the one thing a firing's words cannot: nothing
/// is watching any more. Folding the two would read *expired without firing (and 19 firings since)*,
/// which is a contradiction the model would have to untangle for itself.
#[test]
fn a_burst_and_its_ending_are_two_lines_and_the_ending_is_not_folded_into_the_firings() {
    use letibot_tools::exec::monitor::Fired;
    let mut burst: Vec<_> = (0..3)
        .map(|n| fired("chatty", &format!("the file changed ({n})")))
        .collect();
    burst.push(letibot_tools::exec::monitor::Firing {
        fired: Fired::Expired {
            after: std::time::Duration::from_secs(30),
            last_seen: "no match".into(),
        },
        ..fired("chatty", "the file changed at the end")
    });
    let text = monitor_notice(&burst).expect("a firing is news");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "the burst, and then the ending: {text}");
    assert!(
        lines[0].contains("fired") && lines[0].contains("(and 2 firings since)"),
        "the burst is one line with its count: {text}"
    );
    assert!(
        lines[1].contains("expired without firing") && !lines[1].contains("firings since"),
        "the ending stands alone and claims nothing about the burst: {text}"
    );
}

/// A firing is a delivery, not a message. The row it becomes is a `Notice`: it carries a kind,
/// and there is no `speaker` field for anybody to leave at its default and hand the operator words
/// they never said.
#[test]
fn a_firing_is_a_notice_and_never_a_row_of_somebodys_words() {
    let row = notice_row(
        letibot_transcript::NoticeKind::Monitor,
        "[monitor] a firing",
        None,
    );
    match &row {
        TranscriptItem::Notice { kind, text, .. } => {
            assert_eq!(
                *kind,
                letibot_transcript::NoticeKind::Monitor,
                "the kind says what the delivery is about"
            );
            assert_eq!(text, "[monitor] a firing");
        }
        other => panic!("a firing is not a delivery: {other:?}"),
    }
    assert!(
        !matches!(row, TranscriptItem::User { .. }),
        "a firing must not be a user row, whatever its speaker says"
    );
}

/// And the mid-turn door says the same thing: a firing that arrives while a turn runs is a notice
/// there too, or the same news reads as the operator's words on one door and not the other.
#[test]
fn a_firing_heard_mid_turn_is_a_notice_as_well() {
    let m = letibot_turn::steering::SteeringMessage::notice("[monitor] a firing");
    assert!(
        matches!(m.to_item(), TranscriptItem::Notice { .. }),
        "the steering door must deliver a notice as a notice"
    );
    // A notice is not coalesced with the operator's own queued words: it stands alone, because a
    // person's prompt and the harness's news are not one message.
    assert!(!m.from_operator);
}

/// The whole chain on a real monitor: twenty firings are one delivery carrying all of them, and
/// the step boundary that asks twice cannot make two. Nothing is dropped either - every firing is in
/// the registry, and the count on the line is the proof the model was told about the burst.
#[test]
fn a_burst_is_one_delivery_and_it_is_taken_once() {
    use letibot_tools::exec::monitor::{DEFAULT_TTL, Monitors, Watch};
    use letibot_tools::exec::scope::{ScopeId, ScopeKind};
    use letibot_turn::steering::SteeringSource;

    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "firing-burst-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("out.log");
    let ms = Arc::new(Monitors::new());
    ms.declare(
        "chatty",
        ScopeId {
            kind: ScopeKind::Session,
            name: "t".into(),
            path: dir.clone(),
        },
        Watch::Path(log.clone()),
        None,
        "t-1",
        DEFAULT_TTL,
        true,
    )
    .unwrap();
    // Appended, not rewritten: `fs::write` truncates first, and the monitor's own poller can
    // see the empty file between the two calls and fire for it.
    for n in 0..20 {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        f.write_all(format!("line {n}\n").as_bytes()).unwrap();
        ms.tick();
    }
    assert_eq!(
        ms.firings().len(),
        20,
        "every firing is recorded: the bound is on the ring, not on the record"
    );

    let mut s = HubSteering::new(letibot_sessionlog::hub::Hub::new("s-burst"));
    s.monitors = Some(Arc::clone(&ms));
    let m = s.try_next().expect("the burst is news");
    assert_eq!(
        m.text.lines().count(),
        1,
        "one line for the burst, not one a firing: {}",
        m.text
    );
    assert!(
        m.text.contains("(and 19 firings since)"),
        "the line says how many it stands for: {}",
        m.text
    );
    assert!(
        s.try_next().is_none(),
        "a firing already delivered is not delivered again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
