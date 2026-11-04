//! The compaction-retry line: what one failed compaction attempt leaves behind, and what N accumulate. `memwatch::Counting` is this binary's allocator, so `memwatch::heap` is the live heap exactly, plus the harness's census.
//!
//! The fake is a loopback provider whose answer the test chooses, so a turn and a refused compaction are real, with no GGUF or server and any history size.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::memwatch::{self, Counting};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::log::LogBounds;
use letibot_sessionlog::view::ViewBounds;
use letibot_transcript::TranscriptItem;
use letibot_turn::compaction::SUMMARY_SECTIONS;

#[global_allocator]
static ALLOC: Counting = Counting;

/// One test at a time: the allocator is the process's, so two tests in parallel measure each other.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the fake answers.
const SAY_OK: u8 = 0;
const REFUSE: u8 = 1;
const PROPOSE_A_TOOL: u8 = 2;

/// Bytes one history row carries.
const ROW_BYTES: usize = 64 * 1024;

/// The answer the fake gives when it is asked for a summary.
const SUMMARY: &str = "## Objective\n- measure what a failed compaction leaves\n";

struct Fake {
    url: String,
    mode: Arc<AtomicU8>,
    asked: Arc<AtomicU64>,
}

impl Fake {
    fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        let mode = Arc::new(AtomicU8::new(SAY_OK));
        let asked = Arc::new(AtomicU64::new(0));
        let (m, a) = (mode.clone(), asked.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                a.fetch_add(1, Ordering::Relaxed);
                let _ = answer(stream, m.load(Ordering::Relaxed));
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            mode,
            asked,
        }
    }

    fn answer_with(&self, mode: u8) {
        self.mode.store(mode, Ordering::Relaxed);
    }
}

fn answer(mut s: TcpStream, mode: u8) -> std::io::Result<()> {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    let mut want: Option<usize> = None;
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if want.is_none()
            && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let len = acc[..at]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            want = Some(at + 4 + len);
        }
        if let Some(total) = want
            && acc.len() >= total
        {
            break;
        }
    }
    if mode == REFUSE {
        let body = r#"{"error":{"message":"the request was refused","code":"invalid_request"}}"#;
        return write!(
            s,
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .and_then(|()| s.flush());
    }
    let mut body = String::new();
    if mode == PROPOSE_A_TOOL {
        body.push_str(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\
             \"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n\n",
        );
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n");
    } else {
        for line in SUMMARY.lines() {
            body.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
                serde_json::Value::String(format!("{line}\n"))
            ));
        }
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    s.flush()
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
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

/// A hub whose own terms are small, so the harness is the largest thing this file measures.
fn a_small_hub(session: &str) -> Arc<Hub> {
    Hub::with_bounds(
        session,
        LogBounds {
            scrollback_events: 200,
            scrollback_bytes: 64 << 10,
            max_gap_events: 100,
            max_gap_bytes: 64 << 10,
            snapshot_items: 50,
        },
        ViewBounds {
            items: 50,
            item_bytes: 64 << 10,
            settled_decisions: 20,
            warnings: 20,
        },
    )
}

fn opened(dir: &TempDir, url: &str, session: &str) -> (Harness, Arc<Hub>) {
    opened_with(dir, url, session, |_| {})
}

/// [`opened`], with the last word on the config - the prompt a `/reseat` has to differ from.
fn opened_with(
    dir: &TempDir,
    url: &str,
    session: &str,
    tweak: impl FnOnce(&mut Config),
) -> (Harness, Arc<Hub>) {
    let config = dir.path().join("config");
    std::fs::create_dir_all(config.join("letibot")).expect("the config dir");
    std::fs::write(
        config.join("letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/r66_compaction.rs\n[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .expect("providers.toml");
    // SAFETY: one test in this binary, and the value is only ever read by this fixture.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };

    let mut cfg = Config::for_this_box(dir.path());
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = false;
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    // No wall: the byte vocabulary makes message zero tens of thousands of "tokens", and a
    // pre-emptive compaction fired by a history row would measure the wall, not the retry.
    cfg.auto_compact = false;
    cfg.context_window = Some(8_000_000);
    cfg.store = Some(dir.path().join("sessions.db"));
    cfg.session_id = session.into();
    tweak(&mut cfg);

    let parts = Parts::load(&cfg).expect("a provider session opens on the byte vocabulary");
    let hub = a_small_hub(&cfg.session_id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session must open");
    (h, hub)
}

fn mb(n: f64) -> String {
    format!("{:.2} MiB", n / (1024.0 * 1024.0))
}

/// What the session holds, walked rather than read off a gauge.
#[derive(Debug, Clone, Copy)]
struct Held {
    items: usize,
    text: usize,
    tokens: usize,
    census: usize,
    heap: u64,
    /// The process's high-water mark, so one attempt's transient is a number and not a claim.
    peak: u64,
}

fn held(h: &Harness) -> Held {
    let (live, peak, _) = memwatch::heap();
    Held {
        items: h.items().len(),
        text: h.items().iter().map(TranscriptItem::bytes).sum(),
        tokens: h.ledger_len(),
        census: h.footprint().items_text_bytes,
        heap: live as u64,
        peak: peak as u64,
    }
}

fn history(h: &mut Harness, rows: usize, tag: &str) {
    for i in 0..rows {
        let mut text = format!("{tag} row {i}: ");
        text.push_str(&"h".repeat(ROW_BYTES - text.len()));
        h.system_update(&text)
            .expect("the history turn must answer");
    }
}

/// The rows that ASK for a record, found by what they ask for and not by their bytes: the
/// instruction names the room it has, so two attempts need not spell it the same way. The eight
/// sections are what makes a row that ask, and nothing else this fixture writes names one.
fn instructions(h: &Harness) -> Vec<String> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::System { text, .. }
                if SUMMARY_SECTIONS.iter().all(|s| text.contains(s)) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

/// What ONE refused compaction attempt leaves behind: nothing. Forty of them on a 2 MiB conversation
/// leave the transcript where each attempt found it, because a fold that fails rolls back its own
/// instruction, and the cost per attempt does not move with the conversation's size.
#[test]
fn a_refused_compaction_leaves_the_transcript_where_it_found_it() {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    let dir = TempDir::new("harnessd-r66-compaction-refused");
    let (mut h, _hub) = opened(&dir, &fake.url, "r66-compaction-refused");

    // 32 rows × 64 KiB = 2 MiB of conversation, so a retained copy of it per attempt would be
    // 80 MiB over 40 attempts and impossible to mistake for the instruction.
    history(&mut h, 32, "warm-up");
    let base = held(&h);
    println!(
        "conversation: {} item(s), {} of text, {} token(s), heap {}",
        base.items,
        mb(base.text as f64),
        base.tokens,
        mb(base.heap as f64)
    );
    assert!(
        base.text >= 32 * ROW_BYTES,
        "the history must be the size the arithmetic assumes"
    );

    fake.answer_with(REFUSE);
    let asked_before = fake.asked.load(Ordering::Relaxed);
    let mut per: Vec<Held> = Vec::new();
    let mut prev = base;
    for i in 0..40 {
        let out = h.compact();
        assert!(out.is_err(), "attempt {i} must fail: the fake refused it");
        let now = held(&h);
        if i < 3 || i + 1 == 40 {
            println!(
                "attempt {:>2}: +{} item(s), +{} byte(s) of text, +{} token(s), heap {:+} · peak \
                 +{}",
                i + 1,
                now.items - prev.items,
                now.text - prev.text,
                now.tokens - prev.tokens,
                now.heap as i64 - prev.heap as i64,
                now.peak - prev.peak
            );
        }
        per.push(now);
        prev = now;
        assert_eq!(
            instructions(&h).len(),
            0,
            "attempt {} left a row that asks for a record",
            i + 1
        );
    }
    let end = *per.last().expect("forty attempts");
    let asked = fake.asked.load(Ordering::Relaxed) - asked_before;

    // (1) The transcript is what it was. Not one row, one byte or one token of the forty
    // instructions, and none of the notices a salvage wrote beside them, stays behind: a session
    // whose folds keep failing would otherwise carry a copy of the instruction per attempt.
    assert_eq!(
        end.items,
        base.items,
        "40 refused attempts left {} item(s) within",
        end.items.saturating_sub(base.items)
    );
    assert_eq!(
        end.text,
        base.text,
        "40 refused attempts left {} byte(s) of text",
        end.text.saturating_sub(base.text)
    );
    assert_eq!(
        end.tokens, base.tokens,
        "the token region is where the first attempt found it"
    );
    assert!(
        asked >= 40,
        "each attempt must reach the model at least once, and {asked} of 40 did"
    );

    // (2) The heap cost per attempt is flat, and it is nothing like a copy of the conversation.
    let early = (per[9].heap - base.heap) as f64 / 10.0;
    let late = (end.heap - per[29].heap) as f64 / 10.0;
    println!(
        "heap per attempt: {} early · {} late · total {} for 40 attempts on a {} conversation",
        mb(early),
        mb(late),
        mb(end.heap.saturating_sub(base.heap) as f64),
        mb(base.text as f64)
    );
    assert!(
        late <= early * 1.5 + 4096.0,
        "a refused attempt cost {} late and {} early: a cost that rises with the conversation is \
         a copy of something the session already held",
        mb(late),
        mb(early)
    );
    assert!(
        end.heap - base.heap < 40 * 64 * 1024,
        "40 refused attempts grew the heap by {} on a {} conversation: more than 40 × 64 KiB is \
         something retained per attempt",
        mb(end.heap.saturating_sub(base.heap) as f64),
        mb(base.text as f64)
    );

    // (3) The census has walked the transcript the attempts left, so the watchdog sees it.
    assert_eq!(
        end.census,
        end.text,
        "the footprint census is {} byte(s) behind the transcript after 40 failed attempts",
        end.text.saturating_sub(end.census)
    );
}

/// `/reseat summarise` runs the same summary turn `/compact` does, so a refused one leaves nothing
/// either: a session whose re-seats keep failing must not carry a copy of the instruction per
/// attempt. The census is what proves it, since it is walked off the transcript.
#[test]
fn a_refused_reseat_leaves_the_census_where_it_found_the_transcript() {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    let dir = TempDir::new("harnessd-r66-reseat-refused");
    let session = "r66-reseat-refused";
    let (mut h, _hub) = opened(&dir, &fake.url, session);
    history(&mut h, 8, "warm-up");
    let base = held(&h);
    assert_eq!(
        base.census, base.text,
        "the census is level before anything is refused"
    );
    drop(h);

    // The daemon comes back with a prompt the conversation does not speak, which is the only thing
    // that gives `/reseat` something to re-seat onto.
    let (mut h, _hub) = opened_with(&dir, &fake.url, session, |cfg| {
        cfg.system = format!("{}\n\nAnd one more standing instruction.", cfg.system);
    });
    assert!(
        h.reseat_target().expect("probing").is_some(),
        "the seated prompt differs, so a re-seat has a target"
    );
    let before = held(&h);
    fake.answer_with(REFUSE);
    for i in 0..5 {
        assert!(
            h.reseat().is_err(),
            "attempt {i} must fail: the fake refused the summary turn"
        );
    }
    let end = held(&h);
    assert_eq!(
        instructions(&h).len(),
        0,
        "a refused attempt left a row that asks for a record"
    );
    assert_eq!(
        end.items,
        before.items,
        "5 refused `/reseat summarise` attempts left {} item(s) within",
        end.items.saturating_sub(before.items)
    );
    assert_eq!(
        end.text,
        before.text,
        "5 refused `/reseat summarise` attempts left {} byte(s) of text",
        end.text.saturating_sub(before.text)
    );
    assert_eq!(
        end.census,
        end.text,
        "the footprint census is {} byte(s) behind the transcript after 5 refused `/reseat \
         summarise` attempts",
        end.text.saturating_sub(end.census)
    );
}

/// The other failure: the summary turn ran and wrote its answer, and the compaction was refused for what the answer proposed. What stays is the instruction plus that answer, one turn's output per attempt, not the conversation.
/// The hub pairs the refusal's warning with that instruction, and the error tells the model to retry on the transcript it can see.
#[test]
fn a_compaction_that_ran_and_was_refused_leaves_its_own_answer_too() {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    let dir = TempDir::new("harnessd-r66-compaction-ran");
    let (mut h, _hub) = opened(&dir, &fake.url, "r66-compaction-ran");

    history(&mut h, 8, "warm-up");
    let base = held(&h);
    fake.answer_with(PROPOSE_A_TOOL);

    let mut per: Vec<Held> = Vec::new();
    let mut prev = base;
    for i in 0..6 {
        let out = h.compact();
        let said = out.err().map(|e| format!("{e}"));
        assert!(
            said.as_deref().is_some_and(|s| s.contains("tool call")),
            "attempt {i} must be refused for what the summary proposed, got {said:?}"
        );
        let now = held(&h);
        println!(
            "attempt {:>2}: +{} item(s), +{} byte(s) of text, +{} token(s), heap {:+}",
            i + 1,
            now.items - prev.items,
            now.text - prev.text,
            now.tokens - prev.tokens,
            now.heap as i64 - prev.heap as i64
        );
        per.push(now);
        prev = now;
    }
    let end = *per.last().expect("six attempts");

    // The instruction and the answer, two items per attempt, and the answer is one turn's output.
    assert_eq!(
        end.items - base.items,
        12,
        "instruction + answer, six times"
    );
    let said = instructions(&h);
    assert_eq!(said.len(), 6, "one instruction row per attempt");
    let asked_bytes: usize = said.iter().map(String::len).sum();
    let answers = end.text - base.text - asked_bytes;
    println!(
        "per attempt: +2 item(s), +{} byte(s) of instruction and {answers} byte(s) of answers in all",
        said[0].len()
    );
    assert!(
        answers < 6 * 4096,
        "six refused-but-ran attempts left {answers} byte(s) beside their instructions, which is \
         more than a short answer apiece"
    );
    // And the residue does not grow from attempt to attempt: each attempt left its own instruction
    // plus the same answer, so nothing in it is keyed by the attempt number.
    let answer = answers / 6;
    assert_eq!(answers, answer * 6, "six attempts, six short answers");
    for (i, text) in said.iter().enumerate() {
        let before = if i == 0 { base.text } else { per[i - 1].text };
        assert_eq!(
            per[i].text - before,
            text.len() + answer,
            "attempt {} left its instruction and one answer",
            i + 1
        );
    }
    assert_eq!(
        end.census, end.text,
        "the census must have walked the rows the attempts left"
    );
}

/// The control, and the reason a session that cannot compact stays where it is: a compaction that
/// LANDS replaces the conversation with one row, so the whole history goes back. A failed one
/// leaves every row it was meant to fold, and adds none.
#[test]
fn a_compaction_that_lands_gives_the_conversation_back_and_a_failed_one_does_not() {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let fake = Fake::start();
    let dir = TempDir::new("harnessd-r66-compaction-lands");
    let (mut h, _hub) = opened(&dir, &fake.url, "r66-compaction-lands");

    history(&mut h, 8, "warm-up");
    let before = held(&h);
    println!(
        "before: {} item(s), {} of text, {} token(s)",
        before.items,
        mb(before.text as f64),
        before.tokens
    );

    // Refused first: the conversation stays exactly as it was, and nothing joins it.
    fake.answer_with(REFUSE);
    assert!(h.compact().is_err(), "the refusal must fail the compaction");
    let after_refusal = held(&h);
    assert_eq!(
        after_refusal.items, before.items,
        "a refused compaction leaves the conversation where it found it"
    );

    // Then allowed: the same conversation, folded, is one row.
    fake.answer_with(SAY_OK);
    let report = h.compact().expect("the compaction must land");
    let after = held(&h);
    println!(
        "after a landed compaction: {} item(s), {} of text, {} token(s); {} was {} token(s)",
        after.items,
        mb(after.text as f64),
        after.tokens,
        report.fork.transcript_id,
        report.fork.was_tokens
    );
    assert!(
        after.items < before.items / 2 && after.text < before.text / 8,
        "the fork must give the conversation back: {} item(s) and {} of text after, {} and {} \
         before",
        after.items,
        mb(after.text as f64),
        before.items,
        mb(before.text as f64)
    );
    assert!(
        after.tokens < before.tokens,
        "the fork must shrink the ledger: {} after, {} before",
        after.tokens,
        before.tokens
    );
    assert_eq!(
        after.census, after.text,
        "the census follows a fork to the new base"
    );
}
