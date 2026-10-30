//! **The TUI's one-line cards for a restart and a stale prompt are pinned to what the daemon
//! writes** — the operator, pasting what pg-noop's head drew after its restart onto a new build:
//! *"these gray walls are hilarious … make them the usual one liners"*.
//!
//! `restart_card` and `stale_card` in the TUI parse these files to draw each notice as one line;
//! a reworded notice fails here before it falls back on a screen. Re-bless with
//! `LETIBOT_BLESS_RESTART_CARD=1`, and teach the card the new shape if it moved a part it keys
//! on.

use crate::restart::{
    Outcome, RESTORE_WITHIN, Stopped, header, job_line, monitor_rearmed_line,
    monitor_reminded_line, notice, said, warning_detail,
};
use crate::subagent_record::{parent_reminded_line, parent_resumed_line};
use letibot_tokencore::store::{JobRecord, StoredMonitor};

/// 2026-10-12 08:10:56 UTC — when pg-noop's daemon came back.
const BACK: u64 = 1_791_792_656_000;

/// The operator's job: the command as their paste shows it, its elisions filled in.
const ACCEPTANCE: &str = "cd /tmp/letibot-scratch-1023699/harness && LETIDB_MODE=memory \
                          LETIBOT_ACCEPTANCE=1 ./run_acceptance.sh --all > \
                          /tmp/acceptance-child.log 2>&1; echo \"exit=$?\"; tail -5 \
                          /tmp/acceptance-child.log";

fn running(handle: &str, command: &str) -> JobRecord {
    JobRecord {
        handle: handle.into(),
        command: command.into(),
        how: String::new(),
        state: "running".into(),
        produced: 0,
        elapsed_ms: 0,
        // A running row's redirect, as every build before this one left it.
        redirect: None,
    }
}

fn monitor(name: &str, args: &str) -> StoredMonitor {
    StoredMonitor {
        session_id: "s".into(),
        name: name.into(),
        args_json: args.into(),
        instance: "d-old".into(),
        ttl_ms: 600_000,
        live: true,
        declared_ms: 1,
        updated_ms: 1,
    }
}

fn pin(name: &str, text: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tui/src/app/tests/fixtures")
        .join(name);
    if std::env::var_os("LETIBOT_BLESS_RESTART_CARD").is_some() {
        std::fs::write(&path, text).unwrap();
    }
    let pinned =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        text,
        pinned,
        "the daemon no longer writes what the TUI's card parses ({})",
        path.display()
    );
}

#[test]
fn the_tuis_restart_card_fixtures_are_what_the_daemon_writes() {
    // **The operator's restart**: the first start on a new build, whose old daemon kept no
    // record — and one job.
    let unknown = Stopped::Unknown { seen_ms: 0 };
    let lines = vec![job_line(&running("j46", ACCEPTANCE), None, None, None, &[])];
    let text = notice(
        &header(&unknown, BACK, RESTORE_WITHIN, Outcome::Reminded),
        &lines,
        Outcome::Reminded,
    );
    pin("restart_unknown_notice.txt", &text);
    pin(
        "restart_unknown_warning.txt",
        &warning_detail(&text, &[said::job("j46", 0)]),
    );

    // **Under the threshold, with things brought back**: two subagents, a monitor, and a job
    // that is only ever reminded.
    let stop = Stopped::At {
        at_ms: BACK - 252_000,
        estimated_from: None,
    };
    let (a, b) = (
        "`s-1-sub-2` (fix the flaky test)",
        "`s-1-sub-3` (port the parser)",
    );
    let built = monitor(
        "built",
        r#"{"name":"built","path":"out.txt","ttl_ms":600000}"#,
    );
    let lines = vec![
        parent_resumed_line(a),
        parent_resumed_line(b),
        monitor_rearmed_line(&built),
        job_line(
            &running("j9", "cargo build > build.log 2>&1"),
            Some("build"),
            Some("/w"),
            Some(95_000),
            &["`ship once the build is done`".into()],
        ),
    ];
    let text = notice(
        &header(&stop, BACK, RESTORE_WITHIN, Outcome::Restored),
        &lines,
        Outcome::Restored,
    );
    pin("restart_restored_notice.txt", &text);
    pin(
        "restart_restored_warning.txt",
        &warning_detail(
            &text,
            &[
                said::resumed(a),
                said::resumed(b),
                said::rearmed("built"),
                said::job("j9", 1),
            ],
        ),
    );

    // **Over the threshold, after a crash**: everything reminded.
    let crash = Stopped::At {
        at_ms: BACK - (3 * 3_600_000 + 12 * 60_000),
        estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
    };
    let lines = vec![
        parent_reminded_line(
            a,
            "s-1-sub-2",
            Some("/w/.worktrees/flaky"),
            Some("agent/flaky"),
        ),
        monitor_reminded_line(&built),
        job_line(&running("j9", "cargo build"), None, Some("/w"), None, &[]),
    ];
    let text = notice(
        &header(&crash, BACK, RESTORE_WITHIN, Outcome::Reminded),
        &lines,
        Outcome::Reminded,
    );
    pin("restart_late_notice.txt", &text);
    pin(
        "restart_late_warning.txt",
        &warning_detail(
            &text,
            &[
                said::subagent_reminded(a),
                said::monitor_reminded("built"),
                said::job("j9", 0),
            ],
        ),
    );

    // **Under the threshold with only a job**: nothing restored, and said so.
    let quick = Stopped::At {
        at_ms: BACK - 4_000,
        estimated_from: None,
    };
    let lines = vec![job_line(
        &running("j46", ACCEPTANCE),
        None,
        None,
        Some(61_000),
        &[],
    )];
    let text = notice(
        &header(&quick, BACK, RESTORE_WITHIN, Outcome::NothingRestorable),
        &lines,
        Outcome::NothingRestorable,
    );
    pin("restart_jobs_only_notice.txt", &text);
    pin(
        "restart_jobs_only_warning.txt",
        &warning_detail(&text, &[said::job("j46", 0)]),
    );

    // **A restart with nothing to restore** — the operator, 2026-10-30: *"mb always tell agent
    // about restart? with the current time lol"*. One sentence, no list, and its own code.
    let text = notice(
        &header(&quick, BACK, RESTORE_WITHIN, Outcome::Nothing),
        &[],
        Outcome::Nothing,
    );
    pin("restart_nothing_notice.txt", &text);
    pin("restart_nothing_warning.txt", &warning_detail(&text, &[]));
}

/// **The `prefix_stale` sentence pg-noop's head drew as a wall** — seated 09-20 17:51, a tool
/// (`task_resume`) seated since.
#[test]
fn the_tuis_stale_card_fixture_is_what_the_daemon_writes() {
    pin(
        "prefix_stale_warning.txt",
        &crate::harness::stale_prefix_sentence(
            "s-1789919514688401228",
            "09-20 17:51",
            &["the tool schemas"],
            &["task_resume".into()],
            &[],
            &[],
        ),
    );
    pin(
        "prefix_stale_many_warning.txt",
        &crate::harness::stale_prefix_sentence(
            "s-1789919514688401228",
            "09-20 17:51",
            &["the system instructions", "the tool schemas"],
            &["task_resume".into(), "pkill".into()],
            &["job_wait".into()],
            &["todo_write".into()],
        ),
    );
}
