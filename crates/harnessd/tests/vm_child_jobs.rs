//! A child placed `where: firecode` may not end its turn over a job still running in its VM, and a
//! job the teardown kills is recorded and reported as killed - never as the exit its client came
//! back with.
//!
//! Driven against a fake `firecode` and a scripted model: no VM is booted, the "guest" command runs
//! here, and the fake's own log is what says whether the VM is still up.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions, SubagentExit};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::store::Store;
use letibot_transcript::{ToolCall, ToolOutcome};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::canned::sse_body;
use support::qwen::{THINK_CLOSE, a_plain_answer_turn, ids, spoken};
use support::stub::{read_request, write_frames, write_props};

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const THINK_OPEN: u32 = 248068;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the operator says to start the parent's turn. A marker, so a request is told to be the
/// parent's: the child's prompt carries the subtask and never these words.
const START: &str = "start the child in a VM\nmarker: the parent";

/// The subtask the parent hands the child - a marker too, and the only thing the child is asked.
const SUBTASK: &str = "background the long sleep\nmarker: the vm child";

/// The child's answer to its task, said while the job it backgrounded is still running.
const ANSWER: &str = "backgrounded the sleep; the tree is quiet";

/// What the child says in the turn its job's ending wakes - distinct from [`ANSWER`], so a turn
/// carrying it can only be the one the settlement started.
const AFTER_THE_JOB: &str = "the sleep ended; nothing is left running";

/// What the parent says once its `task` call has started the child.
const PARENT_DONE: &str = "the child is running in its VM";

/// The name the child gives the job, which is how a sentence about it names it.
const SLUG: &str = "long-sleep";

/// How long a driver waits for something a correct build produces in milliseconds. Generous on
/// purpose: the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// The fake firecode
// ---------------------------------------------------------------------------

/// The fake `firecode`: `up` succeeds, `in` runs the command here, and `down` leaves the marker
/// whose absence is "the VM is still up" and whose presence makes every `in` client come back - as
/// the real client does when the guest it was talking to stops.
const FIRECODE: &str = r#"#!/bin/bash
# A firecode that answers without a VM. Its log is `<project>.argv.log`, so the per-test thing is
# the project each call names rather than this file, which `$FIRECODE_BIN` makes process-wide.
set -u
[ $# -eq 0 ] && exit 0
root=$(cd "$(dirname "$0")/.." && pwd)
project=
for a in "$@"; do
  case "$a" in --project=*) project=${a#--project=};; esac
done
prev=
for a in "$@"; do
  [ "$prev" = "--project" ] && project=$a
  prev=$a
done
[ -n "$project" ] && printf 'ARGV %s\n' "$*" >> "$project.argv.log"
verb=$1; shift
case "$verb" in
  in)
    cmd=
    cwd=
    while [ $# -gt 0 ]; do
      case "$1" in
        --project) project=$2; shift 2 ;;
        --cwd) cwd=$2; shift 2 ;;
        *) cmd=$*; break ;;
      esac
    done
    # One argument is the command, as the real client's is; it runs under a shell in the guest.
    ( cd "$cwd" 2>/dev/null || cd "$project" 2>/dev/null || cd / ; bash -c "$cmd" ) &
    child=$!
    while kill -0 "$child" 2>/dev/null; do
      [ -e "$project.down" ] && exit 0
      sleep 0.05
    done
    wait "$child"
    exit $?
    ;;
  info)
    run=$(printf '%s' "$project" | cksum | cut -d' ' -f1)
    printf '{"run": "%s"}\n' "$run"
    ;;
  down)
    touch "$project.down"
    run=$(printf '%s' "$project" | cksum | cut -d' ' -f1)
    mkdir -p "$root/runs/$run"
    printf 'none\n' > "$root/runs/$run/copy_out"
    ;;
esac
exit 0
"#;

/// The one fake every test in this file shares, `$FIRECODE_BIN` pointing at it, and
/// `$XDG_CACHE_HOME` beside it so no VM project lands in the real home cache.
fn fake_firecode() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("r173-firecode-{}", std::process::id()));
        let bin = dir.join("bin").join("firecode");
        std::fs::create_dir_all(bin.parent().expect("a bin directory")).expect("the fake's bin");
        letibot_tools::testing::write_stub(&bin, FIRECODE);
        // SAFETY: once per process, and before anything that spawns a command reads it.
        unsafe {
            std::env::set_var("FIRECODE_BIN", &bin);
            std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        }
        bin
    })
    .clone()
}

/// The readback a helper that entered new namespaces prints: every inode differs from the caller's,
/// which is the one thing the probe reads as *there is a boundary here*. This file is not about the
/// boundary, so the numbers are the fixture's - the same shape `tests/remote_confine.rs` uses.
const FAKE_READBACK: &str = "\
printf 'ns user user:[4026535001]\\n'
printf 'ns mnt mnt:[4026535002]\\n'
printf 'ns pid pid:[4026535003]\\n'
printf 'ns net net:[4026535004]\\n'
printf 'ns ipc ipc:[4026535005]\\n'
printf 'ns uts uts:[4026535006]\\n'
printf 'ns cgroup cgroup:[4026535007]\\n'
printf 'seal nested held\\n'
printf 'seal nnp 1\\n'";

/// A `bwrap` that answers the probe and otherwise runs the command after `--`. A `leticode` session
/// with a shell asks for a wide seat, and a seat is a boundary this file is not about: the shim is
/// so the fixture needs nothing of the box but a process-lifetime tree.
const BWRAP: &str = r#"#!/bin/bash
# A bwrap that enters nothing: `--version`, the measured readback, and the command after `--`.
[ $# -eq 0 ] && exit 0
if [ "$1" = "--version" ]; then echo 'bubblewrap 0.11.1 (fake)'; exit 0; fi
args=(); cmd=(); envp=(); cleared=0; seen=0
argv=("$@"); i=0
while [ $i -lt ${#argv[@]} ]; do
  a="${argv[$i]}"
  if [ $seen -eq 1 ]; then cmd+=("$a"); i=$((i+1)); continue; fi
  case "$a" in
    --) seen=1 ;;
    --chdir) i=$((i+1)); args+=("--chdir" "${argv[$i]}") ;;
    --clearenv) cleared=1 ;;
    --setenv) i=$((i+1)); k="${argv[$i]}"; i=$((i+1)); envp+=("$k=${argv[$i]}") ;;
    --args) i=$((i+1)); while IFS= read -r -d '' -u "${argv[$i]}" b; do fromfd+=("$b"); done ;;
    *) args+=("$a") ;;
  esac
  i=$((i+1))
done
case "${cmd[*]}" in
  *'readlink /proc/self/ns/'*)
    READBACK
    exit 0 ;;
esac
cwd=. ; i=0
while [ $i -lt ${#args[@]} ]; do
  if [ "${args[$i]}" = "--chdir" ]; then cwd="${args[$((i+1))]}"; fi
  i=$((i+1))
done
cd "$cwd" 2>/dev/null || true
if [ $cleared -eq 1 ]; then exec env -i "${envp[@]}" "${cmd[@]}"; fi
exec "${cmd[@]}"
"#;

/// The fake `bwrap`, and `$LETIBOT_BWRAP` pointing at it.
fn fake_bwrap() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("r173-firecode-{}", std::process::id()));
        let bin = dir.join("bin").join("bwrap");
        std::fs::create_dir_all(bin.parent().expect("a bin directory")).expect("the fake's bin");
        letibot_tools::testing::write_stub(&bin, &BWRAP.replace("READBACK", FAKE_READBACK));
        // SAFETY: once per process, and before anything that spawns a command reads it.
        unsafe { std::env::set_var("LETIBOT_BWRAP", &bin) };
        bin
    })
    .clone()
}

/// What the fake was asked about one project, verb first, one entry per call.
fn asked(project: &Path) -> Vec<String> {
    std::fs::read_to_string(format!("{}.argv.log", project.display()))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("ARGV ").map(str::to_string))
        .collect()
}

/// Wait until `done` answers true, or fail with `what` unproven. Serves the bell meanwhile, as the
/// daemon's worker does: a settlement reaches a session only through `Sessions::wake`.
fn until(
    sessions: &mut Sessions<'_>,
    registry: &Registry,
    what: &str,
    mut done: impl FnMut() -> bool,
) {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if done() {
            return;
        }
        if let WorkOrIdle::Work(Work::Woken(id)) =
            registry.next_work_until(Some(Instant::now() + Duration::from_millis(20)))
        {
            let _ = sessions.wake(&id);
        }
    }
    panic!("{what} did not happen within {PATIENCE:?}");
}

/// What these tests need of the box beyond a vocabulary: a process-lifetime tree, because both a
/// `leticode` session's wide seat and a VM's own host scope one. A box without it says so and
/// skips rather than failing - see the operator's rule in `AGENTS.md`.
fn apparatus() -> Option<()> {
    let has_tree = letibot_tools::host_tree().is_ok();
    let has_tree = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        has_tree,
    )
    .is_some();
    has_tree.then_some(())
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A directory that removes itself: a leaked store per run fills the disk.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: AtomicUsize = AtomicUsize::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp dir");
        Self(path)
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

fn config(session_id: &str, workspace: &Path, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode` because it is the unconfined seat that names `task` and `bash`; a confined seat
    // cannot open on a box with no boundary.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.context_window = Some(131_072);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
    // The child's hold is re-checked on this clock, so a short window keeps the test quick.
    cfg.child_park_after = Some(Duration::from_millis(300));
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// Every fixture but the daemon, in the order the borrows need: the vocabulary comes out of a
/// `Parts` built over a config whose endpoint is not yet the stub's.
struct Scene {
    /// Held for its destructor only: the fixture's repository and store go with it.
    _dir: TempDir,
    parent: String,
    cfg: Config,
    parts: Parts,
    registry: Arc<Registry>,
    stub: Stub,
    store: PathBuf,
}

impl Scene {
    fn new(tag: &str, sleep_secs: u64) -> Scene {
        Scene::with_gate(tag, sleep_secs, None)
    }

    /// A scene whose child's answer turn stops half-streamed until `gate` opens.
    fn with_gate(tag: &str, sleep_secs: u64, gate: Option<Arc<Gate>>) -> Scene {
        fake_firecode();
        fake_bwrap();
        let dir = TempDir::new(tag);
        // A VM copies a repository, and a session with no repository to copy is refused - so the
        // workspace is a git tree with one commit.
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("the workspace");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["commit", "-q", "--allow-empty", "-m", "base"],
        ] {
            assert!(
                letibot_tools::testing::git(&repo)
                    .args(&args)
                    .status()
                    .expect("git runs")
                    .success(),
                "git {args:?}"
            );
        }
        letibot_tools::testing::repo_config(&repo);
        let store = dir.path().join("sessions.db");
        // One parent per test, so a scene's cleanup touches only its own children's projects.
        let parent = format!("s-parent-of-a-vm-child-{tag}");

        let parts = parts_for(&config(
            &parent,
            &repo,
            Endpoint::new("127.0.0.1", 1),
            &store,
        ));
        let stub = Stub::new(&parts.vocab, sleep_secs, gate);
        let cfg = config(&parent, &repo, stub.endpoint.clone(), &store);
        let registry = Registry::new();
        // The store source the daemon installs at startup.
        registry.set_source(
            letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("a store source"),
        );
        registry
            .create(&parent, "the parent", wiring(&cfg))
            .expect("the parent is created");
        Scene {
            _dir: dir,
            parent,
            cfg,
            parts,
            registry,
            stub,
            store,
        }
    }

    fn open(&self) -> Sessions<'_> {
        Sessions::open_first(&self.parts, self.cfg.clone(), Arc::clone(&self.registry))
            .expect("the daemon opens")
    }

    /// The child's row in the store, as a restart would read it.
    fn child_state(&self, child: &str) -> Option<(String, String)> {
        let store = Store::open(&self.store).ok()?;
        let row = store.subagent_run(child).ok()??;
        Some((row.state, row.outcome.unwrap_or_default()))
    }

    /// Has the child settled? The row exists from its spawn and reads `running` until then, so
    /// "a row is there" is not the question - what word it settled with is.
    fn settled(&self, child: &str) -> bool {
        self.child_state(child)
            .is_some_and(|(state, _)| state != "running")
    }

    /// What the fake `firecode` was asked about this child's VM.
    fn vm_calls(&self, child: &str) -> Vec<String> {
        asked(&letibot_tools::firecode::FirecodeBackend::project_of(child))
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        let cache = letibot_tools::firecode::FirecodeBackend::project_of(&self.parent);
        let (Some(cache), Some(mine)) = (cache.parent(), cache.file_name()) else {
            return;
        };
        let mine = format!("{}-sub-", mine.to_string_lossy());
        for entry in std::fs::read_dir(cache).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&mine) {
                let path = entry.path();
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    }
}

/// The tree is drained before the process can exit, whatever the test asserted: `exit()`
/// finalizes libllama underneath a child thread still inside it.
struct DrainOnDrop<'s, 'p> {
    sessions: &'s std::cell::RefCell<Sessions<'p>>,
    registry: &'s Registry,
}

impl Drop for DrainOnDrop<'_, '_> {
    fn drop(&mut self) {
        // The daemon's own order, because a child only leaves on a closed hub.
        self.registry.close();
        let gave_up = matches!(
            self.sessions.borrow().drain_subagents(PATIENCE),
            SubagentExit::GaveUp { .. }
        );
        if gave_up && !std::thread::panicking() {
            panic!("a subagent thread outlived {PATIENCE:?} of waiting with its hub closed");
        }
    }
}

/// The daemon's own door: a prompt on the parent's hub, taken the way the worker takes it.
fn cmd_of(hub: &Hub, text: &str) -> letibot_sessionlog::hub::QueuedCommand {
    assert!(
        hub.submit_daemon(
            "test",
            "test-1",
            CommandKind::Prompt {
                text: text.to_string(),
                attachments: Vec::new(),
                context: None,
            },
        ),
        "the hub refused the prompt"
    );
    hub.try_command().expect("a command is on the queue")
}

/// The child the parent's `task` call started, read off the parent's own log.
fn the_child(hub: &Hub) -> Option<String> {
    hub.retained().iter().find_map(|e| match &e.event {
        letibot_sessionlog::SessionEvent::Subagent { subagent_id, .. } => Some(subagent_id.clone()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// A latch the stub waits on: the test opens it once it has done what must land mid-stream.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().expect("the gate") = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let g = self.open.lock().expect("the gate");
        let _ = self
            .cv
            .wait_timeout_while(g, PATIENCE, |open| !*open)
            .expect("the gate");
    }
}

/// Half of `frames`, then `gate`, then the rest: a turn the test can act on while it streams.
/// A prefix of the body is the same bytes whatever follows it, so the halves join exactly.
fn write_frames_gated(mut stream: std::net::TcpStream, frames: &[Frame], gate: &Gate) {
    use std::io::Write;
    let body = sse_body(frames);
    let head = sse_body(&frames[..frames.len() / 2]);
    let head = &head[..head.len() - b"0\r\n\r\n".len()];
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    );
    let _ = stream.write_all(head).and_then(|_| stream.flush());
    gate.wait();
    let _ = stream
        .write_all(&body[head.len()..])
        .and_then(|_| stream.flush());
}

/// A model that answers its first request with a `task where: firecode`, and answers the child
/// according to which of its turns the request holds.
struct Stub {
    endpoint: Endpoint,
    /// How many requests were the child's.
    child_turns: Arc<AtomicUsize>,
    /// Every child prompt, in the order they were asked: what a turn was TOLD is the assertion
    /// that a job's ending reached it.
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn new(
        vocab: &Arc<letibot_tokencore::Vocab>,
        sleep_secs: u64,
        gate: Option<Arc<Gate>>,
    ) -> Stub {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let parent_turn = Arc::new(a_vm_call_turn(
            vocab,
            "task",
            &[("prompt", SUBTASK), ("where", "firecode")],
            30,
        ));
        let background = Arc::new(a_vm_call_turn(
            vocab,
            "bash",
            &[
                ("command", &format!("sleep {sleep_secs}")),
                ("background", "true"),
                ("slug", SLUG),
            ],
            30,
        ));
        let parent_done = Arc::new(a_plain_answer_turn(vocab, "thinking", PARENT_DONE, 30));
        let answer = Arc::new(a_plain_answer_turn(vocab, "thinking", ANSWER, 30));
        let after = Arc::new(a_plain_answer_turn(vocab, "thinking", AFTER_THE_JOB, 30));
        let child_turns = Arc::new(AtomicUsize::new(0));
        let prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let vocab = Arc::clone(vocab);
        let seen = Arc::clone(&child_turns);
        let said = Arc::clone(&prompts);
        std::thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let parent_turn = Arc::clone(&parent_turn);
                let parent_done = Arc::clone(&parent_done);
                let background = Arc::clone(&background);
                let answer = Arc::clone(&answer);
                let after = Arc::clone(&after);
                let vocab = Arc::clone(&vocab);
                let seen = Arc::clone(&seen);
                let said = Arc::clone(&said);
                let gate = gate.clone();
                // One thread per connection: the parent's turn and the child's run at once.
                std::thread::spawn(move || {
                    let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                    // `/props` is not a turn: a spawn asks the endpoint for its window.
                    if line.starts_with("GET ") {
                        let _ = write_props(stream);
                        return;
                    }
                    let asked = prompt_of(&vocab, &body);
                    // The child's own prompt, told from the parent's: the parent's carries the
                    // subtask too, in the `task` call's arguments, so its own words are the half
                    // only the parent has.
                    let is_child = asked.contains(SUBTASK) && !asked.contains(START);
                    // One `task` call, then the parent ends its turn: every child shares the counter
                    // below, so a second child would take turns the first one was scripted for.
                    if !is_child {
                        let frames = if started_a_child(&asked) {
                            &parent_done
                        } else {
                            &parent_turn
                        };
                        let _ = write_frames(stream, frames);
                        return;
                    }
                    said.lock().expect("the child's prompts").push(asked);
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    let frames: &[Frame] = match n {
                        0 => &background,
                        1 => &answer,
                        _ => &after,
                    };
                    match (n, gate) {
                        (1, Some(gate)) => write_frames_gated(stream, frames, &gate),
                        _ => {
                            let _ = write_frames(stream, frames);
                        }
                    }
                });
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            child_turns,
            prompts,
        }
    }

    fn child_turns(&self) -> usize {
        self.child_turns.load(Ordering::SeqCst)
    }

    /// The prompt of the child's `n`th turn, counted from zero - what a turn was TOLD is the only
    /// way to say a job's ending reached it.
    fn prompt(&self, n: usize) -> Option<String> {
        self.prompts
            .lock()
            .expect("the child's prompts")
            .get(n)
            .cloned()
    }
}

/// Has the parent's `task` call already come back? Its result is the first tool response the
/// parent's prompt carries.
fn started_a_child(prompt: &str) -> bool {
    prompt.contains("<tool_response>")
}

/// What the model was handed for one request - the ids, detokenized with the session's own
/// vocabulary. A prompt is the one thing that says who a request is for.
fn prompt_of(vocab: &letibot_tokencore::Vocab, body: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(ids) = json["prompt"].as_array() else {
        return String::new();
    };
    let ids: Vec<u32> = ids
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u32))
        .collect();
    vocab.detokenize(&ids, true).unwrap_or_default()
}

/// One tool call in the Qwen dialect's own shape - a string parameter is emitted raw and every
/// other value as JSON, which is how `background: true` is spelled - and the end of the turn.
fn a_vm_call_turn(
    vocab: &letibot_tokencore::Vocab,
    tool: &str,
    params: &[(&str, &str)],
    n_prompt: u64,
) -> Vec<Frame> {
    let mut call = format!("\n<function={tool}>\n");
    for (k, v) in params {
        call.push_str(&format!("<parameter={k}>\n{v}\n</parameter>\n"));
    }
    call.push_str("</function>\n");
    let call_ids = ids(vocab, &call);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        // The think fences and the two tool-call fences: the accumulator counts what was
        // streamed, so this has to agree with it exactly.
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

// ---------------------------------------------------------------------------
// THE HOLD (R173 item 1)
// ---------------------------------------------------------------------------

/// A VM child that backgrounds a sleep and ends its turn: the VM stays up, the child is woken
/// with the job's exit, and only then does the VM close - so its answer is a finish over a job
/// that ended rather than one that was killed under it.
#[test]
fn a_vm_childs_turn_does_not_end_its_vm_while_its_job_runs() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = apparatus() else {
        return;
    };
    let scene = Scene::new("vm-child-hold", 2);
    let sessions_cell = std::cell::RefCell::new(scene.open());
    let _drain = DrainOnDrop {
        sessions: &sessions_cell,
        registry: &scene.registry,
    };
    let mut sessions = sessions_cell.borrow_mut();

    let parent_hub = scene.registry.get(&scene.parent).expect("the parent's hub");
    let first = cmd_of(&parent_hub, START);
    let outcome = sessions.dispatch(&scene.parent, &first);
    assert!(
        matches!(outcome, Outcome::Replied(_)),
        "the parent's first turn did not run (`Replied` expected)"
    );
    let child = the_child(&parent_hub).expect("the `task` call names the child it started");

    // The child's first turn backgrounds the sleep, and its second ends the turn with the job
    // still running. Both are requests this stub answered.
    until(
        &mut sessions,
        &scene.registry,
        "the child answered its task over a running job",
        || scene.stub.child_turns() >= 2,
    );
    let calls = scene.vm_calls(&child);
    assert!(
        calls.iter().any(|c| c.starts_with("up")),
        "the VM was never booted: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("sleep")),
        "the job never ran in the VM: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("down")),
        "the VM was brought down under the job the child left running: {calls:?}"
    );

    // And then the job ends, the child is woken with it, and only after that turn does the VM go.
    until(
        &mut sessions,
        &scene.registry,
        "the child was woken with its job's ending",
        || scene.stub.child_turns() >= 3,
    );
    let woken = scene.stub.prompt(2).expect("the child's third turn");
    assert!(
        woken.contains("a job you backgrounded has ended"),
        "the child's third turn was not woken by its job's ending: {woken}"
    );
    assert!(
        woken.contains(SLUG),
        "the wake does not name the job it is about: {woken}"
    );
    until(
        &mut sessions,
        &scene.registry,
        "the VM was brought down once nothing was running in it",
        || scene.vm_calls(&child).iter().any(|c| c.starts_with("down")),
    );
    until(
        &mut sessions,
        &scene.registry,
        "the child's finish was recorded",
        || scene.settled(&child),
    );
    let (state, said) = scene.child_state(&child).expect("the child's row");
    assert_eq!(state, "done", "the child's row is not a finish: {said}");
    assert!(
        said.contains(AFTER_THE_JOB),
        "the finish carries the child's last word, which is the turn its job started: {said}"
    );
    assert!(
        !said.contains("lost with its VM") && !said.contains("killed"),
        "a job that ended before the VM closed was reported as killed: {said}"
    );

    // The parent's own reading says the same thing through the tool a parent collects with. The
    // runner settles after the row, on a wake of its own, so a loaded box can read it running once.
    let began = Instant::now();
    let result = loop {
        let read = task_result(&sessions, &scene.parent, &child);
        if !read.contains("has not answered yet") || began.elapsed() > PATIENCE {
            break read;
        }
        if let WorkOrIdle::Work(Work::Woken(id)) = scene
            .registry
            .next_work_until(Some(Instant::now() + Duration::from_millis(20)))
        {
            let _ = sessions.wake(&id);
        }
    };
    assert!(
        !result.contains("killed under it") && !result.contains("lost with its VM"),
        "`task_result` reads a clean finish as a kill: {result}"
    );
    assert!(
        result.contains(AFTER_THE_JOB),
        "`task_result` does not carry the child's last word: {result}"
    );
}

// ---------------------------------------------------------------------------
// THE KILL (R173 item 2)
// ---------------------------------------------------------------------------

/// The same child, stopped while its job is running: the VM closes under the job, so the job is
/// killed with it - recorded as that and never as the exit its client came back with - and the
/// child's finish is not reported as a clean one.
#[test]
fn a_job_killed_with_the_vm_is_reported_as_killed_and_not_as_a_finish() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = apparatus() else {
        return;
    };
    let scene = Scene::new("vm-child-killed", 30);
    let sessions_cell = std::cell::RefCell::new(scene.open());
    let _drain = DrainOnDrop {
        sessions: &sessions_cell,
        registry: &scene.registry,
    };
    let mut sessions = sessions_cell.borrow_mut();

    let parent_hub = scene.registry.get(&scene.parent).expect("the parent's hub");
    let first = cmd_of(&parent_hub, START);
    assert!(
        matches!(
            sessions.dispatch(&scene.parent, &first),
            Outcome::Replied(_)
        ),
        "the parent's first turn did not run"
    );
    let child = the_child(&parent_hub).expect("the `task` call names the child it started");
    until(
        &mut sessions,
        &scene.registry,
        "the child answered its task over a running job",
        || scene.stub.child_turns() >= 2,
    );
    let running = scene.vm_calls(&child);
    assert!(
        running.iter().any(|c| c.contains("sleep")),
        "the job never ran in the VM: {running:?}"
    );
    assert!(
        !running.iter().any(|c| c.starts_with("down")),
        "the VM was down before the child was stopped: {running:?}"
    );

    // The stop is the parent's own door, and the child is holding for its job when it arrives.
    stop_the_child(&sessions, &scene, &child);
    the_kill_is_reported(&mut sessions, &scene, &child);
}

/// The same stop, landing while the child's task turn is still streaming: the interrupt is spent
/// on that turn, and the hold that follows must still read it as the stop it was.
#[test]
fn a_stop_during_the_task_turn_still_kills_the_job_with_the_vm() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = apparatus() else {
        return;
    };
    let gate = Arc::new(Gate::default());
    let scene = Scene::with_gate("vm-child-stopped-mid-turn", 30, Some(Arc::clone(&gate)));
    let sessions_cell = std::cell::RefCell::new(scene.open());
    let _drain = DrainOnDrop {
        sessions: &sessions_cell,
        registry: &scene.registry,
    };
    let mut sessions = sessions_cell.borrow_mut();

    let parent_hub = scene.registry.get(&scene.parent).expect("the parent's hub");
    let first = cmd_of(&parent_hub, START);
    assert!(
        matches!(
            sessions.dispatch(&scene.parent, &first),
            Outcome::Replied(_)
        ),
        "the parent's first turn did not run"
    );
    let child = the_child(&parent_hub).expect("the `task` call names the child it started");
    // The answer turn has been asked for and is held half-streamed by the gate.
    until(
        &mut sessions,
        &scene.registry,
        "the child's answer turn began",
        || scene.stub.child_turns() >= 2,
    );
    stop_the_child(&sessions, &scene, &child);
    gate.open();
    the_kill_is_reported(&mut sessions, &scene, &child);
}

/// The parent's stop, which must name the child.
fn stop_the_child(sessions: &Sessions<'_>, scene: &Scene, child: &str) {
    let stopped = sessions
        .harness_of(&scene.parent)
        .expect("the parent is open")
        .stop_children();
    assert!(
        stopped.as_deref().is_some_and(|s| s.contains(child)),
        "the parent stopped nothing: {stopped:?}"
    );
}

/// A stopped VM child's rows and `task_result`: killed, its job `lost with its VM`, never a finish.
fn the_kill_is_reported(sessions: &mut Sessions<'_>, scene: &Scene, child: &str) {
    let child = child.to_string();
    // The stop writes `killed` / `stopped` the moment it is accepted; the finish rewrites it once
    // the child's thread has closed the VM, and that row is the one under test.
    until(
        sessions,
        &scene.registry,
        "the child's finish was recorded",
        || {
            scene
                .child_state(&child)
                .is_some_and(|(state, said)| state != "running" && said != "stopped")
        },
    );
    let (state, said) = scene.child_state(&child).expect("the child's row");
    assert_ne!(
        state, "done",
        "a child whose job was killed with its VM is recorded as a finish: {said}"
    );
    assert_eq!(state, "killed", "the child's row: {said}");
    assert!(
        said.contains("lost with its VM"),
        "the kill is not named with its reason: {said}"
    );

    // And the job's own row says the kill, whatever its client exited with - the fake returns 0
    // when the guest goes down, which is exactly the exit that must not read as a finish.
    let store = Store::open(&scene.store).expect("the store");
    let handle = store
        .jobs(&child)
        .expect("the jobs")
        .into_iter()
        .find(|j| j.command.contains("sleep"))
        .expect("the job's row")
        .handle;
    let row = |h: &str| {
        Store::open(&scene.store)
            .expect("the store")
            .jobs(&child)
            .expect("the jobs")
            .into_iter()
            .find(|j| j.handle == h)
            .map(|j| j.state)
    };
    assert_eq!(
        row(&handle).as_deref(),
        Some("lost with its VM"),
        "the job's row is not the kill it was"
    );
    // The watcher settles the client after `down` has already written the kill; give it that
    // window and read the row again, because the defect was the settle overwriting this.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        row(&handle).as_deref(),
        Some("lost with its VM"),
        "the settle after the VM closed wrote its client's exit over the kill"
    );

    // The parent's result is the same reading: not a finish, and the kill named.
    let result = task_result(sessions, &scene.parent, &child);
    assert!(
        result.contains("killed under it"),
        "`task_result` does not say the child finished with a job killed under it: {result}"
    );
    assert!(
        result.contains("lost with its VM"),
        "`task_result` does not name the job's kill: {result}"
    );
}

/// `task_result` through the door a parent actually collects with.
fn task_result(sessions: &Sessions<'_>, parent: &str, child: &str) -> String {
    let rt = sessions
        .harness_of(parent)
        .expect("the parent is open")
        .runtime_handle();
    let call = ToolCall {
        id: "call_task_result".into(),
        name: "task_result".into(),
        arguments: serde_json::json!({"task": child}).to_string(),
    };
    let mut sink = letibot_tools::NullToolSink;
    let got = rt.invoke("t1", &call, &mut sink);
    // An abstention's headline is half of what the parent reads.
    match got.outcome {
        ToolOutcome::Abstained { reason } | ToolOutcome::Failed { reason } => {
            format!("{reason}\n\n{}", got.payload)
        }
        _ => got.payload,
    }
}
