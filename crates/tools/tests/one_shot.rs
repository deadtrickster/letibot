//! **A one-shot run refuses `background: true`, and says what to do instead.**
//!
//! `letibot "a question"` starts a daemon of its own on `oneshot-<pid>.sock`, runs the prompt,
//! prints the answer and exits — and the daemon exits with it. The operator, 2026-10-26: several
//! of those daemons were still up after their one-shot run had ended, "some holding background
//! jobs the model had started, and nothing will ever read those jobs' results".
//!
//! # The two answers, and which one this is
//!
//! A one-shot could WAIT: keep the daemon up while a job runs, deliver its completion as another
//! turn, bounded, then exit. That is a second lifetime rule for the one mode that has none, and
//! the bound would be a number nobody can justify — the job a one-shot starts is exactly the long
//! one nobody wants to sit in front of. So it REFUSES, by name, and tells the model the thing
//! that does work here: the foreground with a bigger `timeout_ms`.
//!
//! A refusal has to be reachable on every box to be worth having, so the rule is decided from
//! the REQUEST — before the backend, the scope and the predicate — which is why this test can
//! run the tools over a backend that starts no processes at all (`text_only_runner_harness`) and
//! still reach it. The control case proves the rule did not fire: the same call in the same
//! one-shot runtime, without `background`, gets as far as the backend's own refusal.

use letibot_tools::testing::{Harness, text_only_runner_harness};
use letibot_transcript::ToolOutcome;

/// The fixture with the one-shot fact set, the way `harnessd` sets it: on the RUNTIME, from the
/// run's own config (`Config::one_shot`, wired in `Harness::open_with_registry`).
fn one_shot_harness() -> Harness {
    let mut h = text_only_runner_harness();
    h.rt.one_shot = true;
    h
}

/// **The refusal, in the model's words, with nothing started.**
#[test]
fn a_one_shot_run_refuses_a_background_job_by_name() {
    let mut h = one_shot_harness();
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 5", "background": true}).to_string(),
    );
    let ToolOutcome::NotRun { why } = &r.outcome else {
        panic!(
            "a one-shot run started (or tried to start) a background job: {:?}\n{}",
            r.outcome,
            r.render()
        );
    };
    assert!(
        why.contains("one-shot") && why.contains("background"),
        "the refusal does not name the two facts that make it: {why}"
    );
    assert!(
        r.payload.contains("timeout_ms") && r.payload.contains("FOREGROUND"),
        "the refusal does not say what DOES work in a one-shot: {}",
        r.payload
    );
    assert!(
        !r.payload.contains("`j"),
        "a refusal handed the model a job handle: {}",
        r.payload
    );
}

/// **And the rule is about the background, not about running commands.**
///
/// A foreground call is not refused for being one-shot: it reaches the backend, which cannot
/// start processes in this fixture and says so. A test that only asserted the refusal would pass
/// just as well if `bash` refused everything in a one-shot.
#[test]
fn a_foreground_command_in_a_one_shot_is_not_refused_for_being_one() {
    let mut h = one_shot_harness();
    let r = h.call("bash", &serde_json::json!({"command": "true"}).to_string());
    let rendered = r.render();
    assert!(
        !rendered.contains("one-shot"),
        "a foreground command was refused by the one-shot rule: {rendered}"
    );
    assert!(
        rendered.contains("cannot start processes"),
        "the foreground call did not reach the backend, so this case proves nothing: {rendered}"
    );
}

/// The default is the other way round: a runtime nobody armed treats `background: true` as it
/// always did. A rule that is always on is not a rule about one-shots.
#[test]
fn an_ordinary_runtimes_background_call_does_not_meet_the_one_shot_rule() {
    let mut h = text_only_runner_harness();
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 5", "background": true}).to_string(),
    );
    assert!(
        !r.render().contains("one-shot"),
        "an ordinary session refused a background job for a one-shot's reason: {}",
        r.render()
    );
}
