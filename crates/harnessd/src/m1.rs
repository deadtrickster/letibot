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
//! The run ends with one `M1 EXIT` line and exits 1 unless that line says `MET`. The criterion is
//! that the checks pass, so `NOT RUN` and `PARTIAL` count against it - the exit status used to be
//! "nothing said FAIL", which a run that measured nothing passed - and the two checks this
//! instrument cannot produce are named on the line rather than counted as failures.
//!
//! # `f_keep` here is D11's, and `f_sim` is printed beside it
//!
//! §18.2's C4 originally read `cached_tokens / prompt_tokens`, whose denominator is the *new*
//! prompt - llama's `f_sim`, which falls purely as a function of how much the conversation grew and
//! cannot reach 0.99 on any session that returns tool output. D11 settles C4 as `lcp /
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
        let after_reasoning = reasoned_in_the_last_turn(harness.items());
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
    let (met, line) = exit_verdict(&verdicts);
    println!("\n{line}");

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

    std::process::exit(if met { 0 } else { 1 });
}

/// The checks this instrument cannot produce, and where each is answered instead. Naming them is
/// what keeps the verdict honest: a run that is silent about C2 and C7 reads as if it ran them.
const MEASURED_ELSEWHERE: [(&str, &str); 2] = [
    (
        "C2",
        "the renderer gate, tests/fidelity, which needs no model",
    ),
    (
        "C7",
        "a truncated tool argument, which needs a server `length` and no n_predict cap exists",
    ),
];

/// The checks M1's exit criterion needs, which are the ones this instrument can produce. Written
/// as a list so a verdict that is missing is a failure rather than a smaller denominator.
const REQUIRED: [&str; 8] = ["C1", "C3", "C4", "C5", "C6", "C8", "C9", "C10"];

/// Whether M1's exit criterion is met, and the line that says so. The criterion is that C1-C10
/// pass, so a check that never ran has not passed: `NOT RUN`, `PARTIAL` and a check that is
/// missing are all not-passes here, which is the one place the whole run is allowed to be strict.
fn exit_verdict(verdicts: &[String]) -> (bool, String) {
    let mut states: Vec<(&str, &str)> = Vec::new();
    for v in verdicts {
        let mut words = v.split_whitespace();
        let Some(check) = words.next() else { continue };
        let Some(digits) = check.strip_prefix('C') else {
            continue;
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if MEASURED_ELSEWHERE.iter().any(|(c, _)| *c == check)
            || states.iter().any(|(c, _)| *c == check)
        {
            continue;
        }
        states.push((check, words.next().unwrap_or("")));
    }
    let mut not_pass: Vec<String> = Vec::new();
    for check in REQUIRED {
        match states.iter().find(|(c, _)| *c == check) {
            None => not_pass.push(format!("{check} MISSING")),
            Some((_, state)) if !state.starts_with("PASS") => {
                let state = if *state == "NOT" { "NOT RUN" } else { state };
                not_pass.push(format!("{check} {state}"));
            }
            Some(_) => {}
        }
    }
    let elsewhere: Vec<String> = MEASURED_ELSEWHERE
        .iter()
        .map(|(c, why)| format!("{c} ({why})"))
        .collect();
    let elsewhere = elsewhere.join(", ");
    if not_pass.is_empty() {
        return (
            true,
            format!(
                "M1 EXIT  MET  all {} checks this run can produce passed. Not measured here: \
                 {elsewhere}.",
                REQUIRED.len()
            ),
        );
    }
    (
        false,
        format!(
            "M1 EXIT  NOT MET  {} of {} checks are not a pass: {}. Not measured here either: \
             {elsewhere}.",
            not_pass.len(),
            REQUIRED.len(),
            not_pass.join(", ")
        ),
    )
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

/// C1: each submission's token vector must begin with the previous one's, and a divergence names its token.
/// No pair to compare is not a pass: a run whose every turn failed submitted nothing, the vacuous pass this instrument refuses.
fn c1(vectors: &[Vec<TokenId>]) -> String {
    for w in vectors.windows(2) {
        if !w[1].starts_with(&w[0]) {
            let at = w[0]
                .iter()
                .zip(&w[1])
                .position(|(a, b)| a != b)
                .unwrap_or(w[1].len());
            return format!("C1  FAIL  diverged at token {at} of {}", w[0].len());
        }
    }
    if vectors.len() < 2 {
        return "C1  NOT RUN  no submission was ever compared with the one before it".to_string();
    }
    format!(
        "C1  PASS  exact prefix extension over {} submissions of the token array",
        vectors.len().saturating_sub(1)
    )
}

/// C3: the generation-inclusive prefix invariant, read off the engine's `PrefixCheck`. A
/// `Violated` is the only verdict here that is a defect in us; a shortfall is a measurement of
/// the server, so it is reported apart and named `PASS*` rather than folded into a pass.
fn c3(rows: &[Row]) -> String {
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
    if !c3_violated.is_empty() {
        return format!(
            "C3  FAIL  {} submission(s) VIOLATED the generation-inclusive prefix \
             invariant:\n      {}",
            c3_violated.len(),
            c3_violated.join("\n      ")
        );
    }
    let comparable = c3_held + c3_short.len();
    if comparable == 0 {
        return format!(
            "C3  NOT RUN  no submission was compared with the one before it ({c3_first} \
             first-turn, {} skipped)",
            c3_skipped.len()
        );
    }
    if c3_short.is_empty() {
        return format!(
            "C3  PASS  the invariant held with zero shortfall on all {c3_held} comparable \
             submissions ({c3_first} first-turn, {} skipped)",
            c3_skipped.len()
        );
    }
    format!(
        "C3  PASS*  the invariant was never violated, so our prompt always extended the \
         previous one plus its generation, but the SERVER reused less than it could have on \
         {} of {} comparable submissions. A structural divergence cannot be intermittent, so \
         a shortfall on some turns and not others is a cache event on the server, not a \
         prefix bug here.\n      \
         {} of them carry one exact signature: shortfall = previous generation + 3, i.e. \
         the server reused everything up to the end of the last committed item and \
         discarded BOTH the 4-token generation prompt AND the whole generation. {} do not \
         fit that shape.\n      {}",
        c3_short.len(),
        comparable,
        c3_signature,
        c3_other,
        c3_short.join("\n      ")
    )
}

/// Whether the turn the model is about to continue carried a thought. The turn is what sits
/// between the user's own prompt and the end, so reasoning from an earlier turn answers for
/// that turn only: scanning the whole transcript backwards made every later row count as one.
fn reasoned_in_the_last_turn(items: &[letibot_transcript::TranscriptItem]) -> bool {
    use letibot_transcript::TranscriptItem;
    items
        .iter()
        .rev()
        .take_while(|it| !matches!(it, TranscriptItem::User { .. }))
        .any(|it| matches!(it, TranscriptItem::Reasoning { .. }))
}

/// The bar a replayed thought must clear: below this the cache collapsed on the submission that
/// carried it, and a collapse is the defect C5 exists to see.
const COLLAPSE_KEEP: f64 = 0.90;

/// C5: the submission that follows a turn which thought must still reuse what that turn left.
/// `f_keep` is the measure, so a collapse is a fall in the entry's reuse and not in the prompt's
/// growth.
fn c5(rows: &[Row]) -> String {
    let after: Vec<&Row> = rows.iter().skip(1).filter(|r| r.after_reasoning).collect();
    let worst = after
        .iter()
        .filter_map(|r| r.m.f_keep())
        .fold(f64::INFINITY, f64::min);
    if after.is_empty() || !worst.is_finite() {
        return "C5  NOT RUN  no submission followed an assistant turn carrying reasoning"
            .to_string();
    }
    if worst >= COLLAPSE_KEEP {
        format!(
            "C5  PASS  {} submissions followed a reasoning-bearing turn; the worst f_keep \
             among them is {worst:.4}. Reasoning is in the token vector, so there is no \
             field to forget and nothing to collapse.",
            after.len()
        )
    } else {
        format!("C5  FAIL  worst f_keep after a reasoning-bearing turn is {worst:.4}")
    }
}

/// C6: `content: ""` never becomes `null` on the token path, which shows as a tool-call-only
/// assistant item that still owns ledger tokens. Both numbers are counted where the ledger row
/// index and the item index are the same one.
fn c6(tool_call_only: usize, owning_no_tokens: usize) -> String {
    if tool_call_only == 0 {
        return "C6  NOT RUN  the session produced no tool-call-only assistant turn".to_string();
    }
    if owning_no_tokens == 0 {
        return format!(
            "C6  PASS  {tool_call_only} tool-call-only assistant turn(s), all with non-empty \
             ledger rows (the token-path form of content:\"\" never null)"
        );
    }
    format!("C6  FAIL  {owning_no_tokens} of {tool_call_only} such turns own no tokens")
}

/// C8: `finish_reason` reaches `turn_metrics` on every submission, and a `length` is never recorded as a completed turn.
/// The second half is a type (`TurnOk` refuses a hard-fail verdict; policy tested in `letibot_turn::length`). A live run must also see the server produce a `length`.
fn c8(rows: &[Row]) -> String {
    let dropped = rows
        .iter()
        .filter(|r| r.m.finish_reason.as_str().is_empty())
        .count();
    if dropped > 0 {
        return format!(
            "C8  FAIL  {dropped} of {} submissions reached turn_metrics with no finish_reason",
            rows.len()
        );
    }
    if rows.is_empty() {
        return "C8  NOT RUN  the run recorded no submission to read a finish_reason off"
            .to_string();
    }
    let lengths = rows
        .iter()
        .filter(|r| r.m.finish_reason == letibot_turn::FinishReason::Length)
        .count();
    format!(
        "C8  PARTIAL  finish_reason reached turn_metrics on all {} submissions; {lengths} were \
         `length`. A `length` with nothing usable is not constructible as a completed turn, \
         since TurnOk's constructor asserts the verdict, so the half of C8 that is a decision is \
         a type and not a check.",
        rows.len()
    )
}

/// C9: a system change appended at the tail leaves the stable prefix byte-identical, and the
/// submission after it still reuses what the one before left. A change that rewrote the head is
/// the failure; a change with no turn after it is only half measured.
fn c9(update_at: Option<usize>, prefix_intact: bool, prefix_len: usize, after: &[f64]) -> String {
    let Some(at) = update_at else {
        return "C9  NOT RUN  the script contained no system change".to_string();
    };
    if !prefix_intact {
        return "C9  FAIL  the stable prefix changed; the system update rewrote the head of \
                the prompt rather than appending"
            .to_string();
    }
    if after.is_empty() {
        return format!(
            "C9  PARTIAL  the update appended at item {at} and the stable prefix is \
             byte-identical, but no turn followed it, so the cache half is unmeasured"
        );
    }
    let worst = after.iter().copied().fold(f64::INFINITY, f64::min);
    format!(
        "C9  {}  the update appended at item {at}, the stable prefix ({prefix_len} tokens) is \
         byte-identical, and the worst f_keep after it is {worst:.4}",
        if worst >= COLLAPSE_KEEP {
            "PASS"
        } else {
            "FAIL"
        },
    )
}

/// C10: the two sides agree on what the prompt was, and the server's own count splits into the
/// part it reused and the part it processed. A run that asked the server nothing agreed with it
/// about nothing, so no submission is not a pass.
fn c10(rows: &[Row]) -> String {
    let mut bad: Vec<String> = Vec::new();
    for r in rows {
        if r.m.prompt_tokens_server != r.m.prompt_tokens {
            bad.push(format!(
                "turn {}: we submitted {} tokens, the server counted {}",
                r.turn, r.m.prompt_tokens, r.m.prompt_tokens_server
            ));
        } else if r.m.cached_tokens + r.m.prompt_processed != r.m.prompt_tokens_server {
            bad.push(format!(
                "turn {}: cached {} + processed {} ≠ prompt {}",
                r.turn, r.m.cached_tokens, r.m.prompt_processed, r.m.prompt_tokens_server
            ));
        }
    }
    if rows.is_empty() {
        return "C10 NOT RUN  the run submitted nothing to compare a count with".to_string();
    }
    if bad.is_empty() {
        return format!(
            "C10 PASS  our prompt count equals the server's, and cached+processed = prompt, \
             on all {} submissions",
            rows.len()
        );
    }
    format!(
        "C10 FAIL  {} disagreement(s):\n      {}",
        bad.len(),
        bad.join("\n      ")
    )
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
    out.push(c1(vectors));

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
    out.push(c3(rows));

    // ---- C4: the f_keep distribution -------------------------------------- D11
    out.extend(c4(rows));

    // ---- C5: reasoning replay does not collapse the cache ------------------ Measured on
    // `f_keep`, which since D11 is the right metric for it.
    out.push(c5(rows));

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
    out.push(c6(c6_total, c6_bad));

    // ---- C7 / C8 ----------------------------------------------------------
    out.push(
        "C7  NOT RUN  producing a truncated tool argument needs a `length` finish, which \
         needs an n_predict cap (§5.7 removed it and no test may add it back) or a \
         262,144-token context filled on a production box. The policy is covered \
         exhaustively offline in letibot_turn::length."
            .to_string(),
    );
    out.push(c8(rows));

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
    let after_update: Vec<f64> = match update_at {
        None => vec![],
        Some(_) => rows
            .iter()
            .skip_while(|r| r.kind != "system-update")
            .skip(1)
            .filter_map(|r| r.m.f_keep())
            .collect(),
    };
    out.push(c9(
        update_at,
        prefix_intact,
        prefix_at_open.len(),
        &after_update,
    ));

    // ---- C10: the two sides agree on what the prompt was -------------------
    out.push(c10(rows));

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
         p99 {:.0} tokens  [exit criterion: p10 >= {C4_KEEP}]",
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

    /// The bar the line prints is the bar the line judged by. They were written twice, so a bar
    /// that moved would leave a line saying PASS beside a criterion that reads differently.
    #[test]
    fn c4_prints_the_bar_it_judges_by() {
        let lines = c4(&[row(0, 1_000, 990, 1_000), row(1, 1_000, 990, 1_000)]);
        assert!(
            lines[0].contains(&format!("exit criterion: p10 >= {C4_KEEP}")),
            "{}",
            lines[0]
        );
        // Exactly the bar is a pass; a hair under it is not.
        assert!(lines[0].contains("C4  PASS"), "{}", lines[0]);
        let under = c4(&[row(0, 1_000, 989, 1_000), row(1, 1_000, 989, 1_000)]);
        assert!(under[0].contains("C4  FAIL"), "{}", under[0]);
    }

    /// C5's verdict, both ways: a thought that came back intact passes, a thought whose entry
    /// the server then dropped fails, and a session that never reasoned has nothing to say.
    #[test]
    fn c5_fails_where_the_entry_the_thought_left_collapsed() {
        let mut reasoned = row(0, 1_000, 990, 1_000);
        reasoned.after_reasoning = true;
        let mut kept = row(1, 1_100, 1_000, 1_000);
        kept.after_reasoning = true;
        let line = c5(&[reasoned, kept]);
        assert!(line.contains("C5  PASS"), "{line}");

        let mut dropped = row(1, 1_100, 500, 1_000);
        dropped.after_reasoning = true;
        let line = c5(&[row(0, 1_000, 990, 1_000), dropped]);
        assert!(line.contains("C5  FAIL"), "{line}");
        assert!(line.contains("0.5000"), "{line}");

        let unread = c5(&[row(0, 1_000, 990, 1_000)]);
        assert!(unread.contains("C5  NOT RUN"), "{unread}");
    }

    /// M1's exit is "C1-C10 pass", which a run's exit status has to mean. Every verdict line the
    /// instrument can print is a state, and only a pass is one of the passes.
    #[test]
    fn a_failed_check_fails_the_run_however_the_line_is_spaced() {
        // The two-space form is how C1 and C3 print a failure.
        let (met, line) = exit_verdict(&[
            "C1  PASS  exact prefix extension over 29 submissions".to_string(),
            "C3  FAIL  1 submission(s) VIOLATED the invariant".to_string(),
        ]);
        assert!(!met, "a divergent prompt left the run exiting 0: {line}");
        assert!(line.contains("C3 FAIL"), "{line}");
        assert!(!line.contains("C1 C1"), "{line}");
    }

    /// A check that never ran has not passed, which is why the run has a verdict of its own: the
    /// exit status used to be "nothing said FAIL", and NOT RUN passed it.
    #[test]
    fn a_check_that_never_ran_is_not_a_pass() {
        let (met, line) = exit_verdict(&[
            "C1  PASS  exact prefix extension over 29 submissions".to_string(),
            "C6  NOT RUN  the session produced no tool-call-only assistant turn".to_string(),
        ]);
        assert!(
            !met,
            "a check that never ran left the run exiting 0: {line}"
        );
        assert!(line.contains("C6 NOT RUN"), "{line}");
    }

    /// A check the run never printed at all is not a pass either: the criterion needs every one of
    /// them, and a verdict list with a hole in it would otherwise be counted as whole.
    #[test]
    fn a_check_the_run_never_printed_is_not_a_pass() {
        let all: Vec<String> = ["C1", "C3", "C4", "C5", "C6", "C8", "C10"]
            .iter()
            .map(|c| format!("{c}  PASS  held"))
            .collect();
        let (met, line) = exit_verdict(&all);
        assert!(!met, "C9 was missing and the run was called met: {line}");
        assert!(line.contains("C9 MISSING"), "{line}");
    }

    /// The two checks this instrument cannot produce are named on the line and not counted as
    /// failures, and `PASS*` is a pass: C3's shortfall is a measurement of the server.
    #[test]
    fn the_verdict_names_what_this_run_can_never_measure() {
        let all: Vec<String> = ["C1", "C3", "C4", "C5", "C6", "C8", "C9", "C10"]
            .iter()
            .map(|c| format!("{c}  PASS*  held"))
            .collect();
        let (met, line) = exit_verdict(&all);
        assert!(met, "{line}");
        assert!(line.contains("all 8 checks"), "{line}");
        assert!(
            line.contains("C2 ("),
            "the renderer gate is not named: {line}"
        );
        assert!(
            line.contains("C7 ("),
            "the truncated call is not named: {line}"
        );
        let (met, line) = exit_verdict(&[]);
        assert!(!met, "a run that measured nothing was called met: {line}");
    }

    /// C10's two ways of disagreeing with the server, which are different facts: a count that is
    /// not ours, and a count that does not split into reused plus processed.
    #[test]
    fn c10_names_the_count_that_disagreed() {
        let agreed = c10(&[row(0, 1_000, 990, 1_000)]);
        assert!(agreed.contains("C10 PASS"), "{agreed}");
        assert!(agreed.contains("on all 1 submissions"), "{agreed}");

        let mut counted = row(1, 1_000, 990, 1_000);
        counted.m.prompt_tokens_server = 1_005;
        let line = c10(&[counted]);
        assert!(line.contains("C10 FAIL"), "{line}");
        assert!(
            line.contains("we submitted 1000 tokens, the server counted 1005"),
            "{line}"
        );

        let mut split = row(2, 1_000, 990, 1_000);
        split.m.prompt_processed = 8;
        let line = c10(&[split]);
        assert!(line.contains("C10 FAIL"), "{line}");
        assert!(
            line.contains("cached 990 + processed 8 ≠ prompt 1000"),
            "{line}"
        );
    }

    /// A run that asked nothing agreed with the server about nothing.
    #[test]
    fn a_run_with_no_submission_does_not_pass_c10_by_vacancy() {
        let line = c10(&[]);
        assert!(
            line.contains("C10 NOT RUN"),
            "an empty run was reported as agreement: {line}"
        );
    }

    /// C9's four verdicts: no change to make is not a pass, a rewritten head is the failure, a
    /// change nothing followed is half measured, and a change the next submission reused is the
    /// pass the script is written to reach.
    #[test]
    fn c9_tells_an_appended_change_from_a_rewritten_head() {
        assert!(c9(None, true, 900, &[1.0]).contains("C9  NOT RUN"));
        let rewritten = c9(Some(12), false, 900, &[1.0]);
        assert!(rewritten.contains("C9  FAIL"), "{rewritten}");
        let alone = c9(Some(12), true, 900, &[]);
        assert!(alone.contains("C9  PARTIAL"), "{alone}");
        assert!(alone.contains("item 12"), "{alone}");
        let kept = c9(Some(12), true, 900, &[1.0, 0.995, 0.999]);
        assert!(kept.contains("C9  PASS"), "{kept}");
        assert!(kept.contains("(900 tokens)"), "{kept}");
        assert!(kept.contains("0.9950"), "{kept}");
        let dropped = c9(Some(12), true, 900, &[1.0, 0.5]);
        assert!(dropped.contains("C9  FAIL"), "{dropped}");
    }

    /// C8's three verdicts. A submission whose reason never reached the metrics is the failure,
    /// a run with no submission to read one off has not been asked, and a run that read them all
    /// but saw no `length` has the evidence half and not the server half.
    #[test]
    fn c8_reads_the_reason_off_every_submission() {
        let partial = c8(&[row(0, 1_000, 990, 1_000)]);
        assert!(partial.contains("C8  PARTIAL"), "{partial}");
        assert!(partial.contains("0 were `length`"), "{partial}");
        let mut cut = row(1, 1_100, 1_000, 1_000);
        cut.m.finish_reason = FinishReason::Length;
        let seen = c8(&[cut]);
        assert!(seen.contains("1 were `length`"), "{seen}");
        let mut lost = row(2, 1_200, 1_100, 1_100);
        lost.m.finish_reason = FinishReason::Other("");
        let fail = c8(&[lost]);
        assert!(fail.contains("C8  FAIL"), "{fail}");
        assert!(fail.contains("1 of 1"), "{fail}");
    }

    /// A run that recorded nothing read no reason off anything.
    #[test]
    fn a_run_with_no_submission_does_not_pass_c8_by_vacancy() {
        let line = c8(&[]);
        assert!(
            line.contains("C8  NOT RUN"),
            "an empty run was reported as a reading: {line}"
        );
    }

    /// C6's three verdicts. A tool-call-only turn that owns tokens is the token path's answer to
    /// `content: ""` never being `null`, one that owns none is the failure, and a session that
    /// never made such a turn was not asked the question.
    #[test]
    fn c6_reads_the_turns_that_carry_a_call_and_no_text() {
        let pass = c6(3, 0);
        assert!(pass.contains("C6  PASS"), "{pass}");
        assert!(pass.contains("3 tool-call-only"), "{pass}");
        let fail = c6(3, 1);
        assert!(fail.contains("C6  FAIL"), "{fail}");
        assert!(fail.contains("1 of 3"), "{fail}");
        let none = c6(0, 0);
        assert!(none.contains("C6  NOT RUN"), "{none}");
    }

    /// A thought belongs to the turn that made it. Reading the whole transcript backwards marked
    /// every later submission as following a reasoning turn, which is not what C5 measures.
    #[test]
    fn a_thought_from_an_earlier_turn_does_not_answer_for_this_one() {
        use letibot_transcript::{ReasoningField, Speaker, TranscriptItem, UserPart};
        let user = |text: &str| TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: text.to_string(),
            }],
            speaker: Speaker::Operator,
        };
        let thought = TranscriptItem::Reasoning {
            text: "thinking it over".into(),
            field: ReasoningField::ReasoningContent,
            truncated: false,
        };
        let answer = TranscriptItem::Assistant {
            text: "here".into(),
            tool_calls: vec![],
            truncated: false,
        };

        let reasoned = [user("one"), thought.clone(), answer.clone()];
        assert!(reasoned_in_the_last_turn(&reasoned));
        let later = [user("one"), thought, answer.clone(), user("two"), answer];
        assert!(
            !reasoned_in_the_last_turn(&later),
            "the second turn reasoned nothing, and an earlier thought was counted for it"
        );
    }

    /// C1's reading, both ways: a prompt that does not extend the last one fails and says where,
    /// and one that does is a pass over the number of pairs actually compared.
    #[test]
    fn c1_fails_on_a_divergence_and_names_the_token() {
        let line = c1(&[vec![1, 2, 3, 4], vec![1, 2, 3, 4, 5], vec![1, 2, 9, 4, 5]]);
        assert!(line.contains("C1  FAIL"), "{line}");
        assert!(line.contains("diverged at token 2 of 5"), "{line}");
        let pass = c1(&[vec![1, 2], vec![1, 2, 3], vec![1, 2, 3, 4]]);
        assert!(pass.contains("C1  PASS"), "{pass}");
        assert!(pass.contains("over 2 submissions"), "{pass}");
    }

    /// The run that failed every turn: the only vector is the prefix the session opened with, so
    /// no pair was ever compared and there is nothing to be exact about.
    #[test]
    fn a_run_that_submitted_nothing_does_not_pass_c1_by_vacancy() {
        let line = c1(&[vec![1, 2, 3]]);
        assert!(
            line.contains("C1  NOT RUN"),
            "zero pairs compared was reported as a pass: {line}"
        );
    }

    /// C3's three verdicts, which are three different claims: a `Violated` is a defect in us, a
    /// shortfall is the server reusing less than it could, and a shortfall carrying the exact
    /// signature the 30-turn run showed is the server dropping our generation.
    #[test]
    fn c3_fails_only_where_our_own_prompt_diverged() {
        let mut first = row(1, 500, 400, 400);
        first.m.predicted_tokens = 97;
        first.m.prefix_check = PrefixCheck::Held {
            expected_cached_min: 400,
            cached: 400,
            shortfall: 0,
        };
        let line = c3(std::slice::from_ref(&first));
        assert!(line.contains("C3  PASS"), "{line}");
        assert!(line.contains("all 1 comparable"), "{line}");

        // The next submission's shortfall is exactly this one's generation plus 3.
        let mut short = row(2, 520, 400, 400);
        short.m.prefix_check = PrefixCheck::Held {
            expected_cached_min: 400,
            cached: 300,
            shortfall: 100,
        };
        let line = c3(&[first, short]);
        assert!(line.contains("C3  PASS*"), "{line}");
        assert!(line.contains("short by 100"), "{line}");
        assert!(
            line.contains("1 of them carry one exact signature"),
            "{line}"
        );

        let mut violated = row(3, 600, 0, 0);
        violated.m.prefix_check = PrefixCheck::Violated {
            detail: "prompt 600 does not begin with 520 plus 20".into(),
        };
        let line = c3(&[violated]);
        assert!(line.contains("C3  FAIL"), "{line}");
        assert!(line.contains("turn 3: prompt 600"), "{line}");
    }

    /// A run that recorded no submission has measured no submission, so the invariant was not
    /// held over zero turns: it was never tried.
    #[test]
    fn a_run_with_no_submission_does_not_pass_c3_by_vacancy() {
        let line = c3(&[]);
        assert!(
            line.contains("C3  NOT RUN"),
            "an empty run was reported as a pass: {line}"
        );
    }
}
