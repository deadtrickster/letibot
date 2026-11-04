//! A one-shot run refuses `background: true` and says what to do instead: its daemon exits with the
//! run, so a left-behind job's result would never be read. Decided from the request, before the
//! backend, so a fixture that starts no processes can reach it.

use letibot_tools::testing::{Harness, text_only_runner_harness};
use letibot_transcript::ToolOutcome;

/// The fixture with the one-shot fact set the way `harnessd` sets it: on the runtime, from the
/// run's own config.
fn one_shot_harness() -> Harness {
    let mut h = text_only_runner_harness();
    h.rt.one_shot = true;
    h
}

/// The refusal, in the model's words, with nothing started.
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

/// And the rule is about the background, not about running commands: a test that only asserted
/// the refusal would pass just as well if `bash` refused everything in a one-shot.
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

/// The default is the other way round: a rule that is always on is not a rule about one-shots.
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
