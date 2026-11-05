//! Ctrl-C in a one-shot: the first one interrupts the turn, and the process goes in
//! seconds. The signal does reach the daemon - the launcher `exec`s it for a one-shot, so
//! it is in the terminal's foreground process group - and that leaves two defects 1.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use letibot_tokencore::store::Store;
use letibot_transcript::TranscriptItem;

/// How long the daemon may take to start, open the session and reach its first round.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long the first Ctrl-C may take to end the daemon. Well under the run's own two
/// minutes - that is the whole point - and above everything the daemon's own stop is
/// allowed to spend
const CTRL_C_BUDGET: Duration = Duration::from_secs(15);

/// The file the in-flight command writes as its first act. The signal has to land on a turn
/// whose command is *running*, and nothing the provider can see says that
const MARKER: &str = "the-command-started";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What the fake provider does with a request.
#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// Round one: a `bash` call that sleeps for longer than any bound here. Round two: prose, so a
    /// turn that was NOT interrupted finishes and the test fails on the clock rather than hanging.
    BashThenProse,
    /// Accept the connection, read the request, answer nothing - a provider that stopped
    /// sending, which is a turn blocked where no steering poll can reach it.
    Hang,
}

/// A fake provider on a loopback port, counting the rounds it has served.
struct Fake {
    url: String,
    rounds: Arc<AtomicUsize>,
    /// When the FIRST request had been read off the socket - the moment the daemon could begin to
    /// parse it. The gap from here to the command's marker is what the fixture prints.
    first_read: Arc<std::sync::OnceLock<Instant>>,
}

impl Fake {
    fn serve(script: Script) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let rounds = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&rounds);
        let first_read = Arc::new(std::sync::OnceLock::new());
        let read_at = Arc::clone(&first_read);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let _ = read_request(&conn);
                let n = seen.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    let _ = read_at.set(Instant::now());
                }
                if script == Script::Hang {
                    // Held open and never answered: the request the daemon is waiting on.
                    std::thread::sleep(Duration::from_secs(600));
                    continue;
                }
                let body = if n == 0 {
                    // The command announces itself before it blocks, and the `touch` is what the
                    // test waits for: `rounds` only says the provider has read the request.
                    let arguments =
                        serde_json::json!({ "command": format!("touch {MARKER} && sleep 120") })
                            .to_string();
                    frames([
                        serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                            "index": 0,
                            "id": "call_sigint_1",
                            "function": { "name": "bash", "arguments": arguments },
                        }]}}]}),
                        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
                    ])
                } else {
                    frames([
                        serde_json::json!({"choices":[{"delta":{"content":"done"}}]}),
                        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
                    ])
                };
                let _ = conn.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = conn.write_all(body.as_bytes());
                let _ = conn.flush();
            }
        });
        Fake {
            url,
            rounds,
            first_read,
        }
    }

    fn rounds(&self) -> usize {
        self.rounds.load(Ordering::SeqCst)
    }

    /// When the first request was read, if it has been.
    fn first_read(&self) -> Option<Instant> {
        self.first_read.get().copied()
    }
}

fn frames(items: [serde_json::Value; 2]) -> String {
    let mut body = String::new();
    for item in items {
        body.push_str(&format!("data: {item}\n\n"));
    }
    body.push_str(
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
    );
    body.push_str("data: [DONE]\n\n");
    body
}

/// Drain one HTTP request: headers, then the body `Content-Length` names.
fn read_request(stream: &std::net::TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut len = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).is_err() || head.trim_end().is_empty() {
            break;
        }
        if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// A one-shot daemon, with nothing of the operator's reachable from it.
struct OneShot {
    child: Child,
    log: PathBuf,
    store: PathBuf,
    session: String,
    /// The session's workspace, which is where the command runs and therefore where its marker
    /// lands.
    workspace: PathBuf,
}

impl OneShot {
    fn start(dir: &Path, fake: &Fake) -> OneShot {
        let home = dir.join("home");
        let config = dir.join("config/letibot");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::create_dir_all(home.join("logs")).expect("logs dir");
        // A key in the operator's `providers.toml` would decide where a request goes, and the one
        // rule here is that no request leaves loopback.
        std::fs::write(
            config.join("providers.toml"),
            format!(
                "# crates/harnessd/tests/oneshot_sigint.rs — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");
        // The preapproved list, so the long command is admitted with no head to ask: a one-shot
        // has nobody attached, and an ask would be `gate:no-head` before the command ever ran. Two
        // segments, because the command is two
        std::fs::write(
            config.join("permission.json"),
            "{\"bash\": {\"touch*\": \"allow\", \"sleep*\": \"allow\"}}\n",
        )
        .expect("permission.json");

        let workspace = dir.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = "s-oneshot-sigint".to_string();
        let store = dir.join("sessions.db");
        let socket = dir.join(format!("oneshot-{}.sock", std::process::id()));
        let log = dir.join("daemon.log");
        let out = std::fs::File::create(&log).expect("the log file");
        let err = out.try_clone().expect("a second handle on the log");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_harnessd"));
        cmd.arg("--socket")
            .arg(&socket)
            .arg("--store")
            .arg(&store)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session")
            .arg(&session)
            .arg("--role")
            .arg("leticode")
            .arg("--bash")
            .arg("--provider")
            .arg("deepseek")
            .arg("--prompt")
            .arg("run the command")
            .current_dir(repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_RUNTIME_DIR", dir)
            .env("LETIBOT_LANDING_ROOT", home.join("landing"))
            .env("LETIBOT_NOTES_REPO", home.join("notes"))
            .env("LETIBOT_LOGS_ROOT", home.join("logs"))
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("GLM_API_KEY")
            .env_remove("LETIBOT_PERMISSION")
            .env_remove("LETIBOT_MODE")
            .env_remove("LETIBOT_MODEL")
            .env_remove("LETIBOT_VOCAB")
            .env_remove("LETIBOT_ENDPOINT")
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let child = cmd.spawn().expect("harnessd starts");
        OneShot {
            child,
            log,
            store,
            session,
            workspace,
        }
    }

    /// Where the command's own marker lands - the workspace is the command's cwd.
    fn marker(&self) -> PathBuf {
        self.workspace.join(MARKER)
    }

    /// Wait until the command is RUNNING - the marker the command writes itself, so this is the
    /// condition and not a pause. Returns how long the daemon took from the provider reading
    /// the request to the command being up.
    fn wait_for_the_command(&mut self, fake: &Fake) -> Option<Duration> {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.marker().exists() {
                return Some(fake.first_read().map(|t| t.elapsed()).unwrap_or_default());
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Wait until a round is being generated - the request has reached the provider.
    fn wait_for_a_request(&mut self, fake: &Fake) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if fake.rounds() >= 1 {
                return true;
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// The Ctrl-C of the report: SIGINT, as the terminal's Ctrl-C delivers it.
    fn ctrl_c(&self) {
        let out = Command::new("kill")
            .arg("-INT")
            .arg(self.child.id().to_string())
            .output()
            .expect("kill runs");
        assert!(
            out.status.success(),
            "kill -INT {}: {}",
            self.child.id(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Wait for the process to go, up to `budget`. `Err(elapsed)` if it did not.
    fn wait_gone(&mut self, budget: Duration) -> Result<Duration, Duration> {
        let started = Instant::now();
        while started.elapsed() < budget {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(started.elapsed()),
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => panic!("waiting for the daemon: {e}"),
            }
        }
        Err(started.elapsed())
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The session as the store has it - the rows the interrupted turn left, in order. Read over
    /// a fresh connection with the daemon already gone: that is the *saved, not lost* half.
    fn rows(&self) -> Vec<TranscriptItem> {
        let store = Store::open(&self.store).expect("the store opens after the daemon is gone");
        let id = store
            .current_transcript_id(&self.session)
            .expect("the session's transcript id")
            .expect("the session has a transcript");
        store
            .load_transcript(&id)
            .expect("the transcript loads")
            .items
            .into_iter()
            .map(|(item, _, _)| item)
            .collect()
    }
}

impl Drop for OneShot {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_first_ctrl_c_interrupts_the_turn_and_the_one_shot_goes_in_seconds() {
    let dir = TempDir::new("oneshot-sigint");
    let fake = Fake::serve(Script::BashThenProse);
    let mut shot = OneShot::start(dir.path(), &fake);
    let Some(dispatch) = shot.wait_for_the_command(&fake) else {
        panic!(
            "the one-shot never had a command running ({} round(s) served); its log:\n{}",
            fake.rounds(),
            shot.log()
        );
    };
    // The number the fixture prints, and the reason a pause was a bet: this is how long the box
    // takes to get from a request the provider has read to a process the daemon has spawned.
    println!("  request read -> command running: {dispatch:?}");
    assert_eq!(
        fake.rounds(),
        1,
        "a second round was asked for while the first round's command was running — the turn did \
         not wait for it. Its log:\n{}",
        shot.log()
    );

    shot.ctrl_c();
    let took = match shot.wait_gone(CTRL_C_BUDGET) {
        Ok(took) => took,
        Err(elapsed) => panic!(
            "the one-shot did not go within {CTRL_C_BUDGET:?} of the first Ctrl-C ({elapsed:?} and \
             still running). Its log:\n{}",
            shot.log()
        ),
    };
    println!("  the signal to gone: {took:?}");

    let log = shot.log();
    assert!(
        log.contains("SIGINT (Ctrl-C)") && log.contains("interrupting the turn and stopping"),
        "the stop said nothing about the signal it was given:\n{log}"
    );

    let rows = shot.rows();
    let killed = rows
        .iter()
        .position(|r| {
            matches!(
                r,
                TranscriptItem::ToolResult { outcome, .. }
                    if serde_json::to_string(outcome)
                        .is_ok_and(|o| o.contains("killed by the daemon stopping"))
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "the command in flight was not recorded as killed ({took:?} after the signal); \
                 the rows are:\n{rows:#?}"
            )
        });
    assert!(
        !rows[killed + 1..].iter().any(|r| matches!(
            r,
            TranscriptItem::Assistant { text, .. } if !text.trim().is_empty()
        )),
        "the turn asked the model again after the interrupt, so its answer is in the transcript; \
         the rows after the killed command are:\n{:#?}",
        &rows[killed + 1..]
    );
}

#[test]
fn a_turn_that_never_comes_back_is_bounded_and_a_second_ctrl_c_exits_at_once() {
    let dir = TempDir::new("oneshot-sigint-hang");
    let fake = Fake::serve(Script::Hang);
    let mut shot = OneShot::start(dir.path(), &fake);
    assert!(
        shot.wait_for_a_request(&fake),
        "the one-shot never reached the provider; its log:\n{}",
        shot.log()
    );

    shot.ctrl_c();
    // A second Ctrl-C, a moment later - what a person does, and what must not have to wait for
    // the grace.
    std::thread::sleep(Duration::from_millis(300));
    shot.ctrl_c();
    let took = match shot.wait_gone(CTRL_C_BUDGET) {
        Ok(took) => took,
        Err(elapsed) => panic!(
            "the one-shot did not go within {CTRL_C_BUDGET:?} of the first Ctrl-C ({elapsed:?} and \
             still running). Its log:\n{}",
            shot.log()
        ),
    };
    let log = shot.log();
    assert!(
        log.contains("a second Ctrl-C — exiting now"),
        "the second Ctrl-C was not said, or did not end it ({took:?}):\n{log}"
    );
    // And the grace did not fire - which is what *exited at once* means, said by the daemon
    // itself instead of by a stopwatch on a busy box.
    assert!(
        !log.contains("the turn did not come back within"),
        "the second Ctrl-C waited out the grace instead of exiting at once ({took:?}):\n{log}"
    );
    assert!(
        took < CTRL_C_BUDGET,
        "the second Ctrl-C took {took:?}, which is not *at once* by any reading: {CTRL_C_BUDGET:?} \
         is this test's hang detector and it is far above the grace's own {}\n{log}",
        letibot_harnessd::daemon::ONE_SHOT_EXIT_GRACE.as_secs()
    );
}
