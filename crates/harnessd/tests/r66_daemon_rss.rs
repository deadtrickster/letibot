//! The daemon's memory instrument: a scratch daemon serving one long session against a fake model, with its
//! resident memory and its heap sampled after every turn. The daemon prints `memwatch` lines of its
//! own, so the heap is the allocator's count and not a guess from VmRSS.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// A section header of `letibot_turn::compaction::SUMMARY_INSTRUCTION`: what tells a summary request
/// from an ordinary turn without this file owning a copy of the whole instruction. A phrase with no
/// line break in it, because the instruction's own line breaks arrive JSON-escaped.
const SUMMARY_MARKER: &str = "## Objective";

/// The summary endpoint's refusal. A compaction that fails this way appends its instruction and
/// nothing else, which is the state the killed daemon was in.
const REFUSAL: &str = "{\"error\":{\"message\":\"this endpoint does not summarise\"}}";

/// What the fake model has been asked, so a test can say which turn a number belongs to.
#[derive(Default)]
struct Asked {
    turns: usize,
    summaries: usize,
}

/// A model that answers every turn `ok` and refuses every summary. Its usage line reports one token
/// per four bytes of request, so the daemon's own accounting sees a conversation that grows.
fn a_model() -> (String, Arc<Mutex<Asked>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().expect("the port")
    );
    let asked = Arc::new(Mutex::new(Asked::default()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { return };
            let _ = answer(conn, &seen);
        }
    });
    (url, asked)
}

fn answer(mut conn: std::net::TcpStream, asked: &Mutex<Asked>) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    if body.contains(SUMMARY_MARKER) {
        asked.lock().unwrap().summaries += 1;
        return write!(
            conn,
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL}",
            REFUSAL.len()
        );
    }
    asked.lock().unwrap().turns += 1;
    let prompt_tokens = (body.len() / 4).max(1);
    let mut sse = String::new();
    for e in [
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"ok"}}]}"#.to_string(),
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        format!(
            r#"{{"choices":[],"usage":{{"prompt_tokens":{prompt_tokens},"completion_tokens":1}}}}"#
        ),
    ] {
        sse.push_str(&format!("data: {e}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{sse}",
        sse.len()
    )
}

/// A scratch root that removes itself, and the daemon it started.
struct Scratch {
    home: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let home = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&home);
        for d in [
            "ws",
            ".config/letibot",
            "landing",
            "logs",
            "run",
            "data",
            "state",
        ] {
            std::fs::create_dir_all(home.join(d)).expect("the scratch dir");
        }
        Self { home }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

struct Daemon(std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Daemon {
    /// Start the daemon under an address-space cap, so a reproduction cannot take this box down
    /// with it, and with its own store, socket and runtime dirs.
    fn start(scratch: &Scratch, url: &str, window: Option<u64>, cap: &str) -> Self {
        let home = &scratch.home;
        std::fs::write(
            home.join(".config/letibot/providers.toml"),
            format!("# Written by crates/harnessd/tests/r66_daemon_rss.rs\n[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"),
        )
        .expect("providers.toml");
        let stderr = std::fs::File::create(home.join("daemon.log")).expect("the log");
        let mut args: Vec<String> = vec![
            "--provider".into(),
            "deepseek".into(),
            "--model".into(),
            "deepseek-flash".into(),
            "--role".into(),
            "leticode".into(),
            "--session".into(),
            "s-r66".into(),
            "--workspace".into(),
            home.join("ws").display().to_string(),
            "--store".into(),
            home.join("s.db").display().to_string(),
            "--socket".into(),
            home.join("d.sock").display().to_string(),
            // One attempt, so a refusal is a failed compaction at once rather than a ladder.
            "--http-retries".into(),
            "1".into(),
        ];
        if let Some(w) = window {
            args.push("--context-window".into());
            args.push(w.to_string());
        }
        let script = format!("ulimit -v {cap}; exec \"$0\" \"$@\"");
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg(env!("CARGO_BIN_EXE_harnessd"))
            .args(&args)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("LETIBOT_LANDING_ROOT", home.join("landing"))
            .env("LETIBOT_NOTES_REPO", home.join("notes"))
            .env("LETIBOT_LOGS_ROOT", home.join("logs"))
            // A line a second: the watchdog's own reading of the heap, beside the kernel's of rss.
            .env("LETIBOT_MEMWATCH_SECS", "1")
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("LETIBOT_VOCAB_GGUF")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(stderr))
            .spawn()
            .expect("harnessd starts");
        let socket = home.join("d.sock");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "the daemon never bound its socket; its log says:\n{}",
                std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Daemon(child)
    }
}

/// `VmRSS` of a pid, in bytes, and zero on a box with no `/proc` - every Mac. Resident memory is
/// the instrument here and never the assertion, so an unreadable one costs a printed number.
fn rss(pid: u32) -> u64 {
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return 0;
    };
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// The last summary line the daemon printed - its own count of the heap, the threads and the
/// per-session footprint. Not the `grew` line beside it, which carries no census. `None` until the
/// first tick.
fn last_memwatch(scratch: &Scratch) -> Option<String> {
    let log = std::fs::read_to_string(scratch.home.join("daemon.log")).ok()?;
    log.lines()
        .rfind(|l| l.starts_with("memwatch: rss "))
        .map(str::to_string)
}

fn mb(n: f64) -> String {
    format!("{:.1} MiB", n / (1024.0 * 1024.0))
}

/// One prompt of `bytes` bytes, tagged so a row can be found.
fn prompt(turn: usize, bytes: usize) -> String {
    let mut text = format!("r66 turn {turn}: ");
    if bytes > text.len() {
        text.push_str(&"x".repeat(bytes - text.len()));
    }
    text
}

/// Read frames until the turn this prompt started has ENDED - a finished or a failed turn - and
/// answer whether it reached the model. A turn that is refused for room publishes a failure and never
/// reaches a model, and a wait on the model alone would report the refusal as a hang.
fn wait_turn(frames: &Receiver<ServerFrame>, what: &str, scratch: &Scratch) -> bool {
    let end = Instant::now() + Duration::from_secs(180);
    let mut ran = false;
    loop {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; the daemon's last words:\n{}",
            log_tail(scratch)
        );
        match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(ServerFrame::Event(env)) => match &env.event {
                SessionEvent::TurnFinished { .. } => return true,
                SessionEvent::TurnFailed { .. } => return ran,
                SessionEvent::TurnStarted { .. } => ran = true,
                _ => {}
            },
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the head's socket while waiting for {what}")
            }
        }
    }
}

/// Wait for the compaction until the model has been asked `want` summaries, taking this head's frames as they come.
/// A head that stops reading blocks the daemon's writer (one thread reads and writes this socket), so polling anything else would stop the awaited command from being read.
fn wait_compaction(
    frames: &Receiver<ServerFrame>,
    asked: &Mutex<Asked>,
    want: usize,
    scratch: &Scratch,
) {
    let end = Instant::now() + Duration::from_secs(180);
    while asked.lock().unwrap().summaries < want {
        assert!(
            Instant::now() < end,
            "timed out waiting for the compaction; the daemon's last words:\n{}",
            log_tail(scratch)
        );
        match frames.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the head's socket while waiting for the compaction")
            }
        }
    }
}

/// This head's frames, on a thread of their own: the daemon writes them on the same connection it
/// reads this head's commands from, so somebody has to be reading whatever the test is waiting for.
fn pump_frames(
    mut reader: letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    frames: std::sync::mpsc::Sender<ServerFrame>,
) {
    while let Ok(frame) = reader.read::<ServerFrame>() {
        if frames.send(frame).is_err() {
            return;
        }
    }
}

/// The daemon's own account of itself, for a failure that would otherwise say only that a poll timed
/// out.
fn log_tail(scratch: &Scratch) -> String {
    let log = std::fs::read_to_string(scratch.home.join("daemon.log")).unwrap_or_default();
    log.lines()
        .filter(|l| !l.starts_with("memwatch:") && !l.trim().is_empty())
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The fake's `host:port`, so a test can prove the daemon is not being blamed for a socket nothing on
/// this box could reach.
fn authority(url: &str) -> String {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .expect("a http://host:port/ url")
        .to_string()
}

/// What the daemon's `memwatch` summary line says it holds. Every byte figure is a sum over all sessions and printed rounded, so it is only as exact as its grain. Resident memory is the kernel's and printed separately.
#[derive(Clone, Copy, Default, Debug)]
struct Held {
    harness: u64,
    /// The unit `harness` was printed in: the reading is worth `harness` give or take half of this.
    harness_grain: u64,
    log: u64,
    /// The unit `log` was printed in, as [`harness_grain`](Self::harness_grain) is for `harness`.
    log_grain: u64,
    view: usize,
    heap: u64,
    /// The sessions the sums above cover.
    sessions: usize,
}

impl Held {
    /// Read off a line of the watch; zeroes on a line that has not got the parts yet, which is every
    /// reading before the daemon's first tick.
    fn of(watch: &str) -> Self {
        let (harness, harness_grain) = after(watch, "harness ").and_then(printed).unwrap_or((0, 0));
        let (log, log_grain) = after(watch, "logs ").and_then(printed).unwrap_or((0, 0));
        Held {
            harness,
            harness_grain,
            log,
            log_grain,
            view: after(watch, "view ")
                .and_then(|n| n.split_whitespace().next()?.parse().ok())
                .unwrap_or(0),
            heap: after(watch, "heap live ").map(bytes).unwrap_or(0),
            sessions: after(watch, "threads · ")
                .and_then(|n| n.split_whitespace().next()?.parse().ok())
                .unwrap_or(0),
        }
    }
}

/// The text after `marker`, or `None` when the line does not carry it.
fn after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.split_once(marker).map(|(_, rest)| rest)
}

/// `<n> KiB` / `<n> MiB` / `<n> B`, as `human_bytes` writes it, in bytes.
fn bytes(rest: &str) -> u64 {
    printed(rest).map(|(b, _)| b).unwrap_or(0)
}

/// A byte figure off the watch, and the grain the print rounded it to: `human_bytes` writes whole
/// units and one decimal above a gibibyte, so a figure carries half a grain of doubt either way.
fn printed(rest: &str) -> Option<(u64, u64)> {
    let mut it = rest.split_whitespace();
    let num = it.next()?;
    let n: f64 = num.parse().ok()?;
    let unit = match it.next().unwrap_or("B") {
        "GiB" => 1u64 << 30,
        "MiB" => 1 << 20,
        "KiB" => 1 << 10,
        _ => 1,
    };
    let grain = if num.contains('.') { unit / 10 } else { unit };
    Some(((n * unit as f64) as u64, grain))
}

/// One turn's reading.
#[derive(Clone)]
struct Row {
    turn: usize,
    /// Whether the turn reached the model. A session with no room refuses the turn and stays alive.
    ran: bool,
    rss: u64,
    /// What the daemon's own census says this session holds.
    held: Held,
    /// The daemon's own `memwatch` line, which carries heap live and the whole census.
    watch: String,
}

/// A scratch daemon and what it held after every turn.
struct Run {
    /// Alive for as long as the readings are, and killed by its own `Drop`.
    _daemon: Daemon,
    scratch: Scratch,
    pid: u32,
    rows: Vec<Row>,
}

/// Drive one session for `turns` prompts, asking for a compaction after each one and waiting for the
/// model to be asked both times. Every compaction is refused, which is the state the killed session
/// was in: nothing folds, and the instruction each attempt appends stays in the transcript.
fn drive(tag: &str, turns: usize, prompt_bytes: usize, window: Option<u64>, cap: &str) -> Run {
    let (url, asked) = a_model();
    // Prove the fake is reachable from here before anything the daemon does is called a defect.
    let addr: std::net::SocketAddr = authority(&url)
        .parse()
        .unwrap_or_else(|e| panic!("{} is not a socket address: {e}", authority(&url)));
    std::net::TcpStream::connect(addr).expect("the fake model listens");
    let scratch = Scratch::new(tag);
    let daemon = Daemon::start(&scratch, &url, window, cap);
    let pid = daemon.0.id();
    let (mut head, _hello, reader) = HeadClient::attach(
        scratch.home.join("d.sock"),
        "s-r66",
        0,
        "tui",
        tag,
        Caps {
            can_decide: true,
            ..Caps::default()
        },
    )
    .expect("attach");
    let (train, frames) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump_frames(reader, train));

    let mut rows: Vec<Row> = Vec::new();
    for turn in 0..turns {
        head.prompt(0, &prompt(turn, prompt_bytes))
            .expect("the prompt reaches the daemon");
        let ran = wait_turn(&frames, "the turn", &scratch);
        let want = asked.lock().unwrap().summaries + 1;
        head.compact(0).expect("the compaction reaches the daemon");
        wait_compaction(&frames, &asked, want, &scratch);
        // The failure path publishes its warning after the refusal, and that row is part of the turn.
        std::thread::sleep(Duration::from_millis(200));
        let watch = last_memwatch(&scratch).unwrap_or_default();
        rows.push(Row {
            turn,
            ran,
            rss: rss(pid),
            held: Held::of(&watch),
            watch,
        });
    }
    Run {
        _daemon: daemon,
        scratch,
        pid,
        rows,
    }
}

/// The instrument at its own scale: one session, `R66_TURNS` prompts of `R66_PROMPT_BYTES`, a
/// refused compaction after each, then `R66_IDLE_SECS` of idling. Run by hand; the numbers are the
/// answer, and the test below is the assertion.
#[test]
#[ignore = "a scratch daemon and its own clock: run it by hand with R66_TURNS set"]
fn the_daemon_probe() {
    let turns = env_usize("R66_TURNS", 200);
    let prompt_bytes = env_usize("R66_PROMPT_BYTES", 16 << 10);
    let idle = env_usize("R66_IDLE_SECS", 0);
    let window = std::env::var("R66_WINDOW")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let cap = std::env::var("R66_CAP").unwrap_or_else(|_| "6G".into());

    let run = drive("r66-daemon-probe", turns, prompt_bytes, window, &cap);
    let pid = run.pid;
    println!(
        "\nprobe: pid {pid}, {turns} turns of {prompt_bytes} bytes, a refused compaction after each, \
         window {window:?}, cap {cap}"
    );
    for row in &run.rows {
        if row.turn % 20 == 19 || row.turn + 1 == turns || !row.ran {
            println!(
                "turn {:>4} · ran {} · rss {} · {}",
                row.turn + 1,
                row.ran,
                mb(row.rss as f64),
                row.watch
            );
        }
    }

    if idle > 0 {
        let start = rss(pid);
        println!("\nidle for {idle}s from rss {}", mb(start as f64));
        for i in 0..idle {
            std::thread::sleep(Duration::from_secs(1));
            if (i + 1) % 10 == 0 || i + 1 == idle {
                println!("  idle {:>4}s · rss {}", i + 1, mb(rss(pid) as f64));
            }
        }
    }

    let first = run.rows.iter().find(|r| r.rss > 0);
    let last = run.rows.iter().rfind(|r| r.rss > 0);
    if let (Some(a), Some(b)) = (first, last) {
        let per_turn = (b.rss as f64 - a.rss as f64) / (b.turn - a.turn).max(1) as f64;
        println!(
            "\nrss {} → {} over {} turns · {} per turn · {} of prompts appended",
            mb(a.rss as f64),
            mb(b.rss as f64),
            b.turn - a.turn,
            mb(per_turn),
            mb((turns * prompt_bytes) as f64)
        );
    }
    println!(
        "\nthe daemon's own memory lines:\n{}",
        run.rows.last().map(|r| r.watch.clone()).unwrap_or_default()
    );
    println!("\nthe daemon's log tail:\n{}", log_tail(&run.scratch));
}

/// The assertion: many turns with a refused compaction after each, and what the daemon holds stays within what the rows cost.
/// The numbers are the daemon's own census (rows, region, log, view), since resident memory drifts by megabytes between runs. A term growing with the turn count rather than the rows fails here.
/// The region is 4 bytes a token and a byte vocabulary spends a token a byte, so a session's footprint is about five times its text.
#[test]
fn forty_turns_with_a_failed_compaction_each_keep_the_daemon_bounded() {
    let turns = 40usize;
    let prompt_bytes = 8 << 10;
    // What one refused attempt adds besides the prompt: the instruction, and a boundary or two.
    let instruction = 1_500usize;
    let run = drive("r66-daemon-bounded", turns, prompt_bytes, None, "6G");
    // The watch ticks once a second, so early turns may read no line. The baseline is the first reading with a census, and the slope is measured from it, since first-turn costs (hub bounds filling, store's first write) are constants a slope would misread.
    let readings: Vec<&Row> = run.rows.iter().filter(|r| r.held.harness > 0).collect();
    assert!(
        readings.len() >= turns - 4,
        "the daemon must say what it holds on almost every reading, not {} of {turns}",
        readings.len()
    );
    let first = readings.first().expect("a reading with a census");
    let last = readings.last().expect("a reading with a census");
    let spanned = last.turn - first.turn;
    let refused = run.rows.iter().filter(|r| !r.ran).count();
    // Resident memory is printed and never asserted: it is the allocator's and the kernel's opinion,
    // and on a box with no /proc there is no VmRSS to read at all.
    let resident = if last.rss == 0 {
        "rss unreadable here (no /proc): the census below is the assertion".to_string()
    } else {
        format!(
            "rss {} → {} (the instrument, not the assertion)",
            mb(first.rss as f64),
            mb(last.rss as f64)
        )
    };
    println!(
        "\n{} turns of {prompt_bytes} bytes with a failed compaction each · {refused} turn(s) refused \
         for room\n  {resident}\n  harness {} → {} · view {} → {} rows · log {} → {} · heap {} → {}",
        spanned,
        kb(first.held.harness),
        kb(last.held.harness),
        first.held.view,
        last.held.view,
        kb(first.held.log),
        kb(last.held.log),
        kb(first.held.heap),
        kb(last.held.heap),
    );

    // The arithmetic of a row, in the census's own units: the text, plus the token region's 4 bytes a
    // token where a byte vocabulary spends a token a byte, and the region's capacity doubles - so a
    // row can cost eight times its text in the turn the region doubles, and ten is the allowance.
    let rows_added = (spanned * (prompt_bytes + instruction)) as u64;
    // The line sums every session the daemon serves, so the bound is one session's rows times the
    // sessions the sum covers; and each figure is printed rounded, so both grains are spent too.
    let sessions = last.held.sessions.max(first.held.sessions).max(1) as u64;
    let allowed =
        rows_added * 10 * sessions + (first.held.harness_grain + last.held.harness_grain) / 2;
    let grew = last.held.harness.saturating_sub(first.held.harness);
    assert!(
        grew <= allowed,
        "{spanned} turns appending {} left the daemon holding {} more across {sessions} session(s) \
         — past the {} its own rows and region cost, which is a term nobody has accounted for",
        mb(rows_added as f64),
        mb(grew as f64),
        mb(allowed as f64)
    );

    // Printed, not asserted: the per-turn slope is a sawtooth while the region doubles. A conversation copy per turn is what the total rejects; this line shows a person which it was.
    let mid = readings[readings.len() / 2];
    let early = (mid.held.harness.saturating_sub(first.held.harness)) as f64
        / (mid.turn - first.turn).max(1) as f64;
    let late = (last.held.harness.saturating_sub(mid.held.harness)) as f64
        / (last.turn - mid.turn).max(1) as f64;
    println!(
        "  per turn: {} early · {} late (a doubling of the region lands in one of them)",
        kb(early as u64),
        kb(late as u64)
    );

    // The view is a LEVEL: a refused turn announces its prompt, its reply and its instruction, so it
    // cannot pass three a turn, and a reading up to a tick old can only read it lower. Diffing two
    // readings needed both to land on a turn boundary, which a one-second tick does not.
    assert!(
        last.held.view <= 3 * (last.turn + 1),
        "the view holds {} row(s) after {} turn(s), which is more than the three a turn makes",
        last.held.view,
        last.turn + 1
    );
    // The log is a ring, and forty turns of this size cannot reach its bound - so what is asserted is
    // its growth against the bytes the run published, which a log that kept a copy of the
    // conversation per turn would exceed by the turn count.
    let published = rows_added * sessions;
    let log_grew = last.held.log.saturating_sub(first.held.log);
    let log_allowed = published + (first.held.log_grain + last.held.log_grain) / 2;
    assert!(
        log_grew <= log_allowed,
        "the hub's log grew {} over {spanned} turns that published {}, which is a log keeping more \
         than the events it was given",
        kb(log_grew),
        kb(published)
    );
}

fn kb(n: u64) -> String {
    format!("{} KiB", n / 1024)
}
