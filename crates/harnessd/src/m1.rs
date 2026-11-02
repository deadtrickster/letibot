//! `letibot-m1` as a FUNCTION, so more than one binary can be it.
//!
//! The M1 exit criterion: a fixed 30-turn script replayed against a client, with a judge. Offline
//! apart from the model endpoint, which is why it is checked by its USAGE paths rather than by a
//! run - those need no server and they are what a dispatcher can get wrong.
//!
//! `die` stays. It is called from nine places, several inside a closure that borrows the
//! argument iterator, and every one is a usage error whose contract is exit 2 with a message.
//! Returning `Result` would thread a code through nine sites for no observable difference: the exit
//! code and the wording are the interface, and both are preserved exactly.
//!
//! ```text
//! letibot-m1 [--workspace DIR] [--turns N] [--dialect qwen|glm] [--effort low]
//!            [--endpoint HOST:PORT] [--model ALIAS] [--vocab GGUF] [--json FILE]
//! ```
//!
//! §17's M1 exit is *"C1–C10 pass against harnessd where opencode failed, and `f_keep` p10 ≥ 0.99
//! over a scripted 30-turn session"*. Every C-test it can reach is reported, and the ones it
//! cannot are named with the reason - `NOT RUN`, never folded into a pass count, because a
//! vacuous pass here is exactly the failure this whole project is a reaction to.
//!
//! # What each C-test becomes on the token-array path
//!
//! `tests/fidelity/run_gate.py` gives C2 and is not re-run here; the rest are measured from the
//! ledger and from `turn_metrics`.
//!
//! | # | here |
//! |---|---|
//! | C1 | the submitted token vector at turn N is an exact prefix of turn N+1's. Read off the ledger, not inferred from cache numbers. |
//! | C2 | NOT RUN - it is the fidelity gate, which needs no model. GLM 139/139, Qwen 56/56. |
//! | C3 | the engine's own `PrefixCheck`, which is §18.1-I1: turn N+1's prompt begins with turn N's plus what the model generated. Read off, never recomputed. |
//! | C4 | `f_keep` p10/p50/p99 and p99 re-prefill over the session, with `f_keep` in D11's settled form - `cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))`. Those are C3's own two numbers, so C3 and C4 are read from one `PrefixCheck`. |
//! | C5 | `f_keep` does not collapse on the submission that follows an assistant turn carrying reasoning. |
//! | C6 | every assistant item with tool calls and empty text still owns a non-empty ledger row. |
//! | C7 | NOT RUN live - see below. |
//! | C8 | every turn's `finish_reason` reaches `turn_metrics`; a `length` is never recorded as a completed turn. |
//! | C9 | a mid-session system change leaves the stable prefix byte-identical and appends at the tail, and `f_keep` stays high across it. |
//! | C10 | the server's own `n_prompt_tokens` equals ours, and equals `cached + processed`. |
//!
//! C7 cannot be produced live, deliberately: it needs a `length` finish, which needs either an
//! `n_predict` cap - §5.7 removed it on purpose and no test may add it back - or a 262,144-token
//! context filled on a production box. The policy is exercised exhaustively offline in
//! `letibot_turn::length`; what is measured here is the server half, that `finish_reason` arrives.
//!
//! # `f_keep` here is D11's, and `f_sim` is printed beside it
//!
//! §18.2's C4 originally read `cached_tokens / prompt_tokens`, whose denominator is the *new*
//! prompt - llama's `f_sim`, which falls purely as a function of how much the conversation grew and
//! cannot reach 0.99 on any session that returns tool output (T22). D11 settles C4 as `lcp /
//! cached_entry`. Both are printed under their own names, because the old one is real and only its
//! name was wrong.

use std::path::PathBuf;

use crate::config::Config;
use crate::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::TokenId;
use letibot_turn::{Endpoint, TurnMetrics};

/// One submission's worth of evidence.
struct Row {
    turn: usize,
    kind: &'static str,
    m: TurnMetrics,
    /// Whether the transcript before this submission ended with an assistant turn
    /// that carried reasoning - C5's condition.
    after_reasoning: bool,
}

/// Run the M1 script over `args`, returning the process exit code. `args` is what followed
/// the program name. A usage error exits 2 from `die` rather than returning, which is
/// stated in the module docs above.
pub fn run(args: &[String]) -> i32 {
    let mut cfg = Config::for_this_box(std::env::current_dir().expect("a working directory"));
    // A measurement of a live session on this box, so it reads this box: the instruction
    // file and the skills index are part of what a session costs, and leaving them out would
    // measure a prompt nobody runs.
    cfg.read_the_box();
    cfg.socket = std::env::temp_dir().join(format!("letibot-m1-{}.sock", std::process::id()));
    cfg.effort = Some("low".into());
    let mut turns = 30usize;
    let mut json_out: Option<PathBuf> = None;

    let mut it = args.iter().cloned();
    while let Some(arg) = it.next() {
        let mut next = || {
            it.next()
                .unwrap_or_else(|| die(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--workspace" => cfg.workspace = PathBuf::from(next()),
            "--turns" => {
                turns = next()
                    .parse()
                    .unwrap_or_else(|e| die(&format!("--turns: {e}")))
            }
            "--model" => cfg.model = next(),
            "--vocab" => cfg.vocab_gguf = Some(PathBuf::from(next())),
            "--effort" => cfg.effort = Some(next()),
            "--store" => cfg.store = Some(PathBuf::from(next())),
            "--json" => json_out = Some(PathBuf::from(next())),
            "--dialect" => {
                let n = next();
                cfg.dialect = Dialect::parse(&n).unwrap_or_else(|| die("unknown dialect"));
            }
            "--endpoint" => {
                let v = next();
                let (h, p) = v
                    .rsplit_once(':')
                    .unwrap_or_else(|| die("--endpoint HOST:PORT"));
                cfg.endpoint = Endpoint::new(h, p.parse().unwrap_or_else(|_| die("port")));
            }
            other => die(&format!("unknown argument {other}")),
        }
    }

    // A nonce in the session id keeps the *session* distinct; the stable prefix is
    // deliberately not nonced, because sharing a warm prefix between sessions is
    // the design (§5.2) and noncing it would measure a cold start on purpose.
    let parts = Parts::load(&cfg).unwrap_or_else(|e| die(&e.to_string()));
    let hub = Hub::new(cfg.session_id.clone());
    let workspace = cfg.workspace.clone();
    let model = cfg.model.clone();
    let dialect = cfg.dialect.name();
    let mut harness =
        Harness::open(&parts, cfg, hub.clone()).unwrap_or_else(|e| die(&e.to_string()));

    println!("=== M1 exit test ===");
    println!("model     {model}");
    println!("dialect   {dialect}");
    println!("workspace {}", workspace.display());
    println!("prefix    {} tokens", harness.tokens().len());
    println!("turns     {turns}");
    for d in harness.config().disclosures(harness.wiring()) {
        println!("!         {d}");
    }
    println!();

    let script = script(turns);
    let mut rows: Vec<Row> = Vec::new();
    // C1's evidence: the ledger's token vector after every user turn. Compared as
    // vectors, not as hashes, so a failure can say where it diverged.
    let mut vectors: Vec<Vec<TokenId>> = vec![harness.tokens().to_vec()];
    let mut failures: Vec<String> = Vec::new();
    let prefix_at_open: Vec<TokenId> = harness.prefix_tokens().to_vec();

    for (i, step) in script.iter().enumerate() {
        let after_reasoning = harness
            .items()
            .iter()
            .rev()
            .any(|it| matches!(it, letibot_transcript::TranscriptItem::Reasoning { .. }));
        let started = std::time::Instant::now();
        let reply = match step {
            Step::Ask(text) => harness.submit(text),
            Step::SystemUpdate(text) => harness.system_update(text),
        };
        match reply {
            Ok(r) => {
                for m in r.metrics {
                    rows.push(Row {
                        turn: i + 1,
                        kind: step.kind(),
                        m,
                        after_reasoning,
                    });
                }
                println!(
                    "{:>3}. {:<14} {:>5} ms  {} round(s), {} call(s)  {}",
                    i + 1,
                    step.kind(),
                    started.elapsed().as_millis(),
                    r.rounds,
                    r.tool_calls,
                    first_line(&r.text)
                );
            }
            Err(e) => {
                println!("{:>3}. {:<14} FAILED: {e}", i + 1, step.kind());
                failures.push(format!("turn {}: {e}", i + 1));
                if e.is_fatal() {
                    break;
                }
            }
        }
        vectors.push(harness.tokens().to_vec());
    }

    println!();
    let verdicts = judge(&rows, &vectors, &prefix_at_open, &harness, &failures);
    for v in &verdicts {
        println!("{v}");
    }

    if let Some(path) = json_out {
        let payload = serde_json::json!({
            "model": model,
            "dialect": dialect,
            "turns": script.len(),
            "submissions": rows.len(),
            "rows": rows.iter().map(|r| serde_json::json!({
                "turn": r.turn,
                "kind": r.kind,
                "prompt_tokens": r.m.prompt_tokens,
                "prompt_tokens_server": r.m.prompt_tokens_server,
                "cached_tokens": r.m.cached_tokens,
                "prompt_processed": r.m.prompt_processed,
                "predicted_tokens": r.m.predicted_tokens,
                "f_keep": r.m.f_keep(),
                "f_sim": r.m.f_sim(),
                "reprefill": r.m.reprefill(),
                "finish_reason": r.m.finish_reason.as_str(),
                "id_slot": r.m.id_slot,
                "wall_ms": r.m.wall_ms,
                "prefix_check": format!("{:?}", r.m.prefix_check),
                "expected_cached_min": match &r.m.prefix_check {
                    letibot_turn::PrefixCheck::Held { expected_cached_min, .. } =>
                        Some(*expected_cached_min),
                    _ => None,
                },
                "shortfall": match &r.m.prefix_check {
                    letibot_turn::PrefixCheck::Held { shortfall, .. } => Some(*shortfall),
                    _ => None,
                },
            })).collect::<Vec<_>>(),
            "verdicts": verdicts,
        });
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap());
        println!("\nrows written to {}", path.display());
    }

    let failed = verdicts
        .iter()
        .any(|v| v.starts_with("C") && v.contains(" FAIL"));
    std::process::exit(if failed { 1 } else { 0 });
}

enum Step {
    Ask(String),
    SystemUpdate(String),
}

impl Step {
    fn kind(&self) -> &'static str {
        match self {
            Step::Ask(_) => "ask",
            Step::SystemUpdate(_) => "system-update",
        }
    }
}

/// The scripted session (§18.2: *"a fixed 30-turn conversation with a fixed tool script
/// replayed identically against any client"*).
fn script(turns: usize) -> Vec<Step> {
    let ask = |s: &str| Step::Ask(s.to_string());
    let mut v = vec![
        ask("In one sentence: what is a prefix cache?"),
        ask("What does 'append-only' mean for a conversation transcript? One sentence."),
        ask("Name three things that live in a Cargo workspace manifest. Just the three."),
        ask("List the files at the repository root using the glob tool, then name the manifest."),
        ask("Read Cargo.toml and tell me how many workspace members it lists. Just the number."),
        ask("Grep for 'fn main' and tell me how many files matched. Just the number."),
        ask("Read the first 20 lines of Cargo.toml and tell me the resolver version."),
        ask("Using glob, how many .md files are at the repository root? Just the number."),
        ask("What is the edition set in Cargo.toml? Read it, do not guess."),
        ask("Ask the corpus what our deployment policy is."),
        ask("Read the file docs/nope-does-not-exist.md and tell me what happened."),
        ask("Grep for 'harnessd' in TODO.md and say how many lines matched. Just the number."),
        ask("In one sentence: why would re-rendering an old turn hurt a prefix cache?"),
        ask("Name the crate directory that holds the tool runtime. Use glob if you need to."),
        ask("What is 17 * 23? Just the number."),
        Step::SystemUpdate(
            "From now on, end every answer with the single word: ACKNOWLEDGED.".to_string(),
        ),
        ask("What is 4 + 4?"),
        ask("In one sentence: what is a token ledger?"),
        ask("Read Cargo.toml again and tell me the first workspace member listed."),
        ask("What is the capital of France?"),
        ask("Grep for 'TODO' in Cargo.toml. How many matches? Just the number."),
        ask("In one sentence: what does finish_reason 'length' mean?"),
        ask("Name one read-only tool you have."),
        ask("What is 100 divided by 4? Just the number."),
        ask("In one sentence: what is a stable prefix?"),
        ask("Use glob to list *.toml at the root. How many? Just the number."),
        ask("In one sentence: what is a tool result envelope for?"),
        ask("What is the third planet from the sun?"),
        ask("In one sentence: why is abstention not the same as an empty answer?"),
        ask("Say goodbye in one word."),
    ];
    v.truncate(turns.max(1));
    v
}

fn first_line(s: &str) -> String {
    let t = s.trim().lines().next().unwrap_or("").trim().to_string();
    if t.chars().count() > 60 {
        format!("{}…", t.chars().take(60).collect::<String>())
    } else {
        t
    }
}

/// The percentile convention every figure in this file uses, stated once.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn judge(
    rows: &[Row],
    vectors: &[Vec<TokenId>],
    prefix_at_open: &[TokenId],
    harness: &Harness,
    failures: &[String],
) -> Vec<String> {
    let mut out = Vec::new();

    // ---- C1: exact prefix extension of the token array -------------------
    let mut c1 = None;
    for w in vectors.windows(2) {
        if !w[1].starts_with(&w[0]) {
            let at = w[0]
                .iter()
                .zip(&w[1])
                .position(|(a, b)| a != b)
                .unwrap_or(w[1].len());
            c1 = Some(format!("diverged at token {at} of {}", w[0].len()));
            break;
        }
    }
    out.push(match c1 {
        None => format!(
            "C1  PASS  exact prefix extension over {} submissions of the token array",
            vectors.len() - 1
        ),
        Some(w) => format!("C1  FAIL  {w}"),
    });

    // ---- C2: the fidelity gate, which needs no model ---------------------
    out.push(
        "C2  NOT RUN  renderer fidelity is tests/fidelity/run_gate.py (GLM 139/139) and \
         crates/dialect-qwen/fidelity.py (Qwen 56/56 gating, 1 declared divergence). \
         Neither needs a model, so neither is re-run here."
            .to_string(),
    );

    // ---- C3: the generation-inclusive prefix invariant -------------------- Read off
    // `PrefixCheck`, which the engine computed at the moment it had the evidence, rather than
    // recomputed from `prompt + predicted`
    let mut c3_held = 0;
    let mut c3_short: Vec<String> = Vec::new();
    // Every shortfall is compared against the previous submission's generation, because one shape
    // of shortfall says exactly what the server did: reuse that stops at the end of the last
    // committed item, discarding the generation prompt and the whole generation with it.
    let mut c3_signature = 0;
    let mut c3_other = 0;
    let mut prev_predicted = 0u64;
    let mut c3_violated: Vec<String> = Vec::new();
    let mut c3_first = 0;
    let mut c3_skipped: Vec<String> = Vec::new();
    for r in rows {
        match &r.m.prefix_check {
            letibot_turn::PrefixCheck::FirstTurn => c3_first += 1,
            letibot_turn::PrefixCheck::Held { shortfall: 0, .. } => c3_held += 1,
            letibot_turn::PrefixCheck::Held {
                expected_cached_min,
                cached,
                shortfall,
            } => {
                if *shortfall == prev_predicted + 3 {
                    c3_signature += 1;
                } else {
                    c3_other += 1;
                }
                c3_short.push(format!(
                    "turn {}: the prompts are provably identical over {expected_cached_min} \
                     tokens but the server reused {cached} (short by {shortfall}; the \
                     previous submission generated {prev_predicted}), on slot {}",
                    r.turn, r.m.id_slot
                ))
            }
            letibot_turn::PrefixCheck::Violated { detail } => {
                c3_violated.push(format!("turn {}: {detail}", r.turn))
            }
            letibot_turn::PrefixCheck::Skipped { reason } => {
                c3_skipped.push(format!("turn {}: {reason}", r.turn))
            }
        }
        prev_predicted = r.m.predicted_tokens;
    }
    out.push(if !c3_violated.is_empty() {
        // The only verdict here that is a defect in us.
        format!(
            "C3  FAIL  {} submission(s) VIOLATED the generation-inclusive prefix \
             invariant:\n      {}",
            c3_violated.len(),
            c3_violated.join("\n      ")
        )
    } else if c3_short.is_empty() {
        format!(
            "C3  PASS  the invariant held with zero shortfall on all {c3_held} comparable \
             submissions ({c3_first} first-turn, {} skipped)",
            c3_skipped.len()
        )
    } else {
        format!(
            "C3  PASS*  the invariant was never violated — our prompt always extended the \
             previous one plus its generation — but the SERVER reused less than it could \
             have on {} of {} comparable submissions. A structural divergence cannot be \
             intermittent, so a shortfall on some turns and not others is a cache event \
             on the server, not a prefix bug here.\n      \
             {} of them carry one exact signature: shortfall = previous generation + 3, \
             i.e. the server reused everything up to the end of the last committed item \
             and discarded BOTH the 4-token generation prompt AND the whole generation. \
             {} do not fit that shape.\n      {}",
            c3_short.len(),
            c3_held + c3_short.len(),
            c3_signature,
            c3_other,
            c3_short.join("\n      ")
        )
    });

    // ---- C4: the f_keep distribution -------------------------------------- D11
    out.extend(c4(rows));

    // ---- C5: reasoning replay does not collapse the cache ------------------ Measured on
    // `f_keep`, which since D11 is the right metric for it.
    let after: Vec<&Row> = rows.iter().skip(1).filter(|r| r.after_reasoning).collect();
    let worst = after
        .iter()
        .filter_map(|r| r.m.f_keep())
        .fold(f64::INFINITY, f64::min);
    out.push(if after.is_empty() || !worst.is_finite() {
        "C5  NOT RUN  no submission followed an assistant turn carrying reasoning".to_string()
    } else if worst >= 0.90 {
        format!(
            "C5  PASS  {} submissions followed a reasoning-bearing turn; the worst f_keep \
             among them is {worst:.4}. Reasoning is in the token vector, so there is no \
             field to forget and nothing to collapse.",
            after.len()
        )
    } else {
        format!("C5  FAIL  worst f_keep after a reasoning-bearing turn is {worst:.4}")
    });

    // ---- C6: an empty assistant turn still owns its boundary tokens --------
    let mut c6_total = 0;
    let mut c6_bad = 0;
    for (i, item) in harness.items().iter().enumerate() {
        if let letibot_transcript::TranscriptItem::Assistant {
            text, tool_calls, ..
        } = item
            && text.is_empty()
            && !tool_calls.is_empty()
        {
            c6_total += 1;
            // The ledger row index equals the item index by construction.
            if harness.row_len(i) == 0 {
                c6_bad += 1;
            }
        }
    }
    out.push(if c6_total == 0 {
        "C6  NOT RUN  the session produced no tool-call-only assistant turn".to_string()
    } else if c6_bad == 0 {
        format!(
            "C6  PASS  {c6_total} tool-call-only assistant turn(s), all with non-empty \
             ledger rows (the token-path form of content:\"\" never null)"
        )
    } else {
        format!("C6  FAIL  {c6_bad} of {c6_total} such turns own no tokens")
    });

    // ---- C7 / C8 ----------------------------------------------------------
    out.push(
        "C7  NOT RUN  producing a truncated tool argument needs a `length` finish, which \
         needs an n_predict cap (§5.7 removed it and no test may add it back) or a \
         262,144-token context filled on a production box. The policy is covered \
         exhaustively offline in letibot_turn::length."
            .to_string(),
    );
    let carried = rows.iter().all(|r| !r.m.finish_reason.as_str().is_empty());
    let any_length = rows
        .iter()
        .any(|r| r.m.finish_reason == letibot_turn::FinishReason::Length);
    out.push(format!(
        "C8  {}  finish_reason reached turn_metrics on all {} submissions; {} were `length`. \
         A `length` with nothing usable is not constructible as a completed turn — TurnOk's \
         constructor asserts the verdict — so the half of C8 that is a decision is a type, \
         not a check.",
        if carried { "PARTIAL" } else { "FAIL" },
        rows.len(),
        if any_length { "some" } else { "none" },
    ));

    // ---- C9: a mid-session system change ----------------------------------
    let now = harness.tokens();
    let prefix_intact =
        now.len() >= prefix_at_open.len() && &now[..prefix_at_open.len()] == prefix_at_open;
    let update_at = harness.items().iter().position(|i| {
        matches!(i, letibot_transcript::TranscriptItem::System { .. })
            || matches!(i, letibot_transcript::TranscriptItem::User { parts, .. }
                if parts.iter().any(|p| matches!(p,
                    letibot_transcript::UserPart::Text { text }
                        if text.starts_with("<system-update seq="))))
    });
    let after_update: Vec<&Row> = match update_at {
        None => vec![],
        Some(_) => rows
            .iter()
            .skip_while(|r| r.kind != "system-update")
            .skip(1)
            .collect(),
    };
    let worst_after = after_update
        .iter()
        .filter_map(|r| r.m.f_keep())
        .fold(f64::INFINITY, f64::min);
    out.push(match (update_at, prefix_intact) {
        (None, _) => "C9  NOT RUN  the script contained no system change".to_string(),
        (Some(_), false) => {
            "C9  FAIL  the stable prefix changed; the system update rewrote the head of \
             the prompt rather than appending"
                .to_string()
        }
        (Some(at), true) if after_update.is_empty() => format!(
            "C9  PARTIAL  the update appended at item {at} and the stable prefix is \
             byte-identical, but no turn followed it, so the cache half is unmeasured"
        ),
        (Some(at), true) => format!(
            "C9  {}  the update appended at item {at}, the stable prefix ({} tokens) is \
             byte-identical, and the worst f_keep after it is {worst_after:.4}",
            if worst_after >= 0.90 { "PASS" } else { "FAIL" },
            prefix_at_open.len()
        ),
    });

    // ---- C10: the two sides agree on what the prompt was -------------------
    let mut c10_bad: Vec<String> = Vec::new();
    for r in rows {
        if r.m.prompt_tokens_server != r.m.prompt_tokens {
            c10_bad.push(format!(
                "turn {}: we submitted {} tokens, the server counted {}",
                r.turn, r.m.prompt_tokens, r.m.prompt_tokens_server
            ));
        } else if r.m.cached_tokens + r.m.prompt_processed != r.m.prompt_tokens_server {
            c10_bad.push(format!(
                "turn {}: cached {} + processed {} ≠ prompt {}",
                r.turn, r.m.cached_tokens, r.m.prompt_processed, r.m.prompt_tokens_server
            ));
        }
    }
    out.push(if c10_bad.is_empty() {
        format!(
            "C10 PASS  our prompt count equals the server's, and cached+processed = prompt, \
             on all {} submissions",
            rows.len()
        )
    } else {
        format!(
            "C10 FAIL  {} disagreement(s):\n      {}",
            c10_bad.len(),
            c10_bad.join("\n      ")
        )
    });

    // ---- the post-flight assertion the engine ran on every turn -------------
    let skipped: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.m.prefix_check, letibot_turn::PrefixCheck::Skipped { .. }))
        .collect();
    out.push(format!(
        "I1  {} of {} submissions ran the post-flight prefix assertion; {} skipped \
         (a skip is printed, never counted as a pass)",
        rows.len() - skipped.len(),
        rows.len(),
        skipped.len()
    ));

    if !failures.is_empty() {
        out.push(format!(
            "\nTURN FAILURES ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    out
}

/// C4's exit bar, for `f_keep` alone: its denominator is the entry the previous turn left in the
/// cache. The other ratio, cached over this prompt, falls by however much the conversation grew, so
/// no bar of 0.99 is reachable on it.
const C4_KEEP: f64 = 0.99;

/// C4 over the rows, with `f_sim` printed beside it under its own name. Pure, so one session can be
/// held against the bar without a model, a store and a slot, and it answers the lines a reader sees
/// rather than numbers only a test would agree with.
fn c4(rows: &[Row]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut keeps: Vec<f64> = rows.iter().filter_map(|r| r.m.f_keep()).collect();
    keeps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut repre: Vec<f64> = rows.iter().map(|r| r.m.reprefill() as f64).collect();
    repre.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p10 = percentile(&keeps, 0.10);
    lines.push(format!(
        "C4  {}  f_keep (D11: cached / the entry we left behind) p10 {:.4} p50 {:.4} \
         p99 {:.4} min {:.4} over {} comparable submissions; re-prefill p50 {:.0} \
         p99 {:.0} tokens  [exit criterion: p10 >= 0.99]",
        if p10 >= C4_KEEP { "PASS" } else { "FAIL" },
        p10,
        percentile(&keeps, 0.50),
        percentile(&keeps, 0.99),
        keeps.first().copied().unwrap_or(f64::NAN),
        keeps.len(),
        percentile(&repre, 0.50),
        percentile(&repre, 0.99),
    ));
    let below: Vec<String> = rows
        .iter()
        .filter(|r| r.m.f_keep().is_some_and(|f| f < C4_KEEP))
        .map(|r| {
            format!(
                "turn {}: f_keep {:.4}",
                r.turn,
                r.m.f_keep().unwrap_or(f64::NAN)
            )
        })
        .collect();
    lines.push(if below.is_empty() {
        format!(
            "        every one of the {} comparable submissions reused the whole entry \
             (f_keep = 1.0000 exactly)",
            keeps.len()
        )
    } else {
        format!(
            "        {} of {} below {C4_KEEP}, and they are exactly C3's shortfall turns: {}",
            below.len(),
            keeps.len(),
            below.join("; ")
        )
    });

    // `f_keep` above 1 is a fact about our denominator, not about the cache, and a
    // p50 printed as 1.0001 with no explanation is exactly the kind of number
    // somebody reads wrong. See `TurnMetrics::f_keep`.
    let over: Vec<i64> = rows
        .iter()
        .filter_map(|r| match &r.m.prefix_check {
            letibot_turn::PrefixCheck::Held {
                expected_cached_min,
                cached,
                ..
            } => Some(*cached as i64 - *expected_cached_min as i64),
            _ => None,
        })
        .filter(|d| *d > 0)
        .collect();
    if !over.is_empty() {
        let mut o = over.clone();
        o.sort_unstable();
        lines.push(format!(
            "        {} of {} came back ABOVE 1.0, by a median of {} and at most {} \
             token(s). Our denominator is a lower bound on the entry the server kept \
             — the boundary tokens the renderer strips before commit are re-rendered \
             identically next turn, and a turn that fails after prefilling warms the \
             slot while leaving no witness. Reported unclamped on purpose.",
            o.len(),
            keeps.len(),
            o[o.len() / 2],
            o[o.len() - 1],
        ));
    }

    // `f_sim` is the old reading of C4: cached over this prompt, so appending a tool result caps it
    // whatever the cache did. Nothing here compares it to `C4_KEEP`.
    let mut sims: Vec<f64> = rows.iter().filter_map(|r| r.m.f_sim()).collect();
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let ceilings: Vec<f64> = rows
        .iter()
        .filter_map(|r| match &r.m.prefix_check {
            letibot_turn::PrefixCheck::Held {
                expected_cached_min,
                ..
            } if r.m.prompt_tokens > 0 => {
                Some(*expected_cached_min as f64 / r.m.prompt_tokens as f64)
            }
            _ => None,
        })
        .collect();
    let mut c = ceilings.clone();
    c.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total_prompt: u64 = rows.iter().map(|r| r.m.prompt_tokens).sum();
    let total_repre: u64 = rows.iter().map(|r| r.m.reprefill()).sum();
    lines.push(format!(
        "f_sim   (NOT C4 — cached over this prompt, so appending a tool result caps it \
         whatever the cache did) p10 {:.4} p50 {:.4} p99 {:.4}; the ceiling these prompts \
         allow is p10 {:.4} p50 {:.4}, since appending a tool result caps it \
         regardless of the cache. Session-wide {} of {} prompt tokens were prefilled.",
        percentile(&sims, 0.10),
        percentile(&sims, 0.50),
        percentile(&sims, 0.99),
        percentile(&c, 0.10),
        percentile(&c, 0.50),
        total_repre,
        total_prompt,
    ));

    lines
}

fn die(msg: &str) -> ! {
    eprintln!("letibot-m1: {msg}");
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_backend::{Meter, TurnCost};
    use letibot_turn::{FinishReason, PrefixCheck};

    /// One submission as `judge` sees it. `entry` is what the previous turn left in the server's
    /// cache, `reused` is how much of it came back, and `prompt` is this turn's own size, the only
    /// number `f_sim` is about.
    fn row(turn: usize, prompt: u64, reused: u64, entry: u64) -> Row {
        Row {
            turn,
            kind: "ask",
            after_reasoning: false,
            m: TurnMetrics {
                turn_id: format!("t{turn}"),
                model: "m".into(),
                dialect_template_sha: "00".into(),
                ledger_head_sent: "ab".into(),
                prompt_tokens: prompt,
                cached_tokens: reused,
                predicted_tokens: 0,
                prompt_tokens_server: prompt,
                prompt_processed: prompt - reused,
                finish_reason: FinishReason::Eos,
                prompt_ms: 0.0,
                predicted_ms: 0.0,
                draft_n: 0,
                draft_n_accepted: 0,
                id_slot: 0,
                n_busy_slots: None,
                prefix_check: PrefixCheck::Held {
                    expected_cached_min: entry,
                    cached: reused,
                    shortfall: entry - reused,
                },
                cost: TurnCost {
                    meter: Meter::WallClock,
                    prompt_tokens: prompt,
                    cached_tokens: reused,
                    generated_tokens: 0,
                    wall_ms: 0,
                    micros_usd: None,
                },
                wall_ms: 0,
                first_byte_ms: None,
            },
        }
    }

    /// The turn that found the defect: the whole 9,000-token entry came back and the prompt still
    /// grew to 10,093 through a tool result. A pass on the entry, and the same numbers as a fraction
    /// of the prompt are 0.8917 and would fail.
    #[test]
    fn c4s_bar_is_about_the_entry_and_not_about_the_prompt() {
        let rows: Vec<Row> = (0..3).map(|i| row(i, 10_093, 9_000, 9_000)).collect();
        let lines = c4(&rows);
        assert!(
            lines[0].contains("C4  PASS"),
            "a session that reused every entry it was offered failed C4, so the bar is on the \
             wrong quantity: {}",
            lines[0]
        );
        assert!(lines[0].contains("p10 1.0000"), "{}", lines[0]);
        let sim = lines
            .iter()
            .find(|l| l.starts_with("f_sim"))
            .unwrap_or_else(|| panic!("no f_sim line in {lines:?}"));
        assert!(sim.contains("NOT C4"), "{sim}");
        // The case only means something while the other quantity is genuinely under the bar.
        assert!(sim.contains("p50 0.8917"), "{sim}");
        for weak in ["p10 0.8917", "p10 0.8920"] {
            assert!(!lines[0].contains(weak), "{weak} in {}", lines[0]);
        }
    }

    /// The converse, and the reason both directions are asserted: a bar applied to `f_sim` misses
    /// this one. Half the entry came back on a prompt that grew by a single token, so the fraction
    /// of the prompt is 0.998 and passes while the fraction of the entry is 0.500 and fails.
    #[test]
    fn a_server_that_reused_half_the_entry_fails_c4_though_f_sim_would_pass() {
        let rows = vec![row(0, 501, 500, 1_000), row(1, 501, 500, 1_000)];
        let lines = c4(&rows);
        assert!(
            lines[0].contains("C4  FAIL"),
            "half the entry came back and C4 passed: {}",
            lines[0]
        );
        assert!(lines[0].contains("p10 0.5000"), "{}", lines[0]);
        let sim = lines
            .iter()
            .find(|l| l.starts_with("f_sim"))
            .unwrap_or_else(|| panic!("no f_sim line in {lines:?}"));
        assert!(sim.contains("p50 0.9980"), "{sim}");
    }

    /// A first turn has nothing to compare against: no distribution, and NaN fails the bar rather
    /// than passing by vacancy.
    #[test]
    fn a_session_with_no_cached_entry_does_not_pass_c4_by_vacancy() {
        let mut first = row(0, 1_000, 0, 0);
        first.m.prefix_check = PrefixCheck::FirstTurn;
        let lines = c4(&[first]);
        assert!(
            lines[0].contains("C4  FAIL"),
            "a first turn is not a pass: {}",
            lines[0]
        );
        assert!(lines[0].contains("p10 NaN"), "{}", lines[0]);
    }
}
