//! **An operator's mark, end to end** (TODO R81): a head sends `ClientFrame::Feedback`, the
//! connection thread places it on the turn and round the hub says is running — or on the turn
//! a read-back row was written in — and it lands in a temp `metrics.db` with the head's press
//! time. Nothing drains this hub's command queue: the worker is, in effect, pinned for the whole
//! test, which is the case the frame exists for.

use std::time::Duration;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve;

fn temp(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("letibot-fb-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_mark_lands_in_metrics_db_on_the_running_turn_and_on_a_read_back_row() {
    let dir = temp("db");
    let db = dir.join("metrics.db");
    letibot_metrics::store::start(
        &db,
        letibot_metrics::store::Identity {
            daemon: "test/1/0".into(),
            build: "test".into(),
        },
        letibot_metrics::store::Settings::default(),
    )
    .expect("metrics.db opens")
    .expect("installed");

    let hub = Hub::new("s-fb");
    // Two rounds of one prompt (one `began_ms`), and a row written in the first.
    for (id, row) in [("tr#1", Some("tr.1")), ("tr#2", None)] {
        hub.publish(SessionEvent::TurnStarted {
            turn_id: id.into(),
            model: "m".into(),
            ledger_head: String::new(),
            began_ms: Some(1_000),
        });
        if let Some(row) = row {
            hub.publish(SessionEvent::TranscriptAppended {
                item_id: row.into(),
                kind: "assistant".into(),
                ledger_head: String::new(),
            });
        }
    }
    let server = serve(hub.clone(), dir.join("s.sock")).expect("bind");
    let (mut head, _hello, mut reader) =
        HeadClient::attach(server.path(), "s-fb", 0, "tui", "dead", Caps::default())
            .expect("attach");

    let pressed = letibot_metrics::now_ms() - 1_500;
    head.feedback(pressed, "bad", "#distracted got pulled off by j975", None)
        .unwrap();
    head.feedback(pressed, "good", "", Some("tr.1")).unwrap();

    // Both confirmations arrive, and neither waited for a worker.
    let mut said = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while said.len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "no confirmation: {said:?}"
        );
        let f: ServerFrame = reader.read().expect("frame");
        if let ServerFrame::Event(env) = f
            && let SessionEvent::Warning { code, detail, .. } = &env.event
            && code == "feedback"
        {
            said.push(detail.clone());
        }
    }
    assert!(
        said[0].starts_with("marked bad · turn tr#1, round 2"),
        "{said:?}"
    );
    assert!(said[0].contains("#distracted"), "{said:?}");
    assert!(
        said[1].contains("round 1"),
        "the read-back row's round: {said:?}"
    );

    assert!(letibot_metrics::flush(Duration::from_secs(5)));
    let c = rusqlite_open(&db);
    let rows: Vec<(String, i64, String, i64, String)> = {
        let mut st = c
            .prepare(
                "SELECT name, ts_ms, turn_id, round, attrs->>'tags' FROM event \
                 WHERE kind = 'operator.feedback' ORDER BY id",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert_eq!(
        rows,
        vec![
            (
                "bad".into(),
                pressed as i64,
                "tr#1".into(),
                1,
                "[\"distracted\"]".into()
            ),
            ("good".into(), pressed as i64, "tr#1".into(), 0, "[]".into()),
        ],
        "the head's press time, the prompt's turn (its first round's id), the round"
    );
}

fn rusqlite_open(p: &std::path::Path) -> letibot_metrics::store::Connection {
    letibot_metrics::store::Connection::open(p).unwrap()
}
