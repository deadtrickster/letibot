//! **The metrics instruments on a real turn** (TODO R81 phase 1) — the canned server plays the
//! model, the turn runs through the production turn tail, and what is asserted is the rows the
//! instruments emitted on this thread (`letibot_metrics::capture`): one `turn.start` and one
//! `turn.end` keyed by the first round's id, a `round` per request with its time to first token,
//! a `tool.call` with its duration, outcome and bytes, and an R71 claim carrying the REAL window
//! (the `999999` the jsonl showed was a round trip through the ledger ratio).
//!
//! Needs the vocabulary GGUF and no model; no `metrics.db` is opened — the capture is the sink.

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Harness;
use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
use letibot_metrics::{Event, kind};
use letibot_sessionlog::hub::Hub;

#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(dir: &TempDir, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<canned::Frame> {
    ids.iter()
        .map(|id| canned::Frame::Token {
            id: *id,
            text: vocab.detokenize(&[*id], true).unwrap().leak(),
        })
        .collect()
}

fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str, n_prompt: u64) -> canned::Reply {
    let thought = vocab.tokenize_text("thinking").unwrap();
    let answer_ids = vocab.tokenize_text(answer).unwrap();
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    canned::Reply::Frames(frames)
}

/// One round that reads `rel` and ends pending, so the harness runs the call.
fn a_read(vocab: &letibot_tokencore::Vocab, rel: &str) -> canned::Reply {
    let call = format!("\n<function=read>\n<parameter=path>\n{rel}\n</parameter>\n</function>\n");
    let call_ids = vocab.tokenize_text(&call).unwrap();
    let mut frames = vec![
        canned::Frame::Progress {
            total: 30,
            processed: 30,
        },
        canned::Frame::Token {
            id: THINK_OPEN,
            text: "",
        },
        canned::Frame::Token {
            id: THINK_CLOSE,
            text: "",
        },
        canned::Frame::Token {
            id: TOOL_CALL_OPEN,
            text: "<tool_call>",
        },
    ];
    frames.extend(spoken(vocab, &call_ids));
    frames.push(canned::Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: call_ids.len() as u64 + 4,
        n_prompt: 30,
        cache_n: 0,
    });
    canned::Reply::Frames(frames)
}

fn of<'a>(got: &'a [Event], k: &str) -> Vec<&'a Event> {
    got.iter().filter(|e| e.kind == k).collect()
}

#[test]
fn a_turn_emits_its_start_end_rounds_and_tool_call_keyed_by_one_turn_id() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-metrics-turn");
    std::fs::write(dir.0.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let mut cfg = config(&dir, "metrics-turn");
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let serv = canned::Canned::serve_replies(
        vec![
            a_read(&parts.vocab, "notes.txt"),
            a_plain_answer(&parts.vocab, "Three lines.", 60),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new("metrics-turn");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");

    let (out, got) = letibot_metrics::capture(|| {
        run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
            h.submit("how many lines in notes.txt?").map(Some)
        })
    });
    let reply = out.expect("a turn ran").expect("the turn ends");
    assert_eq!(reply.rounds, 2);

    let rounds = of(&got, kind::ROUND);
    assert_eq!(rounds.len(), 2, "{got:#?}");
    let turn = rounds[0].turn_id.clone().expect("a turn id");
    assert_eq!(rounds[0].round, Some(0));
    assert_eq!(rounds[1].round, Some(1));
    assert_eq!(
        rounds[1].turn_id.as_deref(),
        Some(turn.as_str()),
        "one turn, two rounds"
    );
    assert!(rounds[0].n[0].unwrap_or(0) > 0, "the prompt's tokens");
    assert_eq!(
        rounds[0].get("server_prompt_tokens"),
        Some(&30.into()),
        "and what the server said it counted, beside them"
    );
    assert!(
        rounds.iter().all(|r| r.get("ttfb_ms").is_some()),
        "every round that generated has a time to first token: {rounds:#?}"
    );
    assert!(rounds[0].get("ledger_tokens").is_some(), "R72's other unit");

    let tools = of(&got, kind::TOOL_CALL);
    assert_eq!(tools.len(), 1, "{got:#?}");
    assert_eq!(tools[0].name.as_deref(), Some("read"));
    assert_eq!(tools[0].outcome.as_deref(), Some("ok"));
    assert_eq!(tools[0].turn_id.as_deref(), Some(turn.as_str()));
    assert_eq!(tools[0].round, Some(0));
    assert!(tools[0].dur_ms.is_some());
    assert!(tools[0].n[1].unwrap_or(0) > 0, "the result's bytes");

    let start = of(&got, kind::TURN_START);
    let end = of(&got, kind::TURN_END);
    assert_eq!((start.len(), end.len()), (1, 1), "{got:#?}");
    assert_eq!(start[0].name.as_deref(), Some("prompt"), "the door");
    assert_eq!(start[0].turn_id.as_deref(), Some(turn.as_str()));
    assert!(start[0].ts_ms <= end[0].ts_ms, "stamped when it began");
    assert_eq!(end[0].turn_id.as_deref(), Some(turn.as_str()));
    assert_eq!(end[0].outcome.as_deref(), Some("eos"));
    assert_eq!(end[0].get("rounds"), Some(&2.into()));
    assert_eq!(end[0].get("tool_calls"), Some(&1.into()));
    assert_eq!(end[0].get("tool_errors"), Some(&0.into()));
    assert_eq!(
        end[0].n[0],
        Some(rounds[0].n[0].unwrap() + rounds[1].n[0].unwrap()),
        "prompt tokens summed over both rounds"
    );
}

/// **The claim's window is the window** — 1,000,000 configured, 1,000,000 recorded; the jsonl's
/// `999999` was `shown_tokens(planning_window())`, a round trip through the ratio.
#[test]
fn a_context_claim_row_carries_the_real_window_and_its_sentence() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-metrics-claim");
    let mut cfg = config(&dir, "metrics-claim");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let excuse =
        "My context is at the wall, so I'll stop here rather than start a piece I can't finish.";
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(&parts.vocab, excuse, 30),
            a_plain_answer(&parts.vocab, "Continuing.", 30),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();
    let mut h = Harness::open(&parts, cfg, Hub::new("metrics-claim")).expect("opens");
    let (_, got) = letibot_metrics::capture(|| {
        run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
            h.submit("go on").map(Some)
        })
    });
    let claims = of(&got, kind::CONTEXT_CLAIM);
    assert_eq!(claims.len(), 1, "{got:#?}");
    assert_eq!(claims[0].outcome.as_deref(), Some("slapped"));
    assert_eq!(claims[0].n[1], Some(1_000_000), "the window as configured");
    assert!(claims[0].turn_id.is_some(), "placed on its turn");
    assert_eq!(
        claims[0].get("snippet").and_then(|v| v.as_str()),
        Some(excuse),
        "the sentence travels; the writer keeps it for this kind (decision 5)"
    );
}
