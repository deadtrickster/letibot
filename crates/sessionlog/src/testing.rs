//! Event builders and a recorded session.
//!
//! Public rather than `#[cfg(test)]` on purpose. `docs/workstreams.md` W8→W6 says
//! the TUI *"renders an event stream. Give it a recorded event log as a fixture and
//! it is built and demoed before a turn engine exists."* [`recorded_session`] is
//! that fixture, and a fixture that only exists inside this crate's test binary is
//! a fixture the head cannot use.

use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};

use crate::event::{
    Decider, DecisionOption, DecisionOutcome, DeltaTarget, FinishReason, OnTimeout, OptionKind,
    PromptProgress, SessionEvent, Timings, TodoBy, TodoEntry, TodoStatus, Usage,
};

pub fn warn(detail: &str) -> SessionEvent {
    SessionEvent::Warning {
        code: "test".into(),
        detail: detail.into(),

        compaction: None,
    }
}

pub fn turn_started(turn_id: &str) -> SessionEvent {
    SessionEvent::TurnStarted {
        turn_id: turn_id.into(),
        model: "qwen3-next-80b".into(),
        ledger_head: "0000".into(),
        began_ms: None,
    }
}

pub fn progress(turn_id: &str) -> SessionEvent {
    SessionEvent::PromptProgress {
        turn_id: turn_id.into(),
        progress: PromptProgress {
            total: 1000,
            cache: 900,
            processed: 950,
            time_ms: 40,
        },
    }
}

pub fn tokens_generated(turn_id: &str, tokens: u64) -> SessionEvent {
    SessionEvent::TokensGenerated {
        turn_id: turn_id.into(),
        tokens,
    }
}

pub fn delta(turn_id: &str, text: &str) -> SessionEvent {
    SessionEvent::Delta {
        turn_id: turn_id.into(),
        target: DeltaTarget::Text,
        text: text.into(),
    }
}

pub fn reasoning(turn_id: &str, text: &str) -> SessionEvent {
    SessionEvent::Delta {
        turn_id: turn_id.into(),
        target: DeltaTarget::Reasoning,
        text: text.into(),
    }
}

pub fn proposed(turn_id: &str, call_id: &str, name: &str) -> SessionEvent {
    proposed_on(turn_id, call_id, name, "")
}

/// A proposal with a §4.1 display target: what the call is *about*.
pub fn proposed_on(turn_id: &str, call_id: &str, name: &str, target: &str) -> SessionEvent {
    SessionEvent::ToolCallProposed {
        turn_id: turn_id.into(),
        call_id: call_id.into(),
        name: name.into(),
        args_digest: "fnv1a:0000000000000000".into(),
        target: target.into(),
    }
}

pub fn requested(req_id: &str, summary: &str) -> SessionEvent {
    SessionEvent::DecisionRequested {
        write_targets: Vec::new(),
        req_id: req_id.into(),
        // §11.6's `kind`: which of the two vocabularies settles it. Not the access
        // class — `exec` was here and reads as one, and a permission over `exec` is
        // still answered by option id.
        kind: "permission".into(),
        call_id: Some("c1".into()),
        // The fixture's tool is a shell, so the declaration is `exec` — the one access a
        // card draws the joining clause for. See
        // `SessionEvent::DecisionRequested::access`.
        access: "exec".into(),
        summary: summary.into(),
        target: String::new(),
        detail: String::new(),
        options: vec![
            DecisionOption {
                option_id: "allow".into(),
                label: "Allow once".into(),
                kind: OptionKind::AllowOnce,
            },
            DecisionOption {
                option_id: "deny".into(),
                label: "Deny".into(),
                kind: OptionKind::RejectOnce,
            },
        ],
        choices: Vec::new(),
        because: String::new(),
        advice: None,
        // The fixture is this session's own call: a card a subagent's gate posted
        // here would carry `Some`. See `SubagentAsk`.
        subagent: None,
        deadline: None,
        on_timeout: OnTimeout::Deny,
    }
}

/// A **question**, not a permission: model-provided choices in plain text, no
/// adjudication ladder, and the one line saying what the model is stuck on (T25/D10).
pub fn asked(req_id: &str, text: &str, choices: &[&str], because: &str) -> SessionEvent {
    SessionEvent::DecisionRequested {
        write_targets: Vec::new(),
        req_id: req_id.into(),
        kind: "question".into(),
        call_id: Some("c1".into()),
        // A question is not a gate, and nothing about a declaration asks for it.
        access: String::new(),
        summary: text.into(),
        target: String::new(),
        detail: String::new(),
        options: Vec::new(),
        choices: choices.iter().map(|c| (*c).into()).collect(),
        because: because.into(),
        advice: None,
        subagent: None,
        deadline: None,
        on_timeout: OnTimeout::Deny,
    }
}

pub fn answered(req_id: &str, option_id: &str) -> SessionEvent {
    SessionEvent::DecisionAnswered {
        req_id: req_id.into(),
        outcome: DecisionOutcome::Selected {
            option_id: option_id.into(),
        },
        by: Decider {
            kind: "human".into(),
            identity: "dead@lab2x1".into(),
        },
        basis: "typed d".into(),
        late: false,
    }
}

pub fn tool_progress(call_id: &str, note: &str) -> SessionEvent {
    SessionEvent::ToolProgress {
        turn_id: "t1".into(),
        call_id: call_id.into(),
        note: note.into(),
    }
}

pub fn turn_finished(turn_id: &str) -> SessionEvent {
    SessionEvent::TurnFinished {
        turn_id: turn_id.into(),
        finish_reason: FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 1000,
            cached_tokens: 900,
            predicted_tokens: 42,
            cost_micros_usd: None,
        },
        timings: Timings {
            prompt_ms: 40.0,
            predicted_ms: 900.0,
            wall_ms: 950,
        },
    }
}

pub fn appended(item_id: &str, kind: &str) -> SessionEvent {
    SessionEvent::TranscriptAppended {
        item_id: item_id.into(),
        kind: kind.into(),
        ledger_head: "beef".into(),
    }
}

/// The body for a row `appended` announced.
pub fn content(item_id: &str, text: &str) -> SessionEvent {
    SessionEvent::TranscriptContent {
        item_id: item_id.into(),
        item: Box::new(letibot_transcript::TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text { text: text.into() }],
        }),
    }
}

/// The daemon saying that a fork replaced the transcript — the event a carry is preceded by.
pub fn forked(transcript_id: &str, parent_id: &str) -> SessionEvent {
    SessionEvent::TranscriptForked {
        transcript_id: transcript_id.into(),
        parent_id: parent_id.into(),
    }
}

/// One of every variant, so an exhaustiveness assertion has something to walk.
pub fn one_of_each() -> Vec<SessionEvent> {
    vec![
        turn_started("t1"),
        progress("t1"),
        tokens_generated("t1", 42),
        delta("t1", "x"),
        proposed("t1", "c1", "read"),
        requested("r1", "do it"),
        answered("r1", "allow"),
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            access: "read".into(),
        },
        tool_progress("c1", "half"),
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 12,
            full_bytes: 12,
            spill: None,
            repairs: 0,
            edit: None,
        },
        turn_finished("t1"),
        SessionEvent::TurnInterrupted {
            turn_id: "t1".into(),
            reason: "operator".into(),
            partial_kept: true,
        },
        appended("s.0", "user"),
        content("s.0", "hi"),
        forked("s#t1", "s#t0"),
        SessionEvent::HeadAttached {
            head_id: "h1".into(),
            kind: "tui".into(),
            identity: "dead".into(),
        },
        SessionEvent::HeadDetached {
            head_id: "h1".into(),
            kind: "tui".into(),
            identity: "dead".into(),
        },
        warn("careful"),
        SessionEvent::Explain {
            turn_id: "t1".into(),
            plan: serde_json::json!({"stage": "render"}),
        },
        SessionEvent::CommandIssued {
            head_id: "h1".into(),
            identity: "dead".into(),
            command: "prompt".into(),
            client_request_id: "c1".into(),
            note: "queued".into(),
        },
        SessionEvent::SessionRenamed {
            title: "the name".into(),
        },
        SessionEvent::TodosUpdated {
            todos: vec![
                TodoEntry {
                    content: "read the harness".into(),
                    status: TodoStatus::Completed,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                TodoEntry {
                    content: "render the pane".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        },
        SessionEvent::DenialRaised {
            request_id: "adj-s1-0001".into(),
            turn_id: "t1".into(),
            call_id: "c1".into(),
            tool: "bash".into(),
            summary: "bash(command: systemctl --user restart llama)".into(),
            baseline: "1 command; intents: privilege_escalation(system)".into(),
            by: "human:dead".into(),
            basis: "always-ask: privilege escalation".into(),
            tier: "always_ask".into(),
            outcome: "denied".into(),
            repeat_count: 1,
            breaker_open: false,
            grant: "grant `adj-s1-0001` (bash) for this session".into(),
        },
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: "summarize ~/bin/letibot".into(),
            model: String::new(),
            answer: None,
            slug: "summarize-bin-letibot".into(),
        },
        SessionEvent::JobSettled {
            job: "j7".into(),
            state: "exited 0".into(),
            produced: 41_392,
            elapsed_ms: 74_206,
        },
        SessionEvent::JobOutput {
            job: "j7".into(),
            from: 0,
            to: 24,
            produced: 24,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: vec!["building".into(), "done".into()],
            next: None,
        },
    ]
}

/// A realistic session: a user turn, reasoning, a tool call gated by a decision,
/// and an answer with markdown in it.
///
/// The text is deliberately long enough and blocky enough to exercise §13.3's
/// incremental lexer — several paragraphs, a fenced code block, a list — and it is
/// emitted as small deltas, which is how it arrives from the engine.
pub fn recorded_session() -> Vec<SessionEvent> {
    let mut out = vec![
        appended("s.0", "system"),
        appended("s.1", "user"),
        turn_started("s#1"),
        progress("s#1"),
    ];
    for w in chunks("The user is asking about the prefix cache. I should check the ledger first.") {
        out.push(reasoning("s#1", &w));
    }
    // With its §4.1 display target, because `--demo` is the fixture people look at
    // to decide whether the head is any good, and a demo that shows `Read (c1)`
    // demonstrates the thing that was fixed as though it had not been.
    out.push(proposed_on(
        "s#1",
        "c1",
        "read",
        "/home/dead/Projects/letibot/TODO.md",
    ));
    out.push(requested("d1", "read /home/dead/Projects/letibot/TODO.md"));
    out.push(answered("d1", "allow"));
    out.push(SessionEvent::ToolStarted {
        turn_id: "s#1".into(),
        call_id: "c1".into(),
        name: "read".into(),
        access: "read".into(),
    });
    out.push(tool_progress("c1", "40 of 276 lines"));
    // The assistant row that MADE the call, appended before the row that answers
    // it — which is what the daemon does (`harnessd::harness` appends the
    // assistant item, invokes the round, then appends its results) and what the
    // store holds for every real session.
    //
    // It was missing, and it is the row a head reads a settled call's display
    // target out of: the arguments live on it and nowhere else once the proposal
    // event has gone by. So `--demo` — the fixture people look at to decide
    // whether the head is any good — rendered `▸ Read (c1)`, demonstrating the
    // thing §4.1 fixed as though it had not been. A fixture missing a row the real
    // producer always emits is a fixture that exercises a head nobody runs.
    out.push(appended("s.2", "assistant"));
    out.push(SessionEvent::ToolFinished {
        turn_id: "s#1".into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Ok,
        payload_digest: "fnv1a:deadbeefdeadbeef".into(),
        inline_bytes: 8_412,
        full_bytes: 8_412,
        spill: None,
        repairs: 0,
        edit: None,
    });
    out.push(appended("s.3", "tool_result"));
    for w in chunks(MARKDOWN) {
        out.push(delta("s#1", &w));
    }
    out.push(appended("s.4", "assistant"));
    out.push(turn_finished("s#1"));
    out
}

/// The transcript rows `recorded_session` announces, so a head fixture can be
/// reconciled the way the daemon reconciles a live one.
pub fn recorded_items() -> Vec<(String, TranscriptItem)> {
    vec![
        (
            "s.0".into(),
            TranscriptItem::System {
                text: "You are letibot.".into(),
                origin: letibot_transcript::SystemOrigin::Bootstrap,
            },
        ),
        (
            "s.1".into(),
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "Why is the prefix cache missing?".into(),
                }],
            },
        ),
        (
            // The row that made the call. Its `arguments` are the only surviving
            // copy of what the call was about once the proposal event has gone by,
            // and they are what `letibot_sessionlog::display_target` reads.
            "s.2".into(),
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"/home/dead/Projects/letibot/TODO.md"}"#.into(),
                }],
                truncated: false,
            },
        ),
        (
            "s.3".into(),
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "…276 lines…".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ),
        (
            "s.4".into(),
            TranscriptItem::Assistant {
                text: MARKDOWN.into(),
                tool_calls: vec![],
                truncated: false,
            },
        ),
    ]
}

/// Split into small pieces the way a token stream arrives: 1–6 characters, never
/// aligned to a block boundary, which is what makes the incremental lexer's guards
/// matter.
fn chunks(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut want = 3usize;
    for c in s.chars() {
        cur.push(c);
        if cur.chars().count() >= want {
            out.push(std::mem::take(&mut cur));
            want = 1 + (out.len() * 7) % 6;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub const MARKDOWN: &str = "\
## Why the cache missed

The short answer is `reasoning_content`. Three things had to line up:

1. The dialect replays prior reasoning into the field the model expects.
2. The ledger appends **ids**, never re-derived text.
3. The stable prefix is frozen before the first turn.

Here is the shape of the check:

```rust
assert_eq!(
    cached_tokens(n + 1),
    prompt_tokens(n) + committed_tokens(n),
);
```

> Note that `predicted_tokens` is the wrong term here — a trailing stop token is
> stripped before commit, so the witness must record **committed** tokens.

That is the whole of it. The remaining divergence is the server's own checkpoint
behaviour on a hybrid model, which is a measurement of llama.cpp and not a
violation of the invariant.
";

/// **Where a test's scratch directory goes**: the system temp dir — reached through a short path
/// when the temp dir's own is long, and on macOS `/tmp`.
///
/// A Unix socket's path has to fit `sockaddr_un.sun_path`: 104 bytes on macOS, 108 on Linux. On
/// macOS `std::env::temp_dir()` is the per-user `/var/folders/…/T/`, about fifty of those bytes
/// before a test has named anything, so a scratch dir with a pid and a timestamp in it and a
/// `harnessd.sock` inside runs past the limit and `bind` refuses it — a test that passes on Linux
/// and fails on a Mac for a reason that has nothing to do with what it tests. The same length
/// pushes a path drawn in a pane past the pane's width. `/tmp` is short on both counts.
///
/// **Linux has the same failure under a long `TMPDIR`.** MEASURED 2026-10-14: with `TMPDIR` a
/// 41-byte directory in `$HOME`, eight tests across `restart_seats`, `sudo_ask_live` and
/// `stop_force` failed `path must be shorter than SUN_LEN`, and with `TMPDIR` inside a worktree
/// another run counted about sixty. Which tests fail depends only on how long each one's tag is,
/// so a per-test fix is a list that is never finished. So when the temp dir's path is longer
/// than [`SHORT_ROOT`] bytes, this returns `/tmp/lbt-<pid>`, a symlink to it: every scratch path
/// is short, and every file still lands where `TMPDIR` put it — a `TMPDIR` on disk is usually
/// there to keep a test run's files out of a RAM-backed `/tmp`, and moving them back would undo
/// the reason it was set. One link per process; links whose process is gone are removed when
/// the next one is made (symlinks of exactly that name only). Under the default `/tmp` nothing
/// changes.
pub fn scratch_root() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from("/tmp")
    }
    #[cfg(not(target_os = "macos"))]
    {
        static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| short_alias(std::env::temp_dir()))
            .clone()
    }
}

/// The longest temp dir [`scratch_root`] uses as it is. `/tmp` and `/var/tmp` pass; a directory
/// in `$HOME` does not. Sixteen leaves ninety bytes for a test's own names and its socket.
pub const SHORT_ROOT: usize = 16;

/// `temp` itself when it is short, else `/tmp/lbt-<pid>` linked to it — or `temp` again if the
/// link cannot be made, which leaves the old behaviour rather than a new failure.
#[cfg(not(target_os = "macos"))]
fn short_alias(temp: std::path::PathBuf) -> std::path::PathBuf {
    short_alias_at(temp, &format!("/tmp/lbt-{}", std::process::id()))
}

/// [`short_alias`] with the link's name handed in, so its test cannot replace the link the
/// process's own [`scratch_root`] is using.
#[cfg(not(target_os = "macos"))]
fn short_alias_at(temp: std::path::PathBuf, link: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    if temp.as_os_str().as_bytes().len() <= SHORT_ROOT {
        return temp;
    }
    sweep_dead_links("lbt-");
    let link = std::path::PathBuf::from(link);
    let _ = std::fs::create_dir_all(&temp);
    if std::fs::read_link(&link).is_ok_and(|to| to == temp) {
        return link;
    }
    let _ = std::fs::remove_file(&link);
    match std::os::unix::fs::symlink(&temp, &link) {
        Ok(()) => link,
        Err(_) => temp,
    }
}

/// **Where a test binds `dir`'s socket**: `dir/<name>` when that fits `sun_path`, otherwise the
/// same file reached through a short symlink in `/tmp`.
///
/// [`scratch_root`] is the system temp dir, and a test's scratch dir name carries a tag, a pid
/// and a nanosecond clock. Under the default `/tmp` that leaves room for `harnessd.sock`; under
/// `TMPDIR=$HOME/.cache/letibot-test-tmp-…` it does not. MEASURED 2026-10-14 with a 41-byte
/// `TMPDIR`: `restart_seats`, `sudo_ask_live` and `tui`'s `stop_force` all failed binding
/// (`path must be shorter than SUN_LEN`), for a reason that has nothing to do with what they
/// test. The limit is the KERNEL's — 108 bytes with the NUL on Linux, 104 on macOS — so the
/// product refusing such a path is correct, and the fix belongs here.
///
/// The symlink keeps the scratch dir where `TMPDIR` put it: `/tmp/letibot-sk-<pid>-<n>` points
/// at `dir`, the socket is bound through it, and the socket file itself lands in `dir` and goes
/// when `dir` does. Only the dangling link is left, a few bytes; each call removes the links of
/// processes that no longer exist (Linux, by `/proc`; elsewhere they stay rather than guess).
/// [`scratch_root`] already keeps the common case short; this covers a dir made elsewhere and a
/// socket name longer than `harnessd.sock`.
pub fn socket_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    // sun_path minus its NUL: 104 on macOS, 108 on Linux.
    const ROOM: usize = if cfg!(target_os = "macos") { 103 } else { 107 };
    let direct = dir.join(name);
    if direct.as_os_str().as_bytes().len() <= ROOM {
        return direct;
    }
    sweep_dead_links("letibot-sk-");
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let link = std::path::PathBuf::from("/tmp").join(format!(
        "letibot-sk-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(dir, &link)
        .unwrap_or_else(|e| panic!("linking {} at {}: {e}", dir.display(), link.display()));
    link.join(name)
}

/// Remove the `/tmp/<prefix><pid>[-<n>]` SYMLINKS whose process is gone — never a file or a
/// directory, never a live pid's. Linux only, because `/proc` is the probe.
fn sweep_dead_links(prefix: &str) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(prefix)) else {
            continue;
        };
        let (pid, n) = rest.split_once('-').unwrap_or((rest, "0"));
        if !digits(pid) || !digits(n) {
            continue;
        }
        let is_link = std::fs::symlink_metadata(entry.path())
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_link && !std::path::Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod socket_path_tests {
    use super::socket_path;

    /// A short dir is used as it is; a long one is reached through a link that binds.
    #[test]
    fn a_socket_path_always_fits_and_a_long_one_still_lands_in_its_dir() {
        let short = std::path::Path::new("/tmp/x");
        assert_eq!(socket_path(short, "h.sock"), short.join("h.sock"));

        let long = std::env::temp_dir().join(format!(
            "letibot-socket-path-test-{}-{}",
            std::process::id(),
            "x".repeat(90)
        ));
        std::fs::create_dir_all(&long).unwrap();
        let p = socket_path(&long, "harnessd.sock");
        assert!(p.as_os_str().len() <= 107, "{}", p.display());
        let listener = std::os::unix::net::UnixListener::bind(&p).expect("it binds");
        assert!(
            long.join("harnessd.sock").exists(),
            "the socket is in the dir"
        );
        drop(listener);
        let _ = std::fs::remove_file(p.parent().unwrap());
        let _ = std::fs::remove_dir_all(&long);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod short_alias_tests {
    use super::short_alias_at;

    /// A short temp dir is itself; a long one is reached through a short link to the same dir.
    #[test]
    fn a_long_temp_dir_is_reached_through_a_short_link_to_itself() {
        let name = format!("/tmp/lbt-{}-1", std::process::id());
        assert_eq!(
            short_alias_at("/tmp".into(), &name),
            std::path::PathBuf::from("/tmp")
        );
        let long = std::env::temp_dir().join(format!(
            "letibot-short-alias-test-{}-{}",
            std::process::id(),
            "y".repeat(40)
        ));
        let got = short_alias_at(long.clone(), &name);
        assert_eq!(got, std::path::PathBuf::from(&name));
        assert_eq!(std::fs::read_link(&got).unwrap(), long);
        std::fs::write(got.join("f"), "x").unwrap();
        assert!(long.join("f").is_file(), "the files land in the long dir");
        let _ = std::fs::remove_file(&got);
        let _ = std::fs::remove_dir_all(&long);
    }
}
