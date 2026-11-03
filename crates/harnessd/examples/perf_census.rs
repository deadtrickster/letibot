//! PERF: one daemon holding N sessions, each streaming a fake turn that makes a tool call - and
//! the five numbers a hotspot has to be found by, not guessed at.
//!
//! ```sh
//! cargo run --offline --release -p letibot-harnessd --example perf_census -- [sessions,..] [subs]
//! ```
//!
//! The default is `1,10,50` sessions and one socket subscriber each, which is the shape of the
//! question: what does a daemon cost when many sessions stream at once. Everything runs in ONE
//! process - the daemon really is in-process (`Daemon::serve` plus `Sessions`), the subscribers are
//! real `HeadClient`s on a real unix socket - so "process" below means the daemon and the bench
//! together, and a subscriber's own decode is part of what a subscriber costs. No network: the
//! model is a loopback stub answering the frames `letibot-provider` parses.
//!
//! The numbers, and how each is taken:
//!
//! * **RSS** - `/proc/self/status`: after every session opened, and the peak at the end, so a
//!   per-session cost is read off the difference between the runs.
//! * **CPU** - `getrusage(RUSAGE_SELF)` over the streaming phase only, and again per event.
//! * **emit-to-subscriber latency** - the bench publishes a probe warning on a session's own hub
//!   and times it to the moment a subscriber has the frame. That is the whole real path: the
//!   fan-out clone, the head's queue, the pump's JSON, the socket, the client's parse.
//! * **socket fan-out per subscriber** - one session, K probes, with S subscribers attached (1, 2,
//!   4, 8): the fit of CPU per event against S is what one more subscriber costs.
//! * **store write volume** - the store file and its WAL, before and after, per session and per
//!   event.
//!
//! `subs` may be 0, which is the control: the same turns with nobody listening, so the daemon's own
//! work can be told apart from the fan-out's.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::{Hub, Owner};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::wire::FrameReader;

/// The warning code a probe carries, so a subscriber can tell a measurement from a real warning.
const PROBE: &str = "perf-probe";
/// How many text chunks the stub streams before it calls a tool. A turn that answers in one chunk
/// measures nothing about a stream: the cost is per event, and a stream is many.
const CHUNKS: usize = 32;
/// Chat requests one session's turn makes: the round that calls the tool, and the round that
/// answers after the tool's result. The stub counts them, so "the turn is done" is not a guess.
const ROUNDS_PER_TURN: u64 = 2;
/// The file the tool call reads, written into the workspace before any session opens.
const READ_TARGET: &str = "notes.txt";
/// How long anything here may take. Nothing is a performance claim - a probe is released by the
/// bench - so this is only a bound on a hang.
const PATIENCE: Duration = Duration::from_secs(300);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sizes: Vec<usize> = args
        .first()
        .map(|s| s.split(',').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 10, 50]);
    let subs: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

    println!(
        "perf_census: profile {} · {CHUNKS} chunks per streamed turn · {subs} socket \
         subscriber(s) per session · one tool call per turn",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    let mut rows = Vec::new();
    for n in sizes {
        match run(n, subs) {
            Ok(row) => {
                row.print();
                rows.push(row);
            }
            Err(why) => println!("{n:>6}  failed: {why}"),
        }
    }
    if rows.len() >= 2 {
        let (first, last) = (rows.first().unwrap(), rows.last().unwrap());
        let more = (last.sessions - first.sessions) as f64;
        if more > 0.0 {
            println!(
                "\nmarginal per session: rss {:.0} KiB · cpu {:.1} ms/turn · store {:.1} KiB/turn \
                 · {:.1} events/turn",
                (last.rss_open_kb as f64 - first.rss_open_kb as f64) / more,
                (last.cpu_ms - first.cpu_ms) / more,
                (last.store_after as f64
                    - last.store_before as f64
                    - (first.store_after as f64 - first.store_before as f64))
                    / more
                    / 1024.0,
                (last.events as f64 - first.events as f64) / more
            );
        }
    }
}

/// One run's answers.
struct Row {
    sessions: usize,
    rss_open_kb: u64,
    rss_peak_kb: u64,
    cpu_ms: f64,
    events: u64,
    p50: u64,
    p99: u64,
    max: u64,
    /// `(subscribers, µs CPU per event, µs wall per event)`, from the sweep.
    fanout: Vec<(usize, f64, f64)>,
    store_before: u64,
    store_after: u64,
}

impl Row {
    fn print(&self) {
        println!(
            "\n{:>6} {:>10} {:>10} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8}",
            "sess", "rss-open", "rss-peak", "cpu-ms", "events", "µs/event", "p50-µs", "p99-µs",
            "max-µs"
        );
        println!(
            "{:>6} {:>10} {:>10} {:>9.1} {:>8} {:>9.1} {:>8} {:>8} {:>8}",
            self.sessions,
            self.rss_open_kb,
            self.rss_peak_kb,
            self.cpu_ms,
            self.events,
            self.cpu_ms * 1000.0 / self.events.max(1) as f64,
            self.p50,
            self.p99,
            self.max
        );
        let wrote = self.store_after.saturating_sub(self.store_before);
        println!(
            "       store {:.0} KiB per session-turn ({} -> {} bytes, {} turn(s))",
            wrote as f64 / 1024.0 / self.sessions as f64,
            self.store_before,
            self.store_after,
            self.sessions
        );
        for (subs, cpu, wall) in &self.fanout {
            println!(
                "       fan-out, {subs} subscriber(s): {cpu:>7.2} µs CPU/event  {wall:>7.1} µs \
                 wall/event"
            );
        }
        if let Some((a, b)) = fit(&self.fanout) {
            println!("       fan-out fit: {a:.2} µs/event + {b:.2} µs per extra subscriber");
        }
    }
}

/// Least squares over `(subscribers, µs per event)`: the intercept is one event's base cost and the
/// slope is what one more subscriber adds to it.
fn fit(points: &[(usize, f64, f64)]) -> Option<(f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let sx: f64 = points.iter().map(|(s, _, _)| *s as f64).sum();
    let sy: f64 = points.iter().map(|(_, c, _)| *c).sum();
    let sxx: f64 = points.iter().map(|(s, _, _)| (*s as f64) * (*s as f64)).sum();
    let sxy: f64 = points.iter().map(|(s, c, _)| (*s as f64) * *c).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    Some(((sy - slope * sx) / n, slope))
}

/// What every subscriber thread and the prober share.
#[derive(Default)]
struct Shared {
    /// Events seen by the counting subscriber (index 0) of each session.
    events: Mutex<HashMap<String, u64>>,
    /// Probe nonce -> the instant `publish` was called.
    emitted: Mutex<HashMap<u64, Instant>>,
    /// Arrival minus emit, in micros, for every probe any subscriber saw.
    latencies: Mutex<Vec<u64>>,
    /// Probe arrivals in total, which is how the sweep knows every frame landed.
    arrivals: AtomicU64,
}

impl Shared {
    fn session_events(&self, id: &str) -> u64 {
        *self.events.lock().unwrap().get(id).unwrap_or(&0)
    }

    fn bump(map: &Mutex<HashMap<String, u64>>, id: &str) {
        *map.lock().unwrap().entry(id.to_string()).or_default() += 1;
    }
}

fn run(n: usize, subs: usize) -> Result<Row, String> {
    let root = TempDir::new(&format!("perf-{n}-{subs}"));
    let root = root.path().to_path_buf();
    // Nothing of the operator's may reach these sessions: a key or a ruleset in their config would
    // decide what a request does, and this bench's whole point is that nothing leaves loopback and
    // no call waits for a person.
    std::fs::create_dir_all(root.join("config/letibot")).map_err(|e| e.to_string())?;
    // SAFETY: this binary sets these before any thread that reads config exists, and never again.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
        std::env::remove_var("LETIBOT_PERMISSION");
    }

    let fake = Fake::serve();
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/examples/perf_census.rs - a stub on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .map_err(|e| e.to_string())?;

    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    std::fs::write(
        workspace.join(READ_TARGET),
        "the file a streamed tool call reads\n",
    )
    .map_err(|e| e.to_string())?;
    let store_path = root.join("sessions.db");

    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = id_of(0);
    cfg.store = Some(store_path.clone());
    // The byte vocabulary and a provider, so this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts: &'static Parts = Box::leak(Box::new(
        Parts::load(&cfg).map_err(|e| format!("the byte vocabulary: {e:?}"))?,
    ));

    let registry = Registry::new();
    registry
        .create(id_of(0), "", Sessions::wiring(&cfg))
        .map_err(|e| format!("the daemon's own first session: {e:?}"))?;
    let socket = letibot_sessionlog::testing::socket_path(&root, "harnessd.sock");
    let daemon = Daemon::serve(registry.clone(), &socket).map_err(|e| e.to_string())?;

    let worker = {
        let cfg = cfg.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            let mut sessions =
                Sessions::open_first(parts, cfg, registry).expect("the first session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
            sessions.drain_subagents(Duration::from_secs(2)).sentence()
        })
    };

    for i in 1..n {
        registry
            .create(id_of(i), "", Sessions::wiring(&cfg))
            .map_err(|e| format!("session {i}: {e:?}"))?;
    }
    let ids: Vec<String> = (0..n).map(id_of).collect();
    wait_until("every session to be served by its own thread", || {
        ids.iter()
            .all(|id| registry.get(id).is_some_and(|h| h.owner() == Owner::OwnThread))
    });

    // The subscribers: real heads, on the real socket, each driving its own reader.
    let shared = Arc::new(Shared::default());
    let mut clients: Vec<HeadClient> = Vec::new();
    for id in &ids {
        for s in 0..subs {
            let (client, _hello, reader) = HeadClient::attach(
                &socket,
                id,
                0,
                "bench",
                &format!("bench-{s}"),
                Caps::default(),
            )
            .map_err(|e| format!("attach {id}: {e}"))?;
            clients.push(client);
            spawn_reader(reader, id.clone(), s == 0, shared.clone());
        }
    }

    let rss_open_kb = rss_kb();
    let store_before = store_bytes(&store_path);
    let cpu_before = cpu_ms();

    // The probes run WHILE the turns stream, because a latency measured on an idle daemon is not
    // the latency anyone waits on.
    let probing = Arc::new(AtomicBool::new(subs > 0));
    let prober = {
        let registry = registry.clone();
        let ids = ids.clone();
        let shared = shared.clone();
        let probing = probing.clone();
        std::thread::spawn(move || {
            let mut nonce = 0u64;
            while probing.load(Ordering::Relaxed) {
                for id in &ids {
                    if !probing.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(hub) = registry.get(id) else { continue };
                    nonce += 1;
                    probe(&hub, &shared, nonce);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        })
    };

    for (i, client) in clients.iter_mut().enumerate() {
        let session = &ids[i / subs.max(1)];
        client
            .prompt(0, &format!("stream a turn for {session}"))
            .map_err(|e| format!("prompt to {session}: {e}"))?;
    }
    // The stub is the witness that every turn reached the model, and the hub's turn state is the
    // witness that the work after the last round - the rows, the persist - is done.
    wait_until("every session's turn to reach the model twice", || {
        fake.chat_answers() >= ROUNDS_PER_TURN * n as u64
    });
    wait_until("every turn to end", || {
        ids.iter().all(|id| {
            registry
                .get(id)
                .is_some_and(|hub| !hub.turn_in_flight() && hub.status().items > 0)
        })
    });
    probing.store(false, Ordering::Relaxed);
    let _ = prober.join();

    let cpu_ms = cpu_ms() - cpu_before;
    let rss_peak_kb = hwm_kb();
    let store_after = store_bytes(&store_path);
    let events: u64 = ids.iter().map(|id| shared.session_events(id)).sum();

    let mut latencies = shared.latencies.lock().unwrap().clone();
    latencies.sort_unstable();
    let pct = |p: f64| -> u64 {
        if latencies.is_empty() {
            return 0;
        }
        latencies[((latencies.len() as f64 - 1.0) * p).round() as usize]
    };

    // The fan-out sweep, on one session, with the run's own subscribers still attached.
    let fanout = fanout_sweep(&socket, &registry, &ids[0], subs, shared.clone());

    registry.close();
    let _ = worker.join();
    drop(clients);
    std::fs::remove_dir_all(&root).ok();

    Ok(Row {
        sessions: n,
        rss_open_kb,
        rss_peak_kb,
        cpu_ms,
        events,
        p50: pct(0.50),
        p99: pct(0.99),
        max: latencies.last().copied().unwrap_or(0),
        fanout,
        store_before,
        store_after,
    })
}

/// Publish one probe and remember when it left, so the subscriber's arrival is a latency.
fn probe(hub: &Arc<Hub>, shared: &Arc<Shared>, nonce: u64) {
    shared
        .emitted
        .lock()
        .unwrap()
        .insert(nonce, Instant::now());
    hub.publish(SessionEvent::Warning {
        code: PROBE.into(),
        detail: nonce.to_string(),
        compaction: None,
    });
}

/// One subscriber's reader: count what arrives, and time the probes.
fn spawn_reader(
    mut reader: FrameReader<std::os::unix::net::UnixStream>,
    id: String,
    counting: bool,
    shared: Arc<Shared>,
) {
    std::thread::spawn(move || {
        loop {
            let Ok(frame) = reader.read::<ServerFrame>() else {
                return;
            };
            let ServerFrame::Event(env) = frame else {
                continue;
            };
            let at = Instant::now();
            if counting {
                Shared::bump(&shared.events, &id);
            }
            let SessionEvent::Warning { code, detail, .. } = &env.event else {
                continue;
            };
            if code != PROBE {
                continue;
            }
            let nonce: u64 = detail.parse().unwrap_or(0);
            let emitted = shared.emitted.lock().unwrap().get(&nonce).copied();
            if let Some(emitted) = emitted {
                let us = at.duration_since(emitted).as_micros() as u64;
                shared.latencies.lock().unwrap().push(us);
            }
            shared.arrivals.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// What one more subscriber costs: K probes to one session with S subscribers attached, three times
/// each, the cheapest taken - a probe costs what it costs at the instant nothing else is running.
fn fanout_sweep(
    socket: &Path,
    registry: &Arc<Registry>,
    id: &str,
    already: usize,
    shared: Arc<Shared>,
) -> Vec<(usize, f64, f64)> {
    const K: u64 = 400;
    const REPEATS: usize = 3;
    let Some(hub) = registry.get(id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut extra: Vec<HeadClient> = Vec::new();
    for step in 0..4usize {
        // A head count of 1, 2, 4 and 8 on ONE session: the first step is the run's own
        // subscriber, and each later step adds enough to reach the next count.
        let want = [0usize, 1, 3, 7][step].saturating_sub(already.saturating_sub(1));
        while extra.len() < want {
            let Ok((client, _hello, reader)) =
                HeadClient::attach(socket, id, 0, "bench", "fanout", Caps::default())
            else {
                return out;
            };
            extra.push(client);
            spawn_reader(reader, id.to_string(), false, shared.clone());
        }
        let subscribers = already + extra.len();
        if out.iter().any(|(s, _, _)| *s == subscribers) {
            continue;
        }
        // Settle: the attach itself publishes, and its cost is not fan-out.
        std::thread::sleep(Duration::from_millis(50));
        let mut best_cpu = f64::MAX;
        let mut best_wall = f64::MAX;
        for _ in 0..REPEATS {
            let arrivals_before = shared.arrivals.load(Ordering::Relaxed);
            let before = cpu_ms();
            let began = Instant::now();
            for k in 0..K {
                probe(&hub, &shared, 10_000_000 + (subscribers as u64) * 1000 + k);
            }
            let wall = began.elapsed();
            wait_until("every probe of the sweep to be delivered", || {
                shared.arrivals.load(Ordering::Relaxed) >= arrivals_before + K * subscribers as u64
            });
            best_cpu = best_cpu.min(cpu_ms() - before);
            best_wall = best_wall.min(wall.as_micros() as f64 / K as f64);
        }
        out.push((subscribers, best_cpu * 1000.0 / K as f64, best_wall));
    }
    out.sort_by_key(|(s, _, _)| *s);
    out
}

fn id_of(i: usize) -> String {
    format!("s-perf-{i}")
}

/// This process's resident set, in KiB: the daemon and the bench are one process here.
fn rss_kb() -> u64 {
    status_kb("VmRSS:")
}

fn hwm_kb() -> u64 {
    status_kb("VmHWM:")
}

fn status_kb(field: &str) -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    text.lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The store and its WAL, as the disk holds them.
fn store_bytes(path: &Path) -> u64 {
    let mut total = 0;
    for name in ["", "-wal"] {
        let p = PathBuf::from(format!("{}{name}", path.display()));
        total += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    }
    total
}

fn cpu_ms() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `getrusage` writes only into the struct just zeroed, for this process.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let user = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6;
    let sys = ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6;
    (user + sys) * 1000.0
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("waited {PATIENCE:?} for {what}");
}

/// A directory that removes itself, because a leaked store per run eventually fills the disk and
/// the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
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

/// A model on a loopback port: it streams `CHUNKS` text chunks and then calls a tool, and answers a
/// turn that comes back with a tool result in one chunk. One thread per connection, because two
/// sessions may be inside a request at once.
struct Fake {
    url: String,
    chat: Arc<AtomicU64>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().expect("the port").port()
        );
        let chat = Arc::new(AtomicU64::new(0));
        let counted = chat.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let counted = counted.clone();
                std::thread::spawn(move || {
                    let _ = answer(conn, &counted);
                });
            }
        });
        Fake { url, chat }
    }

    /// Chat requests served across every session.
    fn chat_answers(&self) -> u64 {
        self.chat.load(Ordering::Relaxed)
    }
}

fn answer(mut conn: TcpStream, chat: &AtomicU64) -> std::io::Result<()> {
    let body = read_request(&mut conn)?;
    // The window probe a session makes before its first turn is not a turn.
    if !body.contains("\"messages\"") {
        const PROPS: &str = "{\"default_generation_settings\":{\"n_ctx\":32768}}";
        return write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{PROPS}",
            PROPS.len()
        );
    }
    chat.fetch_add(1, Ordering::Relaxed);
    let after_tool = body.contains("\"role\":\"tool\"");
    let mut out = String::new();
    if after_tool {
        sse(
            &mut out,
            &serde_json::json!({"choices":[{"delta":{"content":"done"}}]}),
        );
        sse(
            &mut out,
            &serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        );
    } else {
        for i in 0..CHUNKS {
            sse(
                &mut out,
                &serde_json::json!({"choices":[{"delta":{"content":format!("chunk {i} ")}}]}),
            );
        }
        // The arguments arrive in two pieces, as a real provider streams them, so the harness's own
        // accumulation of a call is on the measured path.
        let head = "{\"path\":";
        let tail = format!("{READ_TARGET}\"}}");
        for (i, piece) in [(0, head), (1, tail.as_str())] {
            let call = if i == 0 {
                serde_json::json!({
                    "index":0,
                    "id":"call_perf",
                    "type":"function",
                    "function":{"name":"read","arguments":piece}
                })
            } else {
                serde_json::json!({"index":0,"function":{"arguments":piece}})
            };
            sse(
                &mut out,
                &serde_json::json!({"choices":[{"delta":{"tool_calls":[call]}}]}),
            );
        }
        sse(
            &mut out,
            &serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        );
    }
    sse(
        &mut out,
        &serde_json::json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }}),
    );
    out.push_str("data: [DONE]\n\n");
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{out}",
        out.len()
    )?;
    conn.flush()
}

fn sse(out: &mut String, chunk: &serde_json::Value) {
    out.push_str("data: ");
    out.push_str(&chunk.to_string());
    out.push_str("\n\n");
}

/// Drain one request off the socket: the headers, then the body `content-length` names.
fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    let mut head_end = None;
    let mut want = 0usize;
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        acc.extend_from_slice(&buf[..n]);
        if head_end.is_none()
            && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
        {
            head_end = Some(at);
            want = acc[..at]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line).to_ascii_lowercase();
                    let (name, value) = line.split_once(':')?;
                    (name.trim() == "content-length").then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
        }
        if let Some(at) = head_end
            && acc.len() - (at + 4) >= want
        {
            break;
        }
        if acc.len() > (1 << 24) {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc).into_owned();
    Ok(match head_end {
        Some(at) => text[at + 4..].to_string(),
        None => text,
    })
}
