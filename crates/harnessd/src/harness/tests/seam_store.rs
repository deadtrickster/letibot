//! The round seam's store ordering: the round's results are stored before the boundary's side
//! work, so a busy-store wait the seam's own write makes comes before the runs that follow it in
//! the seam, never after them.

use super::*;

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Hold the store's write lock until the sender drops or says go.
fn hold_the_write_lock(
    path: &std::path::Path,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (held, is_held) = std::sync::mpsc::channel::<()>();
    let path = path.to_path_buf();
    let h = std::thread::spawn(move || {
        let c = letibot_tokencore::rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("BEGIN IMMEDIATE").unwrap();
        held.send(()).unwrap();
        let _ = wait.recv();
        c.execute_batch("COMMIT").unwrap();
    });
    is_held.recv().unwrap();
    (go, h)
}

/// The seam stores the round's results before its side work: the operator's queued call runs
/// between the append and the store write on an unfixed seam, and after it once the write leads.
/// A held lock makes the write itself the marker, as the wait it suffers is said on the log.
#[test]
fn a_round_is_stored_before_reviews_run() {
    let dir = Scratch::new("r341-seam");
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let path = dir.0.join("sessions.db");
    // The queue's own session, so the seam's review pass runs as the real one.
    let sid = crate::sessions::gatekeeper::session(std::path::Path::new("/tmp"));
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = sid.clone();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new(&sid)).expect("the session opens");

    std::fs::write(dir.0.join("a-file.txt"), "contents\n").expect("the file the call reads");
    let head = h.hub.attach("tui", "dead", Default::default(), 0);
    h.hub.submit(
        &head.head_id,
        "probe-1",
        0,
        letibot_sessionlog::hub::CommandKind::OperatorCall {
            call_id: "probe-1".into(),
            name: "read".into(),
            arguments: r#"{"path":"a-file.txt"}"#.into(),
            who: "dead".into(),
            execute: true,
        },
    );

    let (release, holder) = hold_the_write_lock(&path);
    let releaser = {
        let hub = h.hub.clone();
        std::thread::spawn(move || {
            // The seam's write waits on the held lock, and the wait is only said once it has run
            // past a second: that line is the fact, not a number standing in for it.
            assert!(
                crate::testing::wait_until(crate::testing::PATIENCE, || hub
                    .retained()
                    .iter()
                    .any(|e| matches!(&e.event,
                        SessionEvent::Warning { code, .. } if code == "store_busy_wait")))
            );
            release.send(()).unwrap();
        })
    };
    let results = vec![TranscriptItem::ToolResult {
        call_id: "call-seam".into(),
        name: "read".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: "the round's own result".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
        ran_ms: None,
        decision: None,
    }];
    h.after_the_round(&results, "t-seam", &[], None, &mut 3)
        .expect("the seam runs");
    releaser.join().unwrap();
    holder.join().unwrap();

    let said = h.hub.retained();
    let at = |code: &str| {
        said.iter().position(|e| {
            matches!(
                &e.event,
                SessionEvent::Warning { code: c, .. } if c == code
            )
        })
    };
    let [busy, ran] = [at("store_busy_wait"), at("operator_call_ran")];
    assert!(
        ran.is_some_and(|i| i > 0),
        "the operator's queued call never ran at the seam"
    );
    let (busy, ran) = (
        busy.expect("the seam's store write never waited on the held lock"),
        ran.unwrap(),
    );
    assert!(
        busy < ran,
        "the seam stored the round's rows only after its side work had run: \
         store_busy_wait at {busy}, the operator's call at {ran}"
    );

    // And the wait was for the round's own rows, which are in the store once the lock frees.
    let other = Store::open_private(&path).expect("the store opens");
    let tool_results: i64 = other
        .connection_of("transcript")
        .query_row(
            "SELECT COUNT(*) FROM transcript_item ti \
             JOIN transcript t ON t.id = ti.transcript_id \
             WHERE t.session_id = ?1 AND ti.kind = 'tool_result'",
            [&sid],
            |row| row.get(0),
        )
        .expect("the store is read");
    assert!(
        tool_results >= 1,
        "the round's results never reached the store: {tool_results} tool_result rows"
    );
}
