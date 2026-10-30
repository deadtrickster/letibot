//! **`leticode_mode` against a real daemon** — the point that needs nobody, and the point that
//! needs a person.
//!
//! # The rule this file exists to pin
//!
//! `allow-all` on a box with no structural boundary is a person's answer about their own
//! machine, and the Claude Code bridge is a program. B8's tool therefore does not SET that
//! point: it asks the daemon, which puts the same card a terminal shows for `/mode allow-all`
//! in front of the session's OTHER heads — never the one that asked — and the point moves only
//! when one of them says yes. **The bridge has no frame in it that consents to anything.**
//!
//! Four facts, in the order a caller meets them:
//!
//! | fact | how it is measured |
//! |---|---|
//! | a point that needs nobody is applied at once | `read-only` lands, the answer says `nobody` was asked, and the session's own settings row says so |
//! | `allow-all` with no other head refuses, and changes NOTHING | the tool's error names the command that opens a head; the settings row is where it was |
//! | `allow-all` with a person at a head applies on their yes | a real head — a `tui` on the same socket — is shown the card and answers it; the point in force afterwards is the CONSENTED one |
//! | their no applies nothing | the same card, answered `no`: the session stays where it was |
//!
//! # Why the settings row, and not the tool's own answer
//!
//! A tool that reported what it had done would be asserting its own work. The mode is a fact the
//! DAEMON holds and publishes (`Config::settings`, the `mode` row a head's `/config` draws), so
//! every *did it change* here is read back from the daemon through a second, watching head —
//! `can_decide: false`, so taking the reading never becomes the head that answers a card this
//! file is about.
//!
//! **One test, because the environment is process-wide** — `$LETIBOT_SOCKET` and the store are
//! what the bridge and the daemon read, and a second `#[test]` here would race the first. This
//! file is a test binary of its own for that reason.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use serde_json::json;

/// What a session opens at, and what it must still be at when a confirmation fails.
const OPENING_MODE: &str = "writes allowed";
/// The consented spelling the daemon puts a bare host at when a person says yes.
const CONSENTED: &str = "allow-all (this box, consented)";

/// Generous on purpose: every wait here is for something a correct build produces in
/// milliseconds, and the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(60);

/// A directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The daemon, and the worktree it serves.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    socket: PathBuf,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    /// **A daemon with no model and no fake provider**, deliberately: every fact this file is
    /// about is the gate's, and a session whose turns go nowhere still has a point. `bridge.start`
    /// sends a brief, so the first turn fails with *this session has no model* — a row on the
    /// session's log and nothing this file asserts about.
    fn start() -> World {
        let dir = TempDir::new("mode-consent");
        let root = dir.path();
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: this test binary runs one test, and the variables are set before the
        // daemon, any thread that reads config, or the bridge exists.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
            std::env::remove_var("LETIBOT_SOCKET");
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        // An empty providers file: nothing of the operator's may decide what answers.
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            "# Written by crates/bridge/tests/mode_consent.rs — no providers.\n",
        )
        .expect("providers.toml");
        std::fs::write(
            root.join("config/letibot/hosts.toml"),
            "# Written by crates/bridge/tests/mode_consent.rs — no hosts.\n",
        )
        .expect("hosts.toml");
        unsafe { std::env::set_var("LETIBOT_HOSTS", root.join("config/letibot/hosts.toml")) };

        let socket = root.join("daemon.sock");
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        // **`leticode` is the unconfined seat**, which is the whole premise: an unconfined
        // session is one where `allow-all` requires a confinement it has not got, so the point
        // stands on a person's answer or not at all.
        cfg.seat = Seat::Leticode;
        // **No exec backend**: a delegated cgroup is apparatus this box may not have, and
        // nothing in this file runs a command. The writable backend is enough for a turn that
        // only talks and for a session that only needs a point.
        cfg.allow_bash = false;
        // The byte vocabulary and no provider: this session never takes a turn, and it does not
        // need one to have an approval point.
        cfg.vocab_gguf = None;
        cfg.provider = None;
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.http_retries = 0;
        // **A window the prompt fits inside.** The byte vocabulary counts every byte of the
        // system prompt and the tool schemas as a token, so a session's message zero is ~54k by
        // that measure; a 32k window made the harness try to compact before every turn, which is
        // a failed compaction per turn and nothing this file is about.
        cfg.context_window = Some(200_000);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "mode-daemon-session".into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a session with a byte vocabulary and no model"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let session = cfg.session_id.clone();
        let registry = Registry::new();
        registry
            .create(session, "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        letibot_sessionlog::placement::write(&socket, "host").expect("the record");
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            let mut sessions = Sessions::open_first(parts, cfg, worker_registry)
                .expect("the daemon opens its first session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
        });

        World {
            dir,
            worktree,
            socket,
            registry,
            worker: Some(worker),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        let _ = &self.dir;
    }
}

// ---------------------------------------------------------------------------
// A head that behaves like a person's terminal
// ---------------------------------------------------------------------------

/// **A person at the session**: a real head on the real socket, watching the frames the daemon
/// sends it and answering the one this file is about.
///
/// Not a mock — this is the *tui* kind, with `Caps::default()` and therefore `can_decide: true`,
/// which is exactly what makes the daemon willing to put the card in front of it and to accept
/// its answer. What it does with a card is what the terminal does: `y` → `ModeConsent { yes }`,
/// anything else → `ModeConsent { no }`.
struct Person {
    client: Arc<Mutex<HeadClient>>,
    frames: mpsc::Receiver<Inbound>,
}

impl Person {
    fn attach(socket: &Path, session: &str) -> Person {
        let (client, hello, reader) =
            HeadClient::attach(socket, session, 0, "tui", "dead@lab2x1", Caps::default())
                .expect("a person's head attaches");
        assert!(
            matches!(hello, ServerFrame::Hello { .. }),
            "the daemon greets a head: {hello:?}"
        );
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || pump(reader, tx));
        Person {
            client: Arc::new(Mutex::new(client)),
            frames,
        }
    }

    /// **Wait for the confirmation card and answer it**, on this thread. Returns the request's
    /// id and the point it was about — what a reader needs to say the card really arrived.
    ///
    /// Everything else the daemon sends is skipped: the head is attached to a live session, so
    /// events and notices arrive alongside, and a reader that treated them as the answer would
    /// be waiting for the wrong frame.
    fn answer_the_card(&self, yes: bool, within: Duration) -> (String, String) {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no confirmation card arrived within {within:?}"
            );
            match self.frames.recv_timeout(left) {
                Ok(Inbound::Frame(ServerFrame::ModeConfirm { req_id, name, .. })) => {
                    let mut c = self.client.lock().unwrap_or_else(|e| e.into_inner());
                    c.mode_consent(0, &req_id, yes)
                        .expect("the answer goes out on this head's own socket");
                    return (req_id, name);
                }
                Ok(_) => continue,
                Err(e) => panic!("the person's head heard nothing more: {e}"),
            }
        }
    }
}

/// **What the daemon says this session's point is**, read through a watching head.
///
/// `can_decide: false` on purpose: a probe that could decide would BE the human head the
/// refusals in this file are about.
fn mode_of(socket: &Path, session: &str) -> String {
    let (mut client, _hello, reader) = HeadClient::attach(
        socket,
        session,
        0,
        "probe",
        "settings-probe",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
    )
    .expect("a watching head attaches");
    let (tx, frames) = mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));
    let deadline = Instant::now() + PATIENCE;
    let mode = loop {
        client.settings().expect("the settings are asked for");
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the daemon never answered `Settings` with a `mode` row"
        );
        match frames.recv_timeout(left) {
            // **Retried, because a session is REGISTERED before it is open.** The settings are
            // published when the worker opens the harness, so a probe that asked in the window
            // before that gets an honest empty list; asking again is what a person's pane does.
            Ok(Inbound::Frame(ServerFrame::Settings { rows, .. })) => {
                if let Some(row) = rows.into_iter().find(|r| r.key == "mode") {
                    break row.value;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(_) => continue,
            Err(e) => panic!("the probe heard nothing more: {e}"),
        }
    };
    let _ = client.detach();
    mode
}

/// **The mode once it has settled on `want`, or the last reading when the patience runs out.**
///
/// A `Mode` frame is queued like a prompt and takes effect from the session's next call, so the
/// first `Settings` a probe gets can still carry the point the session opened at — MEASURED in
/// the merge gate on a loaded lab2x1 (load 26): `always-ask` where `writes allowed` was coming.
/// Asking again until the answer is the one expected cannot hide a wrong move: a point that went
/// somewhere else never becomes `want`, and its reading is what the assertion then reports.
fn mode_reaches(socket: &Path, session: &str, want: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let now = mode_of(socket, session);
        if now == want || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_mode_that_needs_nobody_applies_and_allow_all_waits_for_a_person() {
    let world = World::start();
    let bridge = Bridge::new();
    let worktree = world.worktree.display().to_string();
    let start = |brief: &str| {
        bridge
            .start(&worktree, brief, None, "writes-allowed", None)
            .expect("the bridge starts a session")["session"]
            .as_str()
            .expect("a session id")
            .to_string()
    };

    // --- a point that needs nobody is applied at once -------------------------
    let plain = start("mode-marker: the point nobody has to confirm");
    assert_eq!(
        mode_reaches(&world.socket, &plain, OPENING_MODE),
        OPENING_MODE,
        "a session opens at the point it was started with"
    );
    let moved = bridge
        .set_mode(&plain, "read-only")
        .expect("a point that needs nobody is applied");
    assert_eq!(moved["applied"], json!(true), "{moved}");
    assert_eq!(moved["answer"], json!("nobody"), "{moved}");
    assert_eq!(moved["mode"], json!("read-only"), "{moved}");
    assert_eq!(
        mode_reaches(&world.socket, &plain, "read-only"),
        "read-only",
        "the daemon's own row says the point moved: {moved}"
    );

    // --- `allow-all` with nobody to ask refuses, and changes nothing -----------
    let alone = start("mode-marker: nobody is watching this one");
    let refused = bridge
        .set_mode(&alone, "allow-all")
        .expect_err("there is no head to ask, so nothing was opened");
    assert!(
        refused.contains("needs the operator's confirmation in a head"),
        "the refusal sends the caller to the command that opens one: {refused}"
    );
    assert!(
        refused.contains(&format!("leticode --session {alone}")),
        "and names the session: {refused}"
    );
    assert_eq!(
        mode_reaches(&world.socket, &alone, OPENING_MODE),
        OPENING_MODE,
        "a refused request changed nothing"
    );

    // --- a person at a head says yes, and the point moves ---------------------
    let watched = start("mode-marker: a person is at this one");
    // The ask BLOCKS until the answer settles it, so the person answers from its own thread —
    // which is also the only shape the daemon allows: one head asks, another answers.
    //
    // **The ask waits for the person to be ATTACHED, not for a clock.** A daemon with no head
    // that may decide answers `no head that may decide is attached … besides the one that
    // asked`, so a spawn followed straight by `set_mode` is a race — and on a busy box the ask
    // wins it: MEASURED 2026-10-30 on lab2x1, sixteen of these binaries in parallel, **48 of 48
    // runs failed at `a person confirmed it`** and 0 of 25 alone. `Person::attach` returns once
    // the daemon has greeted the head, and the daemon attaches the seat BEFORE it writes the
    // greeting (`serve_conn`'s `seat_in`), so that greeting is the fact to wait for.
    let socket = world.socket.clone();
    let (attached, is_attached) = mpsc::channel();
    let answering = {
        let watched = watched.clone();
        std::thread::spawn(move || {
            let person = Person::attach(&socket, &watched);
            attached
                .send(())
                .expect("the test is still waiting for the person's head");
            person.answer_the_card(true, PATIENCE)
        })
    };
    is_attached
        .recv_timeout(PATIENCE)
        .expect("the person's head is attached before the ask");
    let yes = bridge
        .set_mode(&watched, "allow-all")
        .expect("a person confirmed it");
    let (req_id, asked) = answering.join().expect("the person's thread");
    assert!(!req_id.is_empty(), "the card carried a request id");
    assert_eq!(asked, "allow-all", "and named the point it was about");
    assert_eq!(yes["applied"], json!(true), "{yes}");
    assert_eq!(yes["answer"], json!("yes"), "{yes}");
    assert_eq!(
        yes["mode"],
        json!(CONSENTED),
        "the point in force is the CONSENTED one, not the name that was asked for: {yes}"
    );
    assert!(
        yes["said"]
            .as_str()
            .unwrap_or_default()
            .contains("confirmation"),
        "and the daemon's own sentence says where it stands: {yes}"
    );
    assert_eq!(
        mode_reaches(&world.socket, &watched, CONSENTED),
        CONSENTED,
        "the daemon's row agrees with the answer: {yes}"
    );

    // --- and their NO applies nothing ----------------------------------------
    let declined = start("mode-marker: a person says no here");
    let socket = world.socket.clone();
    let (attached, is_attached) = mpsc::channel();
    let answering = {
        let declined = declined.clone();
        std::thread::spawn(move || {
            let person = Person::attach(&socket, &declined);
            attached
                .send(())
                .expect("the test is still waiting for the person's head");
            person.answer_the_card(false, PATIENCE)
        })
    };
    // The same fact as the yes above: a head that has not attached is not a head the daemon
    // will ask, and a `no` it never asked for is a refusal this test would read as the answer.
    is_attached
        .recv_timeout(PATIENCE)
        .expect("the person's head is attached before the ask");
    let no = bridge
        .set_mode(&declined, "allow-all")
        .expect("a no is an answer, not a failure");
    let (req_id, _) = answering.join().expect("the person's thread");
    assert!(!req_id.is_empty());
    assert_eq!(no["applied"], json!(false), "{no}");
    assert_eq!(no["answer"], json!("no"), "{no}");
    assert!(
        no["said"]
            .as_str()
            .unwrap_or_default()
            .contains("nothing changed"),
        "{no}"
    );
    assert_eq!(
        mode_reaches(&world.socket, &declined, OPENING_MODE),
        OPENING_MODE,
        "a person's no leaves the session where it was: {no}"
    );

    // **The bridge never consents**, and this is the shape of that promise: the tool has no
    // argument for consent, so the whole vocabulary is the point's name.
    let schema = tools::list()["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == "leticode_mode")
        .cloned()
        .expect("leticode_mode is published");
    let mut props: Vec<&str> = schema["inputSchema"]["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .map(String::as_str)
        .collect();
    props.sort_unstable();
    assert_eq!(
        props,
        vec!["mode", "session"],
        "the whole vocabulary of the ask is the point's name — there is no argument for consent: {schema}"
    );

    // And a session that is only watched cannot be asked about at all.
    let watcher = Bridge::new();
    watcher
        .attach(&worktree, &plain, true)
        .expect("a watching head attaches");
    let refused = watcher
        .set_mode(&plain, "read-only")
        .expect_err("a watcher does not move a point");
    assert!(refused.contains("watched, not held"), "{refused}");

    // A name the daemon does not know is refused by name and settles nothing.
    let refused = bridge
        .set_mode(&plain, "not-a-point")
        .expect_err("an unknown point is refused");
    assert!(refused.contains("not-a-point"), "{refused}");
    assert_eq!(
        mode_reaches(&world.socket, &plain, "read-only"),
        "read-only"
    );
}
