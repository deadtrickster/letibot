//! A one-shot's own background jobs: the run does not exit while one it started is open.
//!
//! The bug this pins is the one an operator hit twice: `letibot --mode allow-all "…"` printed the
//! answer, said `kept (one-shot ended): a job is open`, and exited 0 with the work half done. A
//! foreground command past `[exec] foreground_seconds` is promoted to a job, so this is the
//! ordinary path of any one-shot that runs a build.
//!
//! The three cases are the whole rule: a job that ends gives the run another turn and a pass; a
//! job that never ends stops at the budget with its own exit status and one line; and a run with
//! no jobs is untouched.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The exit status a one-shot that ran out of its budget ends on. Held here as a number rather than
/// imported, so a change to the constant is a change this test disagrees with out loud.
const EXIT_STILL_OPEN: i32 = 75;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch(&format!("letibot-{tag}"));
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

/// What the fake provider answers, round by round.
#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// Round one: one `bash` command with no `timeout_ms`, which the promotion threshold turns
    /// into a job. Every later round: prose, which is a turn ending.
    JobThenProse,
    /// Round one asks for a command that never ends, then every later round is prose.
    ForeverJob,
    /// Every round is prose: the run that starts nothing, which is what this must not change.
    Prose,
}

/// A fake provider on a loopback port, counting the rounds it has served.
struct Fake {
    url: String,
    rounds: Arc<AtomicUsize>,
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
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let _ = read_request(&conn);
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let asks_for_a_job = n == 0 && script != Script::Prose;
                let body = if asks_for_a_job {
                    // No `timeout_ms`: the deadline is what would kill it, and the point of this
                    // fixture is the promotion that happens INSTEAD of the kill.
                    let command = match script {
                        Script::ForeverJob => "sleep 600",
                        _ => "sleep 3 && echo done",
                    };
                    let arguments = serde_json::json!({ "command": command }).to_string();
                    frames([
                        serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                            "index": 0,
                            "id": "call_job_1",
                            "function": { "name": "bash", "arguments": arguments },
                        }]}}]}),
                        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
                    ])
                } else {
                    frames([
                        serde_json::json!({"choices":[{"delta":{"content":
                            format!("round {n} done")}}]}),
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
        Fake { url, rounds }
    }

    fn rounds(&self) -> usize {
        self.rounds.load(Ordering::SeqCst)
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
    /// Where the daemon's stderr went - its diagnostics, which is where the budget line is.
    log: PathBuf,
}

impl OneShot {
    /// Start one. `extra` is the flags this case is about - the budget, mostly.
    fn start(dir: &Path, fake: &Fake, extra: &[&str]) -> OneShot {
        let home = dir.join("home");
        let config = dir.join("config/letibot");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::create_dir_all(home.join("logs")).expect("logs dir");
        // A key in the operator's `providers.toml` would decide where a request goes, and the one
        // rule here is that no request leaves loopback.
        std::fs::write(
            config.join("providers.toml"),
            format!(
                "# crates/harnessd/tests/oneshot_jobs.rs — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");
        // The preapproved list, so the command is admitted with no head to ask: a one-shot has
        // nobody attached, and an ask would be `gate:no-head` before the command ever ran. Two
        // segments, because the command is two.
        std::fs::write(
            config.join("permission.json"),
            "{\"bash\": {\"sleep*\": \"allow\", \"echo*\": \"allow\"}}\n",
        )
        .expect("permission.json");

        let workspace = dir.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        // One second rather than the default sixty, so the promotion happens inside a test. This
        // is the project's own `[exec]`, which is exactly how an operator would make it shorter.
        std::fs::write(
            workspace.join("leticode.toml"),
            "[exec]\nforeground_seconds = 1\n",
        )
        .expect("leticode.toml");

        let store = dir.join("sessions.db");
        let socket = dir.join(format!("oneshot-{}.sock", std::process::id()));
        let log = dir.join("daemon.log");
        let out = std::fs::File::create(&log).expect("the log file");
        let err = out.try_clone().expect("a second handle on the log");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root");
        let mut cmd = letibot_testenv::daemon(env!("CARGO_BIN_EXE_harnessd"));
        cmd.arg("--socket")
            .arg(&socket)
            .arg("--store")
            .arg(&store)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session")
            .arg("s-oneshot-jobs")
            .arg("--role")
            .arg("leticode")
            .arg("--bash")
            .arg("--provider")
            .arg("deepseek");
        for a in extra {
            cmd.arg(a);
        }
        cmd.arg("--prompt")
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
        OneShot { child, log }
    }

    /// Wait for the run to end, and answer its status and its whole log.
    fn finish(&mut self, budget: Duration) -> (Option<i32>, String) {
        let deadline = Instant::now() + budget;
        let status = loop {
            match self.child.try_wait().expect("waitpid") {
                Some(s) => break Some(s),
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        };
        let said = std::fs::read_to_string(&self.log).unwrap_or_default();
        (status.and_then(|s| s.code()), said)
    }
}

/// The guard every case here shares: the daemon this starts seats a shell, so it needs a boundary
/// this box may not be able to build (no cgroup v2, no user namespace). Without one it refuses to
/// run the command at all, and a red here would be the box's rather than the rule's.
fn a_box_that_can_seat_a_shell() -> bool {
    let here = letibot_tools::host_tree().is_ok();
    let apparatus = "a process-lifetime tree (cgroup v2; process groups on macOS)";
    letibot_tokencore::apparatus::present(apparatus, here).is_some()
}

/// A turn that starts a job and ends: the job's end is the run's next turn, and the run passes.
#[test]
fn a_job_that_ends_gives_the_one_shot_another_turn_and_it_exits_zero() {
    if !a_box_that_can_seat_a_shell() {
        return;
    }
    let dir = TempDir::new("oneshot-jobs-settles");
    let fake = Fake::serve(Script::JobThenProse);
    let mut run = OneShot::start(dir.path(), &fake, &[]);
    let (code, log) = run.finish(Duration::from_secs(180));
    assert_eq!(
        code,
        Some(0),
        "the run ends a pass once its job has ended; log:\n{log}"
    );
    // Round 1 is the tool call, round 2 the prose that ends the first turn, and round 3 the turn
    // the job's settlement opened. Two rounds would be a run that exited with the job open.
    assert!(
        fake.rounds() >= 3,
        "the job's end ran no turn: the provider served {} round(s); log:\n{log}",
        fake.rounds()
    );
    assert!(
        log.contains("turn(s) after the job(s)"),
        "the log does not say the tail ran; log:\n{log}"
    );
    // And the model was handed the harness's own sentence, not a one-shot's paraphrase of it.
    assert!(
        !log.contains("kept (one-shot ended): a job is open"),
        "the session was kept back by a job it should have waited for; log:\n{log}"
    );
}

/// A job that never ends stops the run at the budget, with its own status and one line naming what
/// is still running and how to resume it.
#[test]
fn a_job_that_never_ends_stops_at_the_budget_with_its_own_exit_status() {
    if !a_box_that_can_seat_a_shell() {
        return;
    }
    let dir = TempDir::new("oneshot-jobs-budget");
    let fake = Fake::serve(Script::ForeverJob);
    let started = Instant::now();
    let mut run = OneShot::start(dir.path(), &fake, &["--one-shot-budget", "30s"]);
    let (code, log) = run.finish(Duration::from_secs(120));
    assert_eq!(
        code,
        Some(EXIT_STILL_OPEN),
        "a run out of budget with a job open is neither a pass nor an ordinary failure; log:\n{log}"
    );
    // The one line: the handles, and the session id to resume.
    assert!(
        log.contains("`j") && log.contains("`letibot --session s-oneshot-jobs`"),
        "the last line does not name the open job and the id to resume; log:\n{log}"
    );
    assert!(
        log.contains("budget of 30s"),
        "the line does not say which budget it ran out of; log:\n{log}"
    );
    // 30 s is the bound, not the job's 600: this is the budget ending the run, not the job.
    assert!(
        started.elapsed() < Duration::from_secs(110),
        "the run outlived its budget by more than its orderly tail: {:?}",
        started.elapsed()
    );
}

/// A run that starts nothing is untouched: prose, one round, and exit 0.
#[test]
fn a_turn_with_no_jobs_exits_as_it_always_did() {
    if !a_box_that_can_seat_a_shell() {
        return;
    }
    let dir = TempDir::new("oneshot-jobs-none");
    let fake = Fake::serve(Script::Prose);
    let mut run = OneShot::start(dir.path(), &fake, &[]);
    let (code, log) = run.finish(Duration::from_secs(120));
    assert_eq!(code, Some(0), "log:\n{log}");
    assert_eq!(
        fake.rounds(),
        1,
        "a run with no job ran more than its one turn; log:\n{log}"
    );
    assert!(
        !log.contains("turn(s) after the job(s)"),
        "a run with no job reported a job tail; log:\n{log}"
    );
}
