//! **R66's experiment at CI scale**: does a daemon's heap plateau at `sessions × (what one
//! session's harness holds)`, or does it grow past it?
//!
//! # The experiment this is the small shape of
//!
//! The full one is the brief's: a scratch daemon with **50 live children, each running 200 turns
//! of 1 MiB tool results** — 10 GiB of rows in total — where `heap live` must plateau at
//! `50 × (ceilings)` and any slope above that is the term still unaccounted for. That probe is
//! documented in `.letibot/notes/r66-the-full-probe-and-what-it-would-prove.md`; it is not in CI
//! because it needs 10 GiB and half an hour.
//!
//! This file is the same shape divided down: **6 sessions × 12 turns × 64 KiB** — ÷8 sessions,
//! ÷17 turns, ÷16 row size — which is 4.5 MiB of rows and about ten seconds. What it can settle at
//! that scale, and what it cannot, is in §"What this proves" below.
//!
//! # The instrument
//!
//! `memwatch::Counting`, the daemon's own allocator, installed in a test binary of its own so
//! `memwatch::heap()` is this process's live heap and nothing else's. `RssShmem` from
//! `/proc/self/status` is the other half: **a token region is a memfd**, so its pages are never
//! heap — the footprint counts them and the heap does not, and this file checks both against
//! their own instrument rather than adding them together.
//!
//! # What this proves
//!
//! 1. The census's number is the transcript's: the gauges agree with a fresh walk over
//!    `Session::items`, at every turn, including after a compaction fork.
//! 2. The heap grows by what the sessions hold and not more: the per-turn cost is the same in
//!    the last third as in the first — a per-turn copy of the conversation would grow with it.
//! 3. The whole heap is the SUM over sessions, not the product: `N × footprint`, and the tolerance
//!    is stated below with the arithmetic that justifies it.
//! 4. A fork gives it back: a session whose conversation is replaced by a summary stops holding
//!    the conversation, which is the mechanism by which a long-lived session plateaus at all.
//!
//! # What it does NOT prove, and the full probe is needed for
//!
//! A session with NO wall does not plateau here — the transcript is every row it ever appended,
//! and only compaction (or a closed session) takes that away. So this file measures the bound's
//! *shape*; whether a real 200-turn session stays under its ceiling depends on the wall firing,
//! which is the full probe's question and not this one's.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::ForkTail;
use letibot_harnessd::memwatch::{self, Counting};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::{Footprint, Hub};
use letibot_sessionlog::log::LogBounds;
use letibot_sessionlog::view::ViewBounds;
use letibot_turn::CompactionOutcome;

/// **The allocator, counted** — the daemon's own, so what this file measures is what `memwatch`
/// prints on the box.
#[global_allocator]
static ALLOC: Counting = Counting;

/// Sessions held at once — the brief's 50, divided by 8.
const SESSIONS: usize = 6;

/// Turns each runs — the brief's 200, divided by 17.
const TURNS: usize = 12;

/// Bytes one turn appends — the brief's 1 MiB, divided by 16.
const TURN_BYTES: usize = 64 * 1024;

/// **The tolerance, as a number.** What the heap may hold beyond the rows the sessions appended,
/// per session, after `TURNS` turns.
///
/// The arithmetic, term by term, for one session at this scale:
///
/// | term | bytes | why |
/// |---|---|---|
/// | the rows | `TURNS × TURN_BYTES` = 768 KiB | the transcript holds every row verbatim |
/// | the trail | ≤ 144 KiB | 60 utterances × 600 characters, and this file's rows are `System` rows, so ~0 |
/// | the hub's log | ≤ 64 KiB | [`SMALL_LOG`], which is the bound and not a measurement |
/// | the hub's view | ≤ 64 KiB | [`SMALL_VIEW`], same |
/// | the model's answers | ~`TURNS × 1 KiB` = 12 KiB | one `ok` per turn, and its row |
/// | per-session harness overhead | ~300 KiB | measured: a bare session costs 0.1 MiB (R66's own note) and this one carries a store, a runtime and a registry entry |
///
/// which is 768 + 144 + 64 + 64 + 12 + 300 ≈ 1.3 MiB, so 1.75× the rows is the number with room
/// for a `Vec`'s doubling and for the allocator's own slack — and it is a BOUND and not a target:
/// a per-turn copy of the conversation would be 12× the rows by the last turn.
const SLACK_PER_SESSION: f64 = 1.75;

/// **One test at a time.** Both tests here point `XDG_CONFIG_HOME` at their own providers file,
/// and the environment is the process's: two of them in parallel would have one session open on
/// the other's model URL. `--include-ignored` is the only way to run both, and this is what makes
/// that safe rather than flaky.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The hub's own bounds in this file, small on purpose: a hub's log and view are ALREADY bounded
/// (16 MiB and 8 MiB by default) and are not what this file is measuring, so they are cut down
/// until the harness's footprint is the largest term in the heap.
const SMALL_LOG: usize = 64 << 10;
const SMALL_VIEW: usize = 64 << 10;

/// A directory that removes itself — a test that leaks a store per run eventually fills the disk,
/// and the run that finds out is not this one.
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

/// A provider that answers every request with one word, in the frames the provider parses.
///
/// It reads the request before answering — a stub that writes first can fail against a client
/// still sending, and that failure looks exactly like a model error.
fn a_model_that_says_ok() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let _ = answer(stream);
        }
    });
    format!("http://127.0.0.1:{port}/v1/chat/completions")
}

fn answer(mut s: TcpStream) -> std::io::Result<()> {
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
    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    body.push_str("data: [DONE]\n\n");
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    s.flush()
}

/// `VmRSS` and `RssShmem` for this process — the half of the picture the allocator cannot see.
fn rss() -> (u64, u64) {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let get = |key: &str| -> u64 {
        s.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    (get("VmRSS:"), get("RssShmem:"))
}

fn mb(n: f64) -> String {
    format!("{:.1} MiB", n / (1024.0 * 1024.0))
}

/// A hub whose own terms are small, so the harness's footprint is the largest thing in the heap.
fn a_small_hub(session: &str) -> std::sync::Arc<Hub> {
    Hub::with_bounds(
        session,
        LogBounds {
            scrollback_events: 200,
            scrollback_bytes: SMALL_LOG,
            max_gap_events: 100,
            max_gap_bytes: SMALL_LOG,
            snapshot_items: 50,
        },
        ViewBounds {
            items: 50,
            item_bytes: SMALL_VIEW,
            settled_decisions: 20,
            warnings: 20,
        },
    )
}

/// A session that opens on the byte vocabulary and answers every turn, and the hub it answers on.
fn opened(dir: &TempDir, url: &str, session: &str) -> (Harness, std::sync::Arc<Hub>) {
    let config = dir.path().join("config");
    std::fs::create_dir_all(config.join("letibot")).expect("the config dir");
    std::fs::write(
        config.join("letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/footprint_plateau.rs\n[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .expect("providers.toml");
    // SAFETY: one test in this binary, and the value is only ever read by this fixture.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };

    let mut cfg = Config::for_this_box(dir.path());
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    // No `--bash`: a session with an exec backend needs a delegated cgroup, which a test inside a
    // boundary does not have, and nothing here runs a command.
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
    // **No wall.** The byte vocabulary makes message zero tens of thousands of "tokens", so a
    // realistic window would compact on the first turn and this file would measure compaction
    // instead of the ordinary turn — which phase 4 asks for by name.
    cfg.context_window = Some(4_000_000);
    cfg.store = Some(dir.path().join("sessions.db"));
    cfg.session_id = session.into();

    let parts = Parts::load(&cfg).expect("a provider session opens on the byte vocabulary");
    let hub = a_small_hub(&cfg.session_id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session must open");
    (h, hub)
}

fn outcome(summary: &str) -> CompactionOutcome {
    CompactionOutcome {
        turn_id: "r66-footprint-summary-turn".into(),
        summary: summary.into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    }
}

/// **The transcript, walked** — the number the census is compared against, computed the slow way
/// and from the rows themselves, so a gauge that has gone stale is a red test rather than a
/// plausible number.
fn walked(h: &Harness) -> (usize, usize) {
    (
        h.items().len(),
        h.items()
            .iter()
            .map(letibot_transcript::TranscriptItem::bytes)
            .sum(),
    )
}

#[test]
fn the_heap_plateaus_at_what_the_sessions_hold() {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let url = a_model_that_says_ok();
    let dir = TempDir::new("harnessd-r66-footprint");

    // ---- 1. Six sessions, opened and measured ----
    let mut held: Vec<(Harness, std::sync::Arc<Hub>)> = Vec::new();
    for i in 0..SESSIONS {
        held.push(opened(&dir, &url, &format!("r66-footprint-{i}")));
    }
    // **One warm-up turn each, before the baseline.** A session's first turn pays costs it never
    // pays again — the hub's log and view filling to their bounds, the store's first write — and a
    // slope measured across those is a slope of the warm-up and not of the turns. It is the same
    // reason the full probe runs a minute before it starts reading.
    for (i, (h, _)) in held.iter_mut().enumerate() {
        let mut text = format!("warm-up for session {i}: ");
        text.push_str(&"w".repeat(TURN_BYTES - text.len()));
        h.system_update(&text)
            .expect("the warm-up turn must answer");
    }
    let (heap0, _) = (memwatch::heap().0 as u64, 0);
    let (_, shmem0) = rss();
    println!(
        "\n{SESSIONS} sessions open: heap live {} · shmem {} · footprints {:?}",
        mb(heap0 as f64),
        mb(shmem0 as f64),
        held.iter()
            .map(|(h, _)| h.footprint().total())
            .collect::<Vec<_>>()
    );

    // ---- 2. The turns, and the heap after each one ----
    let mut per_turn: Vec<(usize, u64)> = Vec::new();
    for turn in 0..TURNS {
        for (i, (h, _)) in held.iter_mut().enumerate() {
            let mut text = format!("turn {turn} of session {i}: ");
            text.push_str(&"x".repeat(TURN_BYTES - text.len()));
            h.system_update(&text).expect("the turn must answer");
        }
        let (heap, _) = (memwatch::heap().0 as u64, 0);
        per_turn.push((turn, heap));
        if turn % 3 == 0 || turn + 1 == TURNS {
            let total: usize = held.iter().map(|(h, _)| h.footprint().total()).sum();
            println!(
                "turn {turn:>2} · heap live {} · Σ footprint {} · {} row(s) per session",
                mb(heap as f64),
                mb(total as f64),
                held[0].0.items().len()
            );
        }
    }

    // ---- 3. The census is the transcript's, at every session ----
    let mut footprint_total = 0usize;
    let mut held_total = 0usize;
    for (h, hub) in &held {
        let fp = h.footprint();
        let (rows, text) = walked(h);
        assert_eq!(fp.items, rows, "the gauge's row count is the transcript's");
        assert_eq!(
            fp.items_text_bytes, text,
            "the gauge's text bytes are the rows' own — a stale gauge is a number nobody can act on"
        );
        assert_eq!(
            fp.ledger_tokens,
            h.ledger_len(),
            "the gauge's token count is the ledger's"
        );
        // **The region's arithmetic, as documented**: 4 bytes a token, and its capacity doubles,
        // so it is at least the tokens and at most twice them (plus the page-rounded header).
        let floor = fp.ledger_tokens * std::mem::size_of::<letibot_tokencore::TokenId>();
        assert!(
            fp.ledger_region_bytes >= floor,
            "the region is mapped at {} for {} tokens — less than the tokens themselves",
            fp.ledger_region_bytes,
            fp.ledger_tokens
        );
        assert!(
            fp.ledger_region_bytes <= floor * 2 + (64 << 10),
            "the region is mapped at {} for {} tokens — more than the doubling allows",
            fp.ledger_region_bytes,
            fp.ledger_tokens
        );
        // And the hub's census carries the same numbers, which is the whole registration.
        let c = hub.census();
        assert_eq!(c.footprint, fp, "the hub's census is the harness's answer");
        assert!(
            !c.footprint.over_ceiling(),
            "{} rows in a 4 000 000-token window must be under the ceiling: {:?}",
            c.footprint.items,
            c.footprint
        );
        footprint_total += fp.total();
        held_total += fp.items_text_bytes + fp.trail_bytes;
    }

    // ---- 4. The heap is the SUM over sessions, within the stated tolerance ----
    let heap_end = memwatch::heap().0 as u64;
    let (_, shmem_end) = rss();
    let grew = heap_end.saturating_sub(heap0);
    let per_session_room = (TURNS * TURN_BYTES) as f64 * SLACK_PER_SESSION;
    let allowed = (SESSIONS as f64 * per_session_room) as u64;
    println!(
        "\nafter {TURNS} turns each: heap live {} → {} (+{}) · shmem {} → {} (+{})",
        mb(heap0 as f64),
        mb(heap_end as f64),
        mb(grew as f64),
        mb(shmem0 as f64),
        mb(shmem_end as f64),
        mb(shmem_end.saturating_sub(shmem0) as f64),
    );
    println!(
        "Σ footprint {} ({} of it heap: rows + trail; {} of it shmem: the token regions)",
        mb(footprint_total as f64),
        mb(held_total as f64),
        mb(footprint_total.saturating_sub(held_total) as f64)
    );
    println!(
        "allowed growth {} = {SESSIONS} × {} ({} of rows × {SLACK_PER_SESSION})",
        mb(allowed as f64),
        mb(per_session_room),
        mb((TURNS * TURN_BYTES) as f64)
    );
    // **And the token regions are real memory, just not heap**: `RssShmem` grew with them, and by
    // less than the mapping — a doubling's tail is capacity nobody has written yet.
    //
    // **On a box with no `/proc` this half says nothing rather than failing.** `rss()` answers
    // zeroes on Darwin, and "this kernel cannot report it" is not "the regions took no memory":
    // the same rule `apparatus::present` follows. The heap half above is portable and is the one
    // the bound rests on.
    let shmem_grew = shmem_end.saturating_sub(shmem0);
    let region_total = (footprint_total - held_total) as u64;
    if shmem_end == 0 {
        println!(
            "no /proc/self/status on this box: the shmem check is skipped — the regions are \
             mapped at {} and this kernel cannot say what of it is resident",
            mb(region_total as f64)
        );
    } else {
        assert!(
            shmem_grew > 0,
            "the token regions took no resident memory at all, which a memfd mapping cannot do"
        );
        assert!(
            shmem_grew <= region_total,
            "the regions are mapped at {} but took {} resident — more written than exists",
            mb(region_total as f64),
            mb(shmem_grew as f64)
        );
    }
    assert!(
        grew <= allowed,
        "{SESSIONS} sessions × {TURNS} turns of {} grew the heap by {} — more than the {} the \
         arithmetic allows. That is a term nobody has accounted for, which is what R66 is",
        mb(TURN_BYTES as f64),
        mb(grew as f64),
        mb(allowed as f64)
    );

    // **And the heap is not smaller than what the sessions hold**: the rows are on it, so a
    // measurement below them would mean the footprint is counting something else.
    assert!(
        grew >= (SESSIONS * TURNS * TURN_BYTES) as u64,
        "the heap grew by {} but the sessions appended {} of rows, which cannot be held for less",
        mb(grew as f64),
        mb((SESSIONS * TURNS * TURN_BYTES) as f64)
    );

    // ---- 5. The per-turn cost does not grow with the conversation ----
    //
    // A per-turn copy of the conversation has a slope that grows with the conversation; a
    // per-turn cost that is O(one row) has the same slope at the start and at the end.
    let mid = per_turn[TURNS / 2].1 as f64;
    let per_turn_early = (mid - heap0 as f64) / (TURNS / 2) as f64;
    let per_turn_late = (heap_end as f64 - mid) / (TURNS / 2) as f64;
    let one_round = (SESSIONS * TURN_BYTES) as f64;
    println!(
        "per turn: {} early · {} late · one turn appends {} across {SESSIONS} sessions",
        mb(per_turn_early),
        mb(per_turn_late),
        mb(one_round)
    );
    for (when, cost) in [("early", per_turn_early), ("late", per_turn_late)] {
        assert!(
            cost <= one_round * SLACK_PER_SESSION,
            "the {when} turns cost {} each for {} appended — more than {SLACK_PER_SESSION}× is a \
             copy of something a session already held",
            mb(cost),
            mb(one_round)
        );
    }
    assert!(
        per_turn_late <= per_turn_early * 1.5,
        "a turn cost {} late and {} early: a cost that grows with the conversation is the shape \
         R66 is about",
        mb(per_turn_late),
        mb(per_turn_early)
    );

    // ---- 6. A fork gives the conversation back ----
    //
    // This is the only thing that makes a long-lived session plateau: the wall forks, the
    // conversation is replaced by a summary, and what the session holds drops to the summary.
    let before = held[0].0.footprint();
    let report = held[0]
        .0
        .fork_to_summary(
            &outcome("the conversation was summarised; nothing was carried but this"),
            None,
            None,
            ForkTail::NONE,
        )
        .expect("the fork must land");
    let after = held[0].0.footprint();
    println!(
        "\nfork → {} · rows {} → {} · text {} → {} · region {} → {} · total {} → {}",
        report.transcript_id,
        before.items,
        after.items,
        mb(before.items_text_bytes as f64),
        mb(after.items_text_bytes as f64),
        mb(before.ledger_region_bytes as f64),
        mb(after.ledger_region_bytes as f64),
        mb(before.total() as f64),
        mb(after.total() as f64),
    );
    assert!(
        after.items < before.items,
        "a fork carries the summary and not the conversation"
    );
    assert!(
        after.items_text_bytes < before.items_text_bytes / 2,
        "the fork kept {} of the {} it held",
        mb(after.items_text_bytes as f64),
        mb(before.items_text_bytes as f64)
    );
    // And the census says the same thing the harness does, after the swap: the gauges are
    // restated at the fork's own `reconcile`, which is the one place a wholesale replacement goes
    // through.
    assert_eq!(held[0].1.census().footprint, after);
    let (rows, text) = walked(&held[0].0);
    assert_eq!((after.items, after.items_text_bytes), (rows, text));
    let _: Footprint = after;
}

// ---------------------------------------------------------------------------
// The full probe — the brief's 50 × 200 × 1 MiB, run by hand
// ---------------------------------------------------------------------------

/// **The brief's own experiment, at its own scale, behind `--ignored`.**
///
/// `cargo test --release -p letibot-harnessd --test footprint_plateau -- --ignored --nocapture`
/// with `R66_SESSIONS`, `R66_TURNS`, `R66_TURN_BYTES` and `R66_WINDOW` set. The defaults are the
/// brief's: 50 sessions, 200 turns, 1 MiB a turn — 10 GiB of rows.
///
/// # What to read, and what each number proves
///
/// | number | reading |
/// |---|---|
/// | `Σ footprint / Σ rows` | 1.0–1.5 means the daemon holds exactly the work its sessions appended and nothing else. A number that climbs as turns go on is a per-turn copy of the conversation, which is the shape R66's 113 GB is made of. |
/// | `heap` vs `Σ rows` | the same question for the heap alone (the rows and the trail; the token region is a memfd and lands in `shmem`, not the heap). |
/// | `shmem` vs `Σ region` | the region is real memory — it is just not heap. A `shmem` of zero while `Σ region` is gigabytes would mean the mapping is never written, which would be a finding. |
/// | `per turn early` vs `per turn late` | the slope. A late slope more than 1.5× the early one is growth that feeds on itself. |
/// | `worst session` vs `its ceiling` | whether any session is past the bound the census checks — the number `memwatch` would print as `BOUND BROKEN in <session>`. |
///
/// # What it answers, and the one thing it cannot
///
/// It drives `Harness::system_update`, which appends a row and runs the turn — **and which does
/// not consult the wall.** Compaction is attempted at the between-turn doors (`wake`, `nag_turn`,
/// `run_after_reseat`, `continue_after_fallback`) and on a provider's refusal; a turn's own rows go
/// in unconditionally, because a turn that has something to say says it. So this probe measures
/// the UNBOUNDED case on purpose: 50 × 200 × 1 MiB is 10 GiB of text and ~40 GiB of token region,
/// nothing but the session ending takes any of it away, and VERDICT 3 says whether that is past
/// the ceiling. *The daemon holds the work and nothing else, and the work is unbounded* is the
/// answer it gives — a finding about the WALL and not about the census.
///
/// The walled half is a different instrument: a live daemon, where every turn is followed by a
/// wake. Read `memwatch`'s line and its census there — `harness <total>` on the summary line, the
/// `sessions whose harness holds most` block, and `BOUND BROKEN in <session>: this session's
/// harness holds …`. `.letibot/notes/r66-the-full-probe-and-what-it-would-prove.md` has both
/// halves written out with the numbers to read.
///
/// `R66_WINDOW` sets the session's window, which is what the CEILING is computed from — so it is
/// the knob that decides what VERDICT 3 compares against, and a transcript grown past a small
/// window by a door that does not consult the wall is exactly the session `memwatch` will name.
///
/// It is not in CI: 10 000 turns is ~20 minutes and the rows are ~15 GiB with the regions, so the
/// run needs a raised memory cap (`memory: uncapped`, or a box with room).
#[test]
#[ignore = "the brief's own scale: 50 sessions × 200 turns × 1 MiB — run it by hand, see the note"]
fn the_full_probe() {
    let env_usize = |name: &str, default: usize| -> usize {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let sessions = env_usize("R66_SESSIONS", 50);
    let turns = env_usize("R66_TURNS", 200);
    let row = env_usize("R66_TURN_BYTES", 1 << 20);
    let window = env_usize("R66_WINDOW", 4_000_000) as u64;

    let url = a_model_that_says_ok();
    let dir = TempDir::new("harnessd-r66-full-probe");
    let mut held: Vec<(Harness, std::sync::Arc<Hub>)> = Vec::new();
    for i in 0..sessions {
        let (mut h, hub) = opened(&dir, &url, &format!("r66-probe-{i}"));
        // The window is the probe's own knob, and it is the number the CEILING is computed from:
        // a session's transcript is meant to sit inside its window, so this is what VERDICT 3
        // compares against.
        h.config_mut().context_window = Some(window);
        held.push((h, hub));
    }
    // **One warm-up turn each, before the baseline** — the same reason the CI-scale test does it:
    // a session's first turn pays the hub's log and view filling to their bounds and the store's
    // first write, and those are constants per session, not costs per turn.
    for (i, (h, _)) in held.iter_mut().enumerate() {
        let mut text = format!("probe warm-up for session {i}: ");
        text.push_str(&"w".repeat(row.saturating_sub(text.len())));
        h.system_update(&text)
            .expect("the warm-up turn must answer");
    }
    let heap0 = memwatch::heap().0 as u64;
    let (_, shmem0) = rss();
    println!(
        "\nfull probe: {sessions} sessions × {turns} turns × {} · window {window} tokens · heap {} \
         · shmem {}",
        mb(row as f64),
        mb(heap0 as f64),
        mb(shmem0 as f64)
    );

    let mut mid = 0u64;
    for turn in 0..turns {
        for (i, (h, _)) in held.iter_mut().enumerate() {
            let mut text = format!("probe turn {turn} of session {i}: ");
            text.push_str(&"x".repeat(row.saturating_sub(text.len())));
            h.system_update(&text).expect("the turn must answer");
        }
        if turn + 1 == turns / 2 {
            mid = memwatch::heap().0 as u64;
        }
        if (turn + 1) % (turns / 10).max(1) == 0 {
            let total: usize = held.iter().map(|(h, _)| h.footprint().total()).sum();
            let worst = held
                .iter()
                .map(|(h, _)| h.footprint().total())
                .max()
                .unwrap_or(0);
            println!(
                "turn {:>4} · heap {} · shmem {} · Σ footprint {} · worst session {} · rows/session {}",
                turn + 1,
                mb(memwatch::heap().0 as f64),
                mb(rss().1 as f64),
                mb(total as f64),
                mb(worst as f64),
                held[0].0.items().len(),
            );
        }
    }

    let heap_end = memwatch::heap().0 as u64;
    let (_, shmem_end) = rss();
    let appended = (sessions * turns * row) as f64;
    let footprints: Vec<Footprint> = held.iter().map(|(h, _)| h.footprint()).collect();
    let total: usize = footprints.iter().map(|f| f.total()).sum();
    let region: usize = footprints.iter().map(|f| f.ledger_region_bytes).sum();
    let worst = footprints
        .iter()
        .max_by_key(|f| f.total())
        .expect("a session");
    let early = (mid.saturating_sub(heap0)) as f64 / (turns / 2).max(1) as f64;
    let late = (heap_end.saturating_sub(mid)) as f64 / (turns - turns / 2).max(1) as f64;
    println!(
        "\n{} of rows appended · heap {} → {} (+{}) · shmem {} → {} (+{})",
        mb(appended),
        mb(heap0 as f64),
        mb(heap_end as f64),
        mb((heap_end.saturating_sub(heap0)) as f64),
        mb(shmem0 as f64),
        mb(shmem_end as f64),
        mb((shmem_end.saturating_sub(shmem0)) as f64),
    );
    println!(
        "Σ footprint {} ({:.2}× the rows, and the extra is the token region: 4 bytes a token, \
         and a byte vocabulary's token is a byte of RENDERED text) · Σ region {} · per turn {} \
         early, {} late ({:.2}×)",
        mb(total as f64),
        total as f64 / appended.max(1.0),
        mb(region as f64),
        mb(early),
        mb(late),
        late / early.max(1.0),
    );
    println!(
        "worst session {} of its {} ceiling · {} row(s)",
        mb(worst.total() as f64),
        mb(worst.ceiling_bytes as f64),
        worst.items
    );

    // **The three readings as verdicts**, so a probe run does not have to be interpreted from
    // memory. None of them is a CI assertion: this test is the brief's experiment and its numbers
    // are the answer.
    //
    // The first is asked of the HEAP and not of the footprint, because the two hold different
    // things: the rows and the trail are on the heap, and a token region is a memfd, so its pages
    // are `RssShmem`. Adding them would be adding two measurements of two memories.
    let heap_grew = (heap_end.saturating_sub(heap0)) as f64;
    let heap_per_row = heap_grew / appended.max(1.0);
    println!(
        "VERDICT 1 — the heap holds the work and nothing else ({} of heap per byte of row, and \
         1.75× is the CI tolerance): {}",
        format!("{heap_per_row:.2}"),
        if heap_per_row <= 1.75 {
            "yes"
        } else {
            "NO — a term above the rows, and this is where R66 lives"
        }
    );
    let tokens: usize = footprints.iter().map(|f| f.ledger_tokens).sum();
    let bytes_per_token = region as f64 / tokens.max(1) as f64;
    println!(
        "VERDICT 1b — the regions are the doubling the footprint claims ({bytes_per_token:.2} \
         bytes a token, 4 at capacity and 8 just after a doubling) and they are RESIDENT: {}",
        if shmem_end == 0 {
            "cannot say — this box has no /proc/self/status to read RssShmem from"
        } else if bytes_per_token <= 8.0 && shmem_end > shmem0 {
            "yes"
        } else {
            "NO — either the capacity arithmetic is wrong or the region is never written"
        }
    );
    println!(
        "VERDICT 2 — the slope is flat: {}",
        if late <= early * 1.5 {
            "yes"
        } else {
            "NO — a turn costs more late than early, so something grows with the conversation"
        }
    );
    println!(
        "VERDICT 3 — every session is inside its ceiling: {}",
        if worst.total() <= worst.ceiling_bytes {
            "yes (the transcript is bounded, which means the wall is doing its work)"
        } else {
            "NO — this is the session `memwatch` names with `BOUND BROKEN`"
        }
    );
}
