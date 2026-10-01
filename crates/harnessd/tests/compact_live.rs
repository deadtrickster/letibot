//! Compaction against the live server: the numbers C1's done-when asked for.
//!
//! The offline files prove the machinery. This one measures the claim, on the box,
//! with the model the box actually serves:
//!
//! 1. **The summary turn is carried, not re-prefilled.** `cached_tokens` against
//!    `reusable` on the summary turn is the measured cache hit — the whole point
//!    of the cached strategy, and the number opencode's flatten compaction
//!    famously zeroed (`1ee74df`/`4efae6c`: cache 0, ~13 minutes at 182 t/s).
//! 2. **The summary carries the facts.** Numbers given before the compaction are
//!    asked for after it; a summary that lost them would be a base that looks
//!    small and answers wrong, which is the failure mode no token count catches.
//! 3. **The first turn on the fork pays the cold prefill** `docs/compaction.md`
//!    §3 says is unavoidable — disclosed as a number, not denied.
//!
//! # The one rule on this box
//!
//! The server on `127.0.0.1:8080` is a singleton and the GLM one is already
//! serving — this session runs on it. **Never start a second model server to
//! make a test pass**: the services evict each other, and the eviction takes down
//! whatever else is running on the same endpoint. This test uses the GLM dialect
//! and the GLM vocabulary, which is what the server is already holding, so it
//! starts nothing and evicts nothing.
//!
//! The serving preflight (`letibot-turn`'s `serving.rs`) is the gate: against any
//! other model it refuses by name before a token is sent.

use std::sync::OnceLock;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;

const GLM_GGUF: &str =
    "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Glm;
    cfg.model = "glm-5.3-flash".into();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| GLM_GGUF.into());
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg
}

/// **Is the box actually serving the model this test renders for?**
///
/// The docstring above states this as a property — *"the serving preflight is the gate: against any
/// other model it refuses by name before a token is sent"* — and it was **not true of this file**.
/// The guard existed in three sibling live tests and not here, so this one asserted that the GLM
/// *vocabulary file* exists and then went looking for a GLM *server*: it rendered GLM token ids,
/// sent them to whatever was on 8080, and spent **51 minutes** (measured: 3057.70s) discovering
/// that the ids mean something else under another vocabulary. The failure it produced —
/// `262194 tokens exceeds the available context size 262144` — names the qwen server's window, not
/// anything about compaction, which is why it read as a parked mystery.
///
/// So the docstring's claim is now enforced rather than asserted. `live_qwen.rs` has the same
/// function for its own model, and this is that shape with GLM's name in it: refuse by name, in
/// seconds, and say `THIS IS NOT A PASS` so a skip is never read as a green.
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
    // **THE APPARATUS FIRST, and this ordering is a fix rather than a style.** The
    // vocabulary assert used to come first, so MEASURED on a runner — no server and
    // no GGUF — the missing FILE fired before the missing SERVER could be noticed,
    // and absent apparatus was reported as a failed assertion. Nothing here is a
    // defect on that box; the assert just ran before the guard that explains it.
    //
    // A test's guards go in the order the reasons do: what the machine must HAVE,
    // then what must be RUNNING. `todos_live.rs` already had it this way.
    //
    // And it checks the file the CONFIG will actually use — `LETIBOT_VOCAB_GGUF`, else
    // `GLM_GGUF` — rather than the constant alone, because a guard that resolves a
    // different path from the thing it guards is a guard that can be true while the
    // test still cannot run.
    let gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| GLM_GGUF.into());
    let Some(_) = letibot_tokencore::apparatus::present(
        &format!("a GLM vocabulary GGUF ({})", gguf.display()),
        gguf.is_file(),
    ) else {
        return;
    };
    // **The precondition the file's docstring already stated**, checked instead of assumed: a
    // vocabulary on disk is not a model on the endpoint, and only the second one can answer this.
    if !glm_is_served() {
        return;
    }
    let dir = TempDir::new("harnessd-compact-live");
    let path = dir.path().join("sessions.db");
    let cfg = config(&path, "compact-live");
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub).expect("the session must open");

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
    // The structural claim: there WAS a previous request to carry, and the server
    // said it carried it. `reusable` is ours (the prefix invariant proves the
    // prompts identical over the span); `cached_tokens` is the server's word, and
    // a zero here is the flatten failure this design exists to prevent.
    assert!(sum.reusable > 0, "nothing to carry after two turns");
    assert!(
        sum.cached_tokens > 0,
        "the server reused nothing (cached 0 of {} carryable) — this is the flatten \
         failure, and it must be investigated, not absorbed into a pass",
        sum.reusable
    );

    // The facts survive the reduction. This is the check no token count catches:
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
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
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
