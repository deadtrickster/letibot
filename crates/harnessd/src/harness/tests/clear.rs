//! `/clear`'s orientation: the one paragraph the model wakes up holding, so every sentence in it
//! has to be something the model can act on.

use super::*;

/// A harness over a fresh scratch directory, or `None` when this box has no vocabulary GGUF.
fn a_harness(id: &str) -> Option<Harness> {
    let gguf = crate::vocab_gguf()?;
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some(Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens"))
}

/// The old conversation is named by what the model can DO about it, not by its id: `transcript`
/// takes predicates and no transcript id, so an id in this sentence is an instruction that cannot
/// be obeyed. `R93.E10` took the session's own id and the job counts out for the same reason.
#[test]
fn the_orientation_reaches_the_old_conversation_without_naming_its_id() {
    let Some(h) = a_harness("s-clear-orientation") else {
        return;
    };
    let said = h.orientation(None);
    assert!(
        !said.contains("#t"),
        "the orientation hands the model a transcript id: {said}"
    );
    assert!(
        !said.contains("s-clear-orientation"),
        "and not the session's own id either: {said}"
    );
    assert!(
        said.contains("assume nothing") && said.contains("not even a summary"),
        "a clear must still say the old conversation was neither carried nor summarised: {said}"
    );
    assert!(
        said.contains("still in the store") && said.contains("`transcript`"),
        "and where the model can reach it: {said}"
    );
    assert!(
        said.contains("history"),
        "the default that makes `transcript` reach it: {said}"
    );
    assert!(
        !said.contains("Jobs (") && !said.contains("Subagents ("),
        "a count of what is listed is a second statement to keep true: {said}"
    );
}
