//! Compaction against the live server: the numbers the claim rests on, measured on the box
//! with the model it actually serves. 1. The summary turn is carried, not re-prefilled.

use std::sync::OnceLock;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Glm;
    cfg.model = "glm-5.3-flash".into();
    cfg.vocab_gguf = Some(
        std::env::var("LETIBOT_VOCAB_GGUF")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| letibot_tokencore::apparatus::glm_gguf_path()),
    );
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg
}

/// Is the box actually serving the model this test renders for? A vocabulary on disk is not
/// a model on the endpoint
fn glm_is_served() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let want = std::env::var("LETIBOT_MODEL_ALIAS").unwrap_or_else(|_| "glm-5.3-flash".into());
        let endpoint = config(&std::path::PathBuf::from("/tmp"), "preflight").endpoint;
        match letibot_turn::serving::served_model(&endpoint) {
            Ok(served) if letibot_turn::serving::matches(&served, &want) => true,
            Ok(served) => {
                eprintln!(
                    "SKIPPED: {} is serving `{served}`, and this test renders the GLM dialect for \
                     `{want}` — the control tokens would not resolve, so nothing was run and THIS \
                     IS NOT A PASS. Start {want}, or set LETIBOT_MODEL_ALIAS.",
                    endpoint.authority()
                );
                false
            }
            Err(e) => {
                eprintln!(
                    "SKIPPED: could not ask {}/props ({e}), so nothing was run and THIS IS NOT A \
                     PASS.",
                    endpoint.authority()
                );
                false
            }
        }
    })
}

#[test]
fn a_live_compaction_carries_the_prefix_and_the_summary_carries_the_facts() {
    // THE APPARATUS FIRST, and this ordering is a fix rather than a style. A test's guards go
    // in the order the reasons do: what the machine must HAVE, then what must be RUNNING - a
    // vocabulary assert that fires first reports absent apparatus as a failed assertion.
    let gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| letibot_tokencore::apparatus::glm_gguf_path());
    let Some(_) = letibot_tokencore::apparatus::present(
        &format!("a GLM vocabulary GGUF ({})", gguf.display()),
        gguf.is_file(),
    ) else {
        return;
    };
    // The precondition the file's docstring already stated, checked instead of assumed: a
    // vocabulary on disk is not a model on the endpoint, and only the second one can answer this.
    if !glm_is_served() {
        return;
    }
    let dir = TempDir::new("harnessd-compact-live");
    let path = dir.path().join("sessions.db");
    let cfg = config(&path, "compact-live");
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session must open");

    // Two real turns, so the summary has facts to record and the summary turn has
    // a previous request to carry. Deterministic sampling from the config: the
    // same prompts measure the same thing twice.
    let r1 = h
        .submit("Remember these three numbers for later: 41, 427, 9001. Reply with just: ack")
        .expect("turn 1");
    let r2 = h
        .submit("Without any tool, what is 41 + 427? Reply with just the number.")
        .expect("turn 2");
    eprintln!("  turn 1 -> {:?}", r1.text.trim());
    eprintln!("  turn 2 -> {:?}", r2.text.trim());
    assert!(
        r2.text.contains("468"),
        "arithmetic came back as {:?}",
        r2.text
    );

    let before = h.ledger_len();
    let report = h.compact().expect("the compaction");
    // EVERY ROW THE SUMMARY TURN ANNOUNCED CARRIES ITS BODY. The instruction and the answer
    // are appended to this session through a capturing sink, which announces them
    let snap = hub.snapshot();
    let unfilled: Vec<&str> = snap
        .items
        .iter()
        .filter(|r| r.item.is_none())
        .map(|r| r.item_id.as_str())
        .collect();
    assert!(
        unfilled.is_empty(),
        "{} row(s) announced and never filled (the compaction's own instruction and \
         answer are the usual pair): {unfilled:?}",
        unfilled.len()
    );
    let sum = &report.summary_turn;
    eprintln!(
        "  compacted: base {before} -> {} tokens (transcript {})",
        report.fork.base_tokens, report.fork.transcript_id
    );
    eprintln!(
        "  summary turn: cached {} of {} carryable ({} generated, {} tok summary)",
        sum.cached_tokens,
        sum.reusable,
        sum.generated_tokens,
        sum.summary.len()
    );
    // The structural claim: there WAS a previous request to carry, and the server said it
    // carried it. `reusable` is ours (the prefix invariant proves the prompts identical over
    // the span)
    assert!(sum.reusable > 0, "nothing to carry after two turns");
    assert!(
        sum.cached_tokens > 0,
        "the server reused nothing (cached 0 of {} carryable) — this is the flatten \
         failure, and it must be investigated, not absorbed into a pass",
        sum.reusable
    );

    // The facts survive the reduction. This is the check no token count catches
    // a base that answers from the summary, not from a lost history.
    let r3 = h
        .submit("What three numbers did I ask you to remember? Just the three, in order.")
        .expect("the first turn on the forked base");
    eprintln!("  post-fork -> {:?}", r3.text.trim());
    eprintln!(
        "  post-fork turn: {} prompt tokens, cached {} (the cold base §3 predicted)",
        r3.metrics.last().map(|m| m.predicted_tokens).unwrap_or(0),
        r3.metrics.last().map(|m| m.cached_tokens).unwrap_or(0)
    );
    for n in ["41", "427", "9001"] {
        assert!(
            r3.text.contains(n),
            "the summary lost {n}: the forked base answered {:?}",
            r3.text
        );
    }
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
