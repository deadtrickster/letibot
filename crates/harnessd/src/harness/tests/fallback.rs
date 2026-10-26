//! **The turn a fallback owes** — armed where the refusal is, taken once by the tail, and never
//! started over a stop the operator pressed.
//!
//! `model_switch_at_turn_end.rs` measures the whole of it end to end (a refused model, the switch,
//! the continuation turn, and the notice that opens it). What belongs here is the part a daemon
//! cannot make deterministic: an interrupt that arrives in the microseconds between the refusal
//! and the tail's take, which is exactly the window the guard exists for.

use super::*;

/// A harness over a fresh store in its own scratch directory, or `None` when this box has no
/// vocabulary GGUF.
fn a_harness(id: &str) -> Option<(Harness, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    Some((h, dir))
}

/// **One fallback, one continuation — and none over the operator's stop.**
///
/// The operator's ruling of 2026-10-12 (*"when it finally applied the turn didnt continue"*) is
/// what arms the notice; the two things measured here are its BOUNDS, because both failure modes
/// cost somebody real money: a notice that could be taken twice would open a turn nobody asked
/// for, and a continuation started over their interrupt would either be aborted by it or ignore
/// their stop.
#[test]
fn a_fallback_continuation_is_taken_once_and_never_over_a_stop() {
    let Some((mut h, dir)) = a_harness("s-fallback-continuation") else {
        return;
    };
    let notice = "[fallback] glm-coding/glm-5.3 refused (http 429); continuing this task on \
                  glm/glm-4.6."
        .to_string();

    // **A turn that fell back owes exactly one turn, and the notice is spent when it is taken.**
    h.fallback_continuation = Some(notice.clone());
    assert_eq!(
        h.take_fallback_continuation().as_deref(),
        Some(notice.as_str()),
        "the continuation the fallback earned was not taken"
    );
    assert!(
        h.take_fallback_continuation().is_none(),
        "the same fallback opened a second turn"
    );

    // **And nothing is started over a stop.** The interrupt arrived after the failed turn's last
    // round, so no round of it would act on it — but starting a turn on top of it is either a
    // turn aborted at its first delta or, worse, their stop ignored.
    h.fallback_continuation = Some(notice.clone());
    h.hub.submit(
        letibot_sessionlog::hub::DAEMON_SUBMITTER,
        "stop-1",
        0,
        letibot_sessionlog::hub::CommandKind::Interrupt {
            reason: "the operator stopped this turn".into(),
        },
    );
    assert!(
        h.hub.has_queued_interrupt(),
        "the stop was not queued, so this measured nothing"
    );
    assert!(
        h.take_fallback_continuation().is_none(),
        "a continuation was started over the operator's interrupt"
    );
    assert!(
        h.take_fallback_continuation().is_none(),
        "and the notice was left armed, so some LATER turn would have opened one"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
