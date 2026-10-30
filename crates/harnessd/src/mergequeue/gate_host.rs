//! **The gate on a declared host** — the operator's instruction of 2026-10-29: *"i told you about
//! other hosts so you actually use that capability for running gates and subagents fanouts"*.
//!
//! `[queue] gate_host = "<name>"` in `leticode.toml`, or `--gate-host <name>` on the daemon, moves
//! the merge gate's **steps** to a machine the operator declared in `hosts.toml`. Empty — the
//! default — is [`super::gate::run_with_memory`] and nothing else: this machine, byte for byte.
//!
//! # What crosses the wire, and what does not
//!
//! **The landing tree goes over, and the merge stays here.** The host runs `sh -c <step>` in a
//! copy of the landing worktree under the host's own `root` (`~/letibot-remote/gate/<repo>-<hex>/`)
//! and never sees a ref of the repository's: the target branch moves by a compare-and-swap on this
//! box's `refs/heads/<target>` ([`super::git::update_ref_cas`]), and nothing in this module runs a
//! `git` that writes to anything but the host's own copy.
//!
//! # Two transports, and why both
//!
//! * **`rsync`** carries the tree — the brief's transport, and the right one for it: a landing
//!   worktree is not only its commit. `[queue] gate_carry` copies gitignored inputs into it
//!   (`images/`, `vendor/` — a firecode VM's kernel and rootfs), and those are in no commit, so
//!   only a filesystem copy carries them. `--delete` so a file the branch removed is gone on the
//!   host too; `/.git` and `/target` excluded (the first is a *file* naming a path on this box,
//!   the second is the local build cache and would be gigabytes of nothing useful there).
//! * **A `git` bundle** carries the repository's own metadata, because `rsync` cannot: a landing
//!   worktree's `.git` is a file pointing at `<repo>/.git/worktrees/<name>`, which means nothing
//!   on another machine — and the gate's FIRST step is `sh scripts/check-fmt.sh main`, which runs
//!   `git diff main HEAD` and `git status --porcelain`. Without objects and a ref named `main`
//!   that step fails on every branch and the whole gate is red for a reason that has nothing to do
//!   with the code. So the tip and the target branch go over as a bundle (`git fetch` into the
//!   host's bare `src.git`, then a worktree at the tip), and the rsync'd tree lands on top of it:
//!   the index and `HEAD` are the commit, the working tree is the landing tree — exactly the state
//!   the local gate runs in.
//!
//! The bundle is **incremental**: the host's own refs are the record of what it already has
//! (`git bundle create … --not <its tips>`), so the first gate pays for the history and every one
//! after it pays for the branch.
//!
//! # The warm cache
//!
//! `CARGO_TARGET_DIR` is `<dir>/target` — *outside* the rsync'd tree and *outside* the worktree's
//! `--delete`, so it survives every gate of that repository on that host. A cold gate there would
//! be worse than a local one (the whole dependency tree, downloaded and compiled from nothing);
//! measured on lab2x1, see `docs/remote-hosts.md` § *The merge gate on a declared host*.
//!
//! # A host that cannot be reached is a verdict, not a hang
//!
//! Every call is bounded ([`letibot_tools::rpc::run_bounded`]) and goes through the same breaker
//! the rest of the fleet path uses ([`letibot_tools::rpc::before`]), so a host that is down is a
//! [`GateVerdict::Infra`] with a sentence in the first second — the tip UNJUDGED and unblocked,
//! not red. Every step is started **detached, under `setsid`, with a pid file**, so the group can
//! be killed when the deadline passes, the daemon stops or the lease goes: the one window a step
//! can outlive its gate is a daemon killed with `SIGKILL` between two polls, which is the window
//! the local gate has too.
//!
//! # Two clocks, and which one is which
//!
//! A gate on a host is under **two** bounds and they are not the same number, which cost the
//! queue a halt on 2026-10-30:
//!
//! * **The step's** — [`GateControl::deadline`], `[queue]`'s `gate_deadline`, thirty minutes. It is
//!   the only thing that may end a step, and a first gate on a host needs all of it: a cold
//!   `CARGO_TARGET_DIR` is a whole dependency tree compiled from nothing.
//! * **One poll's** — [`POLL_BOUND`], seconds. It asks *can this machine fork a shell and answer*,
//!   and the answer on a box whose own build holds every core can be many seconds.
//!
//! A poll that is not answered is therefore **retried, not a verdict**: the question is idempotent
//! (its answer is `tail -c +<offset>`, so nothing is lost by asking again) and a miss says nothing
//! about the code. What ends a step is its own deadline, or a poll that reaches no machine at all
//! — which is the breaker's judgement, not this module's guess. Three missed polls in a row are
//! ninety seconds of a machine that will not fork a shell, and that is where the breaker
//! ([`letibot_tools::rpc::BREAKER_THRESHOLD`]) stops answering: the step is then UNJUDGED — a
//! retry, never a red branch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use letibot_tokencore::store::MergeGateOutcome;
use letibot_tools::remote::{Hosts, Paths, Ssh, expand_root};
use letibot_tools::rpc;

use super::gate::{
    Done, GateControl, GateRun, GateVerdict, StepEnd, StepExit, StepMemory, assemble, clause_of,
    judge, now_ms, read_tail, tempfile_in,
};
use super::git;

/// **How long one poll may take** — and this is the *health* question, not the step's deadline:
/// *can this machine fork a shell and answer?* Over the connection's `ControlMaster` that is a few
/// milliseconds on an idle box and **seconds** on a busy one — measured on lab2x1 while its own
/// `cargo test --workspace` held 25 of 32 cores, where a poll took longer than the ten seconds this
/// bound used to be, and the step was killed for it.
///
/// Thirty, because a machine that cannot answer a question in thirty seconds is not answering, and
/// because a missed poll costs nothing ([`Remote::run_step_into`] asks again): the bound only
/// decides when a poll stops waiting, never when a step stops running.
pub const POLL_BOUND: Duration = Duration::from_secs(30);

/// How often a running step is asked how it is — one ssh round trip, which over the connection's
/// `ControlMaster` is a few milliseconds. A second is the same order as the local gate's 100 ms
/// poll and 1/1800th of the calls a 30-minute step could make.
pub const POLL_EVERY: Duration = Duration::from_millis(1000);

/// The wall clock on one arranging call (`mkdir`, a `git fetch`, a `git worktree`), on the
/// transfer and on the kill. Each names what it bounds in the sentence it refuses with.
const SETUP_BOUND: Duration = Duration::from_secs(120);
const TRANSFER_BOUND: Duration = Duration::from_secs(900);
const KILL_BOUND: Duration = Duration::from_secs(30);

/// **`ssh`'s own exit code for *the connection failed*** — never a remote command's, by OpenSSH's
/// convention. It is the one code that says the far side never ran anything, so it is what this
/// module counts as *not reached* for the breaker, and what it refuses with a sentence about the
/// machine rather than about the step.
const SSH_TROUBLE: i32 = 255;

/// **The gate, on `name`'s machine** — the whole of [`super::gate::run_gate`]'s host arm.
///
/// Everything that can go wrong before the first step (a name that is not declared, a key that is
/// missing, a host that does not answer, a tree that will not arrive) is one [`GateVerdict::Infra`]
/// with the reason in it and every step `not_run`: the branch was not judged, and saying so is the
/// difference between a queue that parks a good branch and a queue that lands a bad one.
pub fn run_on_host(name: &str, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
    let remote = match Remote::open(name, worktree) {
        Ok(r) => r,
        Err(why) => return infra(steps, why),
    };
    let mut cost = Cost::default();
    if let Err(why) = remote.prepare(ctl, &mut cost) {
        return infra(steps, format!("{why}{}", cost.said(&remote, ctl.budget)));
    }
    let mut done: Vec<Done> = Vec::with_capacity(steps.len());
    for step in steps {
        let started = now_ms();
        let end = remote.run_step(step, ctl);
        let elapsed = now_ms().saturating_sub(started);
        let judged = judge(
            step,
            end,
            StepMemory::Elsewhere(name.to_string()),
            0,
            ctl.budget,
        );
        let last = judged.verdict.is_some();
        done.push(Done {
            command: step.clone(),
            started_ms: started,
            elapsed_ms: elapsed,
            judged,
        });
        if last {
            break;
        }
    }
    // **Where the time went, on the first row of the log.** A first gate on a host pays for a
    // transport and a build cache that are nobody's code, and a reader looking at a row that says
    // *twenty minutes* has to be able to see which of the two it was.
    if let Some(first) = done.first_mut() {
        let note = cost.line(&remote, ctl.budget);
        first.judged.output = if first.judged.output.is_empty() {
            note
        } else {
            format!("{}\n{note}", first.judged.output.trim_end())
        };
    }
    assemble(steps, done)
}

/// Every step `not_run`, and the reason the gate could not run at all.
fn infra(steps: &[String], why: String) -> GateRun {
    GateRun {
        steps: steps
            .iter()
            .map(|s| letibot_tokencore::store::MergeGateStep {
                command: s.clone(),
                outcome: MergeGateOutcome::NotRun,
                output: String::new(),
                started_ms: 0,
                elapsed_ms: 0,
            })
            .collect(),
        verdict: GateVerdict::Infra(format!(
            "the gate could not run on the declared host: {why}. Nothing was judged — the tip is \
             UNJUDGED, not red, and the queue is not halted by it: it is gated again on a later \
             pass. `[queue] gate_host` in the project's leticode.toml (or `--gate-host`) chooses \
             the machine, and an empty value runs the gate here."
        )),
    }
}

/// **One repository's gate directory on one host** — where the tree, the git store and the build
/// cache live, and the connection they are reached through.
struct Remote {
    ssh: Ssh,
    /// The host's name, for the sentences a row carries.
    name: String,
    /// The landing worktree here, which is what is sent and where the log is written.
    local: PathBuf,
    /// `<root>/gate/<repo>-<hex>`, absolute on that machine — the same `<repo>-<hex>` the local
    /// landing directory uses (`git::landing_dir_in`), so a host and a box name one repository
    /// one way.
    dir: String,
    src: String,
    tree: String,
    target: String,
    run: String,
    /// **Whether this gate is the one that makes the build cache** — asked before `mkdir` touches
    /// it, because a cold gate on a host is a whole dependency tree compiled from nothing and the
    /// row has to say so rather than let a reader call it a slow machine.
    cold: bool,
    /// Free KiB on the filesystem the gate's directory is on, as the host answered at open. A
    /// first gate writes tens of gigabytes (`target/debug` of this workspace measured 35 GB on
    /// lab2x1) and a machine with 6 GiB left is where a link fails with `Bus error`.
    free_kb: Option<u64>,
    seq: AtomicU64,
}

impl Remote {
    fn open(name: &str, worktree: &Path) -> Result<Remote, String> {
        let hosts = Hosts::load();
        let Some(host) = hosts.get(name).cloned() else {
            return Err(hosts.refusal(name));
        };
        let ssh = Ssh::connect(host.clone(), None, Paths::from_env())?;
        // **The root, made and read back as an absolute path.** `expand_root` keeps a `~` for the
        // FAR shell (`"$HOME"/letibot-remote`) — the rake RH1 and RH4 both paid for — and `pwd -P`
        // turns whatever that machine's home is into a path this side can splice anywhere,
        // following symlinks (`/var` → `/private/var` on a Mac, a bind mount on a Linux box).
        let root = expand_root(&host.root);
        let said = call_ok(
            &ssh,
            &format!("making the gate's root `{}` on {name}", host.root),
            &format!("mkdir -p {root} && cd {root} && pwd -P"),
            SETUP_BOUND,
        )?;
        let root = said.trim().to_string();
        if !root.starts_with('/') {
            return Err(format!(
                "`{}` on {name} answered {root:?} rather than an absolute path, so the gate has \
                 nowhere on that machine to put the tree",
                host.root
            ));
        }
        let dir = format!("{}/gate/{}", root.trim_end_matches('/'), dir_name(worktree));
        let src = format!("{dir}/src.git");
        let tree = format!("{dir}/tree");
        let target = format!("{dir}/target");
        let run = format!("{dir}/run");
        // **The cache and the room, asked in the call that makes the directories** — before
        // `mkdir` has made `target`, so a cache this gate creates is `cold`, and `df` of the
        // directory's filesystem answers how much room the build has. One round trip, not two.
        let said = call_ok(
            &ssh,
            &format!("asking what `{}` already has and how much room", dir),
            &format!(
                "if [ -d '{target}' ] && [ -n \"$(ls -A '{target}')\" ]; then echo cache=warm; \
                 else echo cache=cold; fi; \
                 mkdir -p '{dir}' '{run}' '{target}'; \
                 echo \"free=$(df -P -k '{dir}' | tail -1 | awk '{{print $4}}')\""
            ),
            SETUP_BOUND,
        )?;
        // Named fields, not line positions: a login shell is entitled to say something of its own
        // before these lines, and a `motd` read as a cache state is a sentence about the wrong
        // thing.
        let field = |key: &str| -> Option<String> {
            said.lines()
                .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
        };
        let cold = field("cache=").as_deref() != Some("warm");
        let free_kb = field("free=").and_then(|kb| kb.parse::<u64>().ok());
        let r = Remote {
            src,
            tree,
            target,
            run,
            cold,
            free_kb,
            ssh,
            name: name.to_string(),
            local: worktree.to_path_buf(),
            dir: dir.clone(),
            seq: AtomicU64::new(0),
        };
        // **From here the wrapper points at the tree**, because a toolchain is pinned by the
        // `mise.toml`/`.tool-versions` in the tree the work is in — and because `mise exec -C`
        // REFUSES a directory that does not exist, so the directory had to be made first.
        let mut r = r;
        r.ssh = r.ssh.clone().with_dir(r.dir.clone());
        Ok(r)
    }

    fn next(&self) -> usize {
        self.seq.fetch_add(1, Ordering::Relaxed) as usize
    }

    // -- the far side's file names, one per step ------------------------------------------------

    fn launcher_path(&self, n: usize) -> String {
        format!("{}/step-{n}.sh", self.run)
    }
    fn out_path(&self, n: usize) -> String {
        format!("{}/step-{n}.out", self.run)
    }
    fn pid_path(&self, n: usize) -> String {
        format!("{}/step-{n}.pid", self.run)
    }
    fn exit_path(&self, n: usize) -> String {
        format!("{}/step-{n}.exit", self.run)
    }
    /// The marker a poll's answer leads with — unique per run and step, so the step's own output
    /// cannot be mistaken for it and a login shell's chatter before it is skipped rather than
    /// parsed.
    fn tag(&self, n: usize) -> String {
        format!("@@letibot-gate-{}-{n}@@", std::process::id())
    }

    // -- one call, bounded and through the breaker ----------------------------------------------

    fn call(&self, what: &str, script: &str, within: Duration) -> Result<String, String> {
        let argv = self.ssh.argv(&self.ssh.remote_command(script));
        let answer = run(
            what,
            &self.name,
            self.ssh.program(),
            &argv,
            within,
            None,
            true,
        )
        .map_err(|m| m.why)?;
        if answer.code == 0 {
            return Ok(String::from_utf8_lossy(&answer.out).into_owned());
        }
        Err(answer.refusal(what, &self.name))
    }

    /// The same call, with bytes on its stdin — how the launcher and the bundle leave this box
    /// without being spliced into an argv.
    fn feed(&self, what: &str, script: &str, within: Duration, bytes: &[u8]) -> Result<(), String> {
        let argv = self.ssh.argv(&self.ssh.remote_command(script));
        let answer = run(
            what,
            &self.name,
            self.ssh.program(),
            &argv,
            within,
            Some(bytes),
            true,
        )
        .map_err(|m| m.why)?;
        if answer.code == 0 {
            Ok(())
        } else {
            Err(answer.refusal(what, &self.name))
        }
    }

    // -- the tree, on the host --------------------------------------------------------------

    /// **Put the landing tree on the host** — the bundle (the repository's metadata), the
    /// worktree at the tip, and then the tree itself over `rsync`.
    fn prepare(&self, ctl: &GateControl, cost: &mut Cost) -> Result<(), String> {
        let head = git::rev_parse(&self.local, "HEAD").ok_or_else(|| {
            format!(
                "the landing tree {} has no HEAD to send",
                self.local.display()
            )
        })?;
        let branches: Vec<String> = ["main", "master"]
            .iter()
            .filter(|b| git::rev_parse(&self.local, &format!("refs/heads/{b}")).is_some())
            .map(|b| b.to_string())
            .collect();
        self.check(ctl)?;
        // What the host already has, as the bundle's prerequisites — its own refs are the record,
        // so there is no stamp file to fall out of step with the repository.
        let began = Instant::now();
        let have = self.call(
            &format!("asking what {} already has", self.name),
            &format!(
                "mkdir -p '{}' && [ -d '{}' ] || git init -q --bare '{}'; \
                 git -C '{}' for-each-ref --format='%(objectname)' refs/gate refs/heads 2>/dev/null",
                self.dir, self.src, self.src, self.src
            ),
            SETUP_BOUND,
        )?;
        let have: Vec<&str> = have
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !have.contains(&head.as_str()) {
            self.send_bundle(&head, &branches, &have, ctl)?;
        }
        cost.repo_ms = Some(began.elapsed().as_millis() as u64);
        self.check(ctl)?;
        let began = Instant::now();
        self.call(
            &format!("putting the landing tree at {head} on {}", self.name),
            &worktree_script(&self.src, &self.tree, &head),
            SETUP_BOUND,
        )?;
        cost.checkout_ms = Some(began.elapsed().as_millis() as u64);
        self.check(ctl)?;
        let began = Instant::now();
        self.rsync(ctl)?;
        cost.tree_ms = Some(began.elapsed().as_millis() as u64);
        Ok(())
    }

    /// The tip and the target branch as one (usually thin) bundle, sent on the connection's stdin.
    fn send_bundle(
        &self,
        head: &str,
        branches: &[String],
        have: &[&str],
        ctl: &GateControl,
    ) -> Result<(), String> {
        let local = std::env::temp_dir().join(format!(
            "letibot-gate-bundle-{}-{}.bundle",
            std::process::id(),
            self.next()
        ));
        let mut args: Vec<String> = vec![
            "-C".into(),
            self.local.display().to_string(),
            "bundle".into(),
            "create".into(),
            local.display().to_string(),
            "HEAD".into(),
        ];
        args.extend(branches.iter().cloned());
        if !have.is_empty() {
            args.push("--not".into());
            args.extend(have.iter().map(|s| s.to_string()));
        }
        let made = std::process::Command::new("git")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("running git bundle: {e}"))?;
        let bytes = if made.status.success() {
            std::fs::read(&local).unwrap_or_default()
        } else {
            Vec::new()
        };
        if bytes.is_empty() {
            let _ = std::fs::remove_file(&local);
            return Err(format!(
                "git bundle produced nothing for {head} on {}: {}",
                self.name,
                String::from_utf8_lossy(&made.stderr).trim()
            ));
        }
        // **The refspecs are the BUNDLE's heads, not the ones this side asked for.** A thin bundle
        // whose prerequisite already covers a branch drops that ref (measured: the third gate of
        // one repository asked for `refs/heads/main` and `git fetch` answered *couldn't find remote
        // ref*), so what to fetch is read off the bundle rather than assumed.
        let refspecs = bundle_refspecs(&local, head)?;
        let _ = std::fs::remove_file(&local);
        self.check(ctl)?;
        let remote = format!("{}/in.bundle", self.dir);
        self.feed(
            &format!("sending {head} to {} ({} bytes)", self.name, bytes.len()),
            &format!("cat > '{remote}'"),
            TRANSFER_BOUND,
            &bytes,
        )?;
        self.call(
            &format!("storing {head} on {}", self.name),
            &format!(
                "git -C '{}' fetch --no-tags '{}' {} && rm -f '{}'",
                self.src,
                remote,
                refspecs.iter().map(|r| sq(r)).collect::<Vec<_>>().join(" "),
                remote
            ),
            TRANSFER_BOUND,
        )?;
        Ok(())
    }

    /// **The landing tree, over the host's declared key and root.**
    fn rsync(&self, ctl: &GateControl) -> Result<(), String> {
        self.check(ctl)?;
        let argv = rsync_argv(&self.ssh, &self.local, &self.tree);
        let what = "sending the landing tree".to_string();
        let answer = run(
            &what,
            &self.name,
            Path::new("rsync"),
            &argv,
            TRANSFER_BOUND,
            None,
            true,
        )
        .map_err(|m| m.why)?;
        if answer.code == 0 {
            return Ok(());
        }
        Err(format!(
            "{} — nothing was run there",
            answer.refusal(&what, &self.name)
        ))
    }

    /// The deadline, the daemon's stop flag and the lease, asked between the calls that arrange
    /// things — a stop must not wait out a `git fetch`.
    fn check(&self, ctl: &GateControl) -> Result<(), String> {
        if Instant::now() >= ctl.deadline {
            return Err("the gate's deadline passed before the tree was on the host".into());
        }
        if ctl.stop.load(Ordering::Relaxed) {
            return Err("the daemon stopped before the tree was on the host".into());
        }
        if !(ctl.alive)() {
            return Err(
                "this daemon lost the repository's lease before the tree was on the host, and a \
                 daemon that does not own the repository may not land into it"
                    .into(),
            );
        }
        Ok(())
    }

    // -- one step ---------------------------------------------------------------------------

    /// **One step on the host**: start it detached, poll it, and kill its group if it outlives the
    /// gate. The log arrives at the local path the local gate would have written, so the pane and
    /// the failure excerpt do not change.
    fn run_step(&self, step: &str, ctl: &GateControl) -> StepEnd {
        let n = self.next();
        let log = match tempfile_in(&self.local) {
            Ok(l) => l,
            Err(e) => return StepEnd::Infra(e),
        };
        if let Err(e) = std::fs::File::create(&log) {
            return StepEnd::Infra(format!("{}: {e}", log.display()));
        }
        let end = self.run_step_into(n, step, &log, ctl);
        let _ = std::fs::remove_file(&log);
        end
    }

    fn run_step_into(&self, n: usize, step: &str, log: &Path, ctl: &GateControl) -> StepEnd {
        let launcher = launcher_script(
            step,
            &self.tree,
            &self.target,
            &self.pid_path(n),
            &self.out_path(n),
            &self.exit_path(n),
        );
        let script = self.launcher_path(n);
        if let Err(why) = self.feed(
            &format!("writing `{step}`'s launcher on {}", self.name),
            &format!("cat > '{script}'"),
            SETUP_BOUND,
            launcher.as_bytes(),
        ) {
            return StepEnd::Infra(why);
        }
        if let Err(why) = self.call(
            &format!("starting `{step}` on {}", self.name),
            &launch_script(&script, &self.exit_path(n)),
            SETUP_BOUND,
        ) {
            return StepEnd::Infra(why);
        }
        let mut offset: u64 = 0;
        let mut last_beat = Instant::now();
        // **A poll that was not answered is a question to ask again, not a step to kill** — the
        // 2026-10-30 halt, in one line: a first gate on lab2x1 ran `cargo test --workspace` on 25
        // of its 32 cores, one poll took longer than the ten seconds a poll was once allowed, and
        // a step that had been running for four minutes was killed and reported as *could not
        // run*. What a miss says is *the machine was busy*; what ends a step is its own deadline.
        let mut missed: u32 = 0;
        let mut last_miss = String::new();
        let mut last_within = POLL_BOUND;
        loop {
            if Instant::now() >= ctl.deadline {
                self.kill(n);
                return StepEnd::Deadline(said_with_misses(
                    read_tail(log),
                    missed,
                    &last_miss,
                    last_within,
                ));
            }
            if ctl.stop.load(Ordering::Relaxed) {
                self.kill(n);
                return StepEnd::Stopped(read_tail(log), "the daemon is stopping".into());
            }
            if last_beat.elapsed() >= Duration::from_secs(5) {
                last_beat = Instant::now();
                if !(ctl.alive)() {
                    self.kill(n);
                    return StepEnd::Stopped(
                        read_tail(log),
                        "this daemon lost the repository's lease, so it may not land".into(),
                    );
                }
            }
            // A poll never outlives the gate: the last one before the deadline is bounded by what
            // is left of it, so the verdict arrives when the deadline says and not a bound later.
            let within = POLL_BOUND.min(ctl.deadline.saturating_duration_since(Instant::now()));
            last_within = within;
            match self.poll(n, offset, within) {
                Ok(poll) => {
                    missed = 0;
                    if !poll.bytes.is_empty() {
                        offset += poll.bytes.len() as u64;
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .append(true)
                            .create(true)
                            .open(log)
                        {
                            let _ = f.write_all(&poll.bytes);
                        }
                    }
                    if let Some(code) = poll.exit {
                        let tail = read_tail(log);
                        return ended(code, tail);
                    }
                }
                Err(miss) if miss.unreached => {
                    // **No machine answered, so there is nothing to wait for.** This is not a
                    // slow host: ssh could not dial it, or the breaker has already decided it is
                    // down after its own three failures. The group is killed here and the step is
                    // UNJUDGED — never red, because nothing about the code was read.
                    self.kill(n);
                    return StepEnd::Infra(format!(
                        "{}: `{step}` was started on {} and its process group was killed here. \
                         What it had written is above, and nothing was judged about the code",
                        miss.why, self.name
                    ));
                }
                Err(miss) => {
                    missed += 1;
                    last_miss = miss.why;
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    }

    /// **How the step is**, and whatever it has written since `offset` — one round trip, so a
    /// step's progress and its state cannot disagree about which moment they describe.
    ///
    /// `within` is [`POLL_BOUND`] or what is left of the gate, whichever is less; a miss comes back
    /// as [`Missed`], which says whether the machine was reached at all, because *busy* and *gone*
    /// are two different verdicts and only one of them ends the step.
    fn poll(&self, n: usize, offset: u64, within: Duration) -> Result<Poll, Missed> {
        let tag = self.tag(n);
        let script = poll_script(
            &tag,
            &self.pid_path(n),
            &self.exit_path(n),
            &self.out_path(n),
            offset,
        );
        let argv = self.ssh.argv(&self.ssh.remote_command(&script));
        let what = format!("asking how `{}` is doing", self.name);
        let answer = run_poll(&what, &self.name, self.ssh.program(), &argv, within)?;
        if answer.code == SSH_TROUBLE {
            return Err(Missed::gone(answer.refusal(&what, &self.name)));
        }
        if answer.code != 0 && answer.out.is_empty() {
            return Err(Missed::gone(answer.refusal(&what, &self.name)));
        }
        let at = find(&answer.out, tag.as_bytes()).ok_or_else(|| {
            Missed::gone(format!(
                "{} answered without this gate's marker: {}",
                self.name,
                tail(&String::from_utf8_lossy(&answer.out), 400)
            ))
        })?;
        let rest = &answer.out[at + tag.len()..];
        let nl = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
        let line = String::from_utf8_lossy(&rest[..nl]).into_owned();
        let mut fields = line.split_whitespace();
        let _alive = fields.next();
        let exit = fields.next().and_then(|e| e.parse::<i32>().ok());
        let bytes = if nl < rest.len() {
            rest[nl + 1..].to_vec()
        } else {
            Vec::new()
        };
        Ok(Poll { exit, bytes })
    }

    /// **`SIGTERM` to the far side's group, five seconds, then `SIGKILL`** — the local gate's own
    /// discipline, one machine over. `kill -TERM -<pid>` is the group because the launcher is
    /// `setsid`'d; where a host has no `setsid` the pid is only the launcher, so the plain kill is
    /// the fallback and the sentence is not.
    fn kill(&self, n: usize) {
        let script = kill_script(&self.pid_path(n));
        let _ = self.call(
            &format!("ending the gate's step on {}", self.name),
            &script,
            KILL_BOUND,
        );
    }
}

/// **What one poll answered**: the step's exit code when it has one, and the bytes it wrote since
/// the last poll.
struct Poll {
    exit: Option<i32>,
    bytes: Vec<u8>,
}

/// **Why a call did not answer**, and whether the far side was reached at all — the same words to
/// a person and two different verdicts to the gate.
#[derive(Debug)]
struct Missed {
    why: String,
    /// `true` when nothing reached the machine: ssh could not dial it, the breaker has already
    /// decided it is down, or the far side ran nothing. `false` when the question was sent and this
    /// box's own clock ran out first — a machine too busy to answer, which is asked again.
    unreached: bool,
}

impl Missed {
    /// The call was made and no answer came back in time — ask again.
    fn slow(why: String) -> Missed {
        Missed {
            why,
            unreached: false,
        }
    }
    /// No machine answered — there is nothing to wait for.
    fn gone(why: String) -> Missed {
        Missed {
            why,
            unreached: true,
        }
    }
}

/// **Where a first gate's time went** — the three phases before the first step, measured, plus what
/// the host's own build cache was. A cold cache is a whole dependency tree compiled inside the
/// steps, and a reader who cannot see that reads a slow build as a slow machine.
#[derive(Default)]
struct Cost {
    repo_ms: Option<u64>,
    checkout_ms: Option<u64>,
    tree_ms: Option<u64>,
}

impl Cost {
    /// One line for the gate's log: what arrived, how long it took, and what the host had room for.
    ///
    /// `budget` is the gate's own `[queue]` `gate_deadline`, said beside the transport so a reader
    /// can tell *the tree took two seconds of a half hour* from *the tree took most of it* — a
    /// first gate on a host pays the transport out of the same clock its steps run on.
    fn line(&self, r: &Remote, budget: Duration) -> String {
        let at = |v: Option<u64>| match v {
            Some(ms) => format!("{:.2} s", ms as f64 / 1000.0),
            None => "not reached".to_string(),
        };
        let transport = self.repo_ms.unwrap_or(0) + self.tree_ms.unwrap_or(0);
        let cache = if r.cold {
            "COLD — this gate made it, so the first step below pays for the whole dependency tree"
        } else {
            "warm"
        };
        format!(
            "[gate on {}: the repository {}, the checkout {}, the tree {} — {:.2} s of transport \
             out of the gate's {}; build cache {cache}; {}]",
            r.name,
            at(self.repo_ms),
            at(self.checkout_ms),
            at(self.tree_ms),
            transport as f64 / 1000.0,
            clause_of(budget),
            free_words(r.free_kb),
        )
    }

    /// The same line as a paragraph on a failure, or nothing at all when no phase was measured.
    fn said(&self, r: &Remote, budget: Duration) -> String {
        if self.repo_ms.is_none() && self.checkout_ms.is_none() && self.tree_ms.is_none() {
            return String::new();
        }
        format!("\n\n{}", self.line(r, budget))
    }
}

/// Free space as a sentence says it.
fn free_words(kb: Option<u64>) -> String {
    match kb {
        Some(kb) if kb >= 1024 * 1024 => {
            format!(
                "{:.1} GiB free on that filesystem",
                kb as f64 / (1024.0 * 1024.0)
            )
        }
        Some(kb) => format!("{:.1} MiB free on that filesystem", kb as f64 / 1024.0),
        None => "free space unknown there".to_string(),
    }
}

/// **The step's tail, and what the gate was doing when its deadline came** — a verdict has to say
/// that the last minutes of it were spent asking a question nobody answered.
///
/// `bound` is the bound the LAST poll was given, which is [`POLL_BOUND`] or what was left of the
/// gate when it was asked — the sentence names the clock that actually ran out, because *not
/// answered within 30 s* and *not answered within the last 4 s of the gate* are different facts
/// about a machine.
fn said_with_misses(tail: String, missed: u32, last: &str, bound: Duration) -> String {
    if missed == 0 {
        return tail;
    }
    format!(
        "{tail}\n\n[gate: {missed} poll(s) in a row were not answered within {} s while this step \
         ran — the last said: {last}]",
        bound.as_secs()
    )
}

/// A far-side step's exit status as [`StepExit`] — and the difference matters, which is why this
/// is a function with a test rather than a `Code(n)`.
///
/// A shell reports *my child was killed by signal n* as `128 + n`, and the local gate waits on its
/// own child so it sees the signal itself ([`StepExit::of`]). If the host's `137` were read as an
/// exit code, a step killed by `SIGKILL` — a `timeout`, a reboot's `SIGTERM`, an OOM killer — would
/// be `RedCause::Code` and the branch would be called RED for a death that judged nothing. The
/// range is signals 1–64, the whole of what Linux defines.
pub fn exit_of(code: i32) -> StepExit {
    if (129..=192).contains(&code) {
        StepExit::Signal(code - 128)
    } else {
        StepExit::Code(code)
    }
}

fn ended(code: i32, tail: String) -> StepEnd {
    match exit_of(code) {
        StepExit::Code(0) => StepEnd::Ok(tail),
        other => StepEnd::Red(tail, other),
    }
}

// ---------------------------------------------------------------------------------------------
// The far side's scripts, as values
// ---------------------------------------------------------------------------------------------

/// **The launcher** — one file per step, so the step's own text needs no quoting: it is written
/// into a file this box sends over the connection's stdin, never spliced into an argv.
///
/// `( … )` is a subshell and not `{ … }`, which is R99's measured trap: a brace group runs in the
/// launcher's own shell, so a step containing `exit` would end the launcher before it could write
/// the status and the step would read as *died without saying*.
///
/// `CARGO_TARGET_DIR` is the host's warm cache, **outside** both the worktree and the rsync'd tree,
/// so a `git clean -fd` or an `rsync --delete` cannot take it with them.
pub fn launcher_script(
    step: &str,
    tree: &str,
    target: &str,
    pid: &str,
    out: &str,
    exit: &str,
) -> String {
    format!(
        "#!/bin/bash\n\
         echo $$ > '{pid}'\n\
         cd '{tree}' || exit 97\n\
         export CARGO_TARGET_DIR='{target}'\n\
         ( {step} ) > '{out}' 2>&1\n\
         echo $? > '{exit}'\n"
    )
}

/// **Start it and go** — detached, `setsid` where the host has it (so its pid is its process group
/// and [`kill_script`] reaches everything it forked), all three streams on `/dev/null` so the ssh
/// call returns rather than waiting for the step.
pub fn launch_script(script: &str, exit: &str) -> String {
    format!(
        "rm -f '{exit}'; \
         if command -v setsid >/dev/null 2>&1; then setsid bash '{script}' </dev/null >/dev/null 2>&1 & \
         else nohup bash '{script}' </dev/null >/dev/null 2>&1 & fi; echo started"
    )
}

/// **How the step is, and what it has written since `offset`** — the marker first, so a login
/// shell's own chatter before it is skipped, and the bytes after it are the step's, untouched.
///
/// The two reads are ordered the way `a-finishing-job-and-a-killed-job-answer-the-same-poll`
/// says: the status file FIRST, then the pid. A step that finished between the two reads answers
/// *exit 0, pid gone*, which is a step that finished — the other order answers *pid gone, no
/// status*, which is what a kill looks like.
pub fn poll_script(tag: &str, pid: &str, exit: &str, out: &str, offset: u64) -> String {
    format!(
        "e=; if [ -f '{exit}' ]; then e=$(cat '{exit}' 2>/dev/null); fi; \
         a=no; if [ -f '{pid}' ]; then p=$(cat '{pid}' 2>/dev/null); \
         [ -n \"$p\" ] && kill -0 \"$p\" 2>/dev/null && a=yes; fi; \
         printf '{tag} %s %s\\n' \"$a\" \"$e\"; \
         tail -c +{} '{out}' 2>/dev/null",
        offset + 1
    )
}

/// **End the step's group**: `SIGTERM`, up to five seconds, then `SIGKILL` — the local gate's own
/// numbers. A host without `setsid` started the step as its own process, so the plain kill is the
/// fallback rather than a silently useless `-<pid>`.
pub fn kill_script(pid: &str) -> String {
    format!(
        "p=$(cat '{pid}' 2>/dev/null); \
         if [ -n \"$p\" ]; then \
           kill -TERM -\"$p\" 2>/dev/null || kill -TERM \"$p\" 2>/dev/null; \
           i=0; while kill -0 \"$p\" 2>/dev/null && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done; \
           kill -KILL -\"$p\" 2>/dev/null || kill -KILL \"$p\" 2>/dev/null; \
           echo \"ended $p\"; \
         else echo 'no pid file, so nothing to end'; fi"
    )
}

/// **Put the host's worktree at `head`** — the same three commands the local landing worktree gets
/// (`checkout --detach --force`, `reset --hard`, `clean -fd`), so a gate never judges a file the
/// previous one left behind.
///
/// **`-e`, not `-d`.** A git worktree's `.git` is a FILE (`gitdir: …`), so `[ -d tree/.git ]` is
/// false for every worktree that exists — measured as a second gate of the same repository dying
/// on `fatal: '<tree>' already exists`, which is a host gate that could only ever run once.
pub fn worktree_script(src: &str, tree: &str, head: &str) -> String {
    format!(
        "if [ -e '{tree}/.git' ]; then \
           cd '{tree}' && git checkout --detach --force '{head}' && git reset --hard '{head}' && \
           git clean -fd; \
         else git -C '{src}' worktree add --detach '{tree}' '{head}'; fi"
    )
}

/// **The `rsync` invocation, as a value** — the brief's transport, and the one thing about it a
/// test can assert without a second machine.
///
/// * `-a` keeps modes and mtimes (a script's `+x` matters: the gate runs `sh scripts/…`).
/// * `--delete` so a file the branch REMOVED is gone there too — a stale file is a gate that
///   passes on something the branch no longer has.
/// * `/.git` is excluded because in a landing worktree it is a *file* naming a path on this box,
///   and overwriting the host's would break the repository the bundle just made.
/// * `/target` is the local build cache: gigabytes, and the host has its own.
/// * `-e` carries the host's declared key and flags, because that is the whole of how this box is
///   allowed to reach the machine.
///
/// **The destination path is not quoted here**: `rsync` hands its own remote command to the shell
/// with the arguments protected (`protect_args`, on by default since 3.0), and quoting it as well
/// measured as a literal `\'…\'` on the far side — *mkdir … failed: No such file or directory*, for
/// a directory whose name had two backslashes in it.
pub fn rsync_argv(ssh: &Ssh, src: &Path, dest: &str) -> Vec<String> {
    let mut rsh = sq(&ssh.program().display().to_string());
    for flag in ssh.flags() {
        rsh.push(' ');
        rsh.push_str(&sq(&flag));
    }
    vec![
        "-a".to_string(),
        "--delete".to_string(),
        "--exclude=/.git".to_string(),
        "--exclude=/target".to_string(),
        "-e".to_string(),
        rsh,
        format!("{}/", src.display()),
        format!("{}:{}", ssh.destination(), dest),
    ]
}

/// **`<repo>-<hex>`** — the same name the local landing directory has (`git::landing_dir_in`), so
/// one repository is spelled one way on both machines.
pub fn dir_name(worktree: &Path) -> String {
    git::landing_dir_in(Path::new("."), worktree)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}

/// **The refspecs a bundle can actually be fetched by** — its own heads, with `HEAD` renamed to
/// the ref the host's worktree is put at.
///
/// `git bundle list-heads <file>` is one line per ref the bundle carries: `<sha> <refname>`.
/// A branch whose tip is already on the host (a prerequisite) is not in that list, and asking for
/// it is the fetch that fails with *couldn't find remote ref*.
fn bundle_refspecs(bundle: &Path, head: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .args(["bundle", "list-heads"])
        .arg(bundle)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("running git bundle list-heads: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git bundle list-heads {}: {}",
            bundle.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut specs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((_, name)) = line.split_once(' ') else {
            continue;
        };
        if name == "HEAD" {
            specs.push("+HEAD:refs/gate/tip".to_string());
        } else {
            specs.push(format!("+{name}:{name}"));
        }
    }
    if !specs.iter().any(|s| s.ends_with("refs/gate/tip")) {
        // No `HEAD` in the bundle: the tip itself is a prerequisite, which `prepare` has already
        // ruled out — but a ref the worktree can be put at is still needed, and the commit is
        // there, so name it by its sha.
        specs.push(format!("+{head}:refs/gate/tip"));
    }
    Ok(specs)
}

/// One shell word, quoted.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The end of `text`, at most `cap` bytes — [`super::gate::tail`]'s rule, for a sentence.
fn tail(text: &str, cap: usize) -> String {
    super::gate::tail(text, cap)
}

// ---------------------------------------------------------------------------------------------
// One bounded call
// ---------------------------------------------------------------------------------------------

/// One bounded call's answer, as the far side gave it — `Debug` because a test that expects a miss
/// prints the answer it got instead, and a panic saying `Answer` is not much of a sentence.
#[derive(Debug)]
struct Answer {
    out: Vec<u8>,
    err: Vec<u8>,
    code: i32,
}

impl Answer {
    /// **What a failed call is told** — the far side's own words when it had any, and a sentence
    /// about the machine when the failure was the connection rather than the command.
    fn refusal(&self, what: &str, host: &str) -> String {
        let said = if self.err.is_empty() {
            String::from_utf8_lossy(&self.out).trim().to_string()
        } else {
            String::from_utf8_lossy(&self.err).trim().to_string()
        };
        let said = if said.is_empty() {
            "it said nothing".to_string()
        } else {
            tail(&said, 800)
        };
        if self.code == SSH_TROUBLE {
            format!(
                "{what} did not reach {host}: ssh exited {SSH_TROUBLE}, which is a connection or \
                 authentication failure and never a remote command's own status — {said}"
            )
        } else {
            format!("{what} on {host}: exit {}: {said}", self.code)
        }
    }
}

/// **One bounded call through the breaker** — the discipline `docs/remote-hosts.md` states for
/// every call this box makes to a declared host, reused rather than restated: refuse at once when
/// the host is known down, take a slot in the per-host queue, kill the local process group at the
/// deadline, and tell the breaker whether the far side said anything.
///
/// `report` is [`rpc::record`]'s switch, and a poll is the one caller that says no:
/// [`rpc::RECORD_RING`] is 256 entries, so a 30-minute step's 1800 successful polls would evict
/// every call a reader could act on — the same rule [`RemoteProcesses`] follows for a job's
/// heartbeat.
fn run(
    what: &str,
    host: &str,
    program: &Path,
    argv: &[String],
    within: Duration,
    fed: Option<&[u8]>,
    report: bool,
) -> Result<Answer, Missed> {
    let pass = rpc::before(host).map_err(|why| Missed::gone(format!("{what}: {why}")))?;
    let began = Instant::now();
    let ended = {
        let _slot =
            rpc::enter(host, within).map_err(|why| Missed::slow(format!("{what}: {why}")))?;
        match fed {
            Some(bytes) => rpc::run_bounded_fed(program, argv, within, bytes),
            None => rpc::run_bounded(program, argv, within),
        }
    };
    let outcome = match &ended {
        rpc::Ended::Answered(_) => rpc::Outcome::Ok,
        rpc::Ended::TimedOut => rpc::Outcome::TimedOut,
        rpc::Ended::Unstartable(_) => rpc::Outcome::Unknown,
    };
    let answered = match &ended {
        rpc::Ended::Answered(out) => out.status.code() != Some(SSH_TROUBLE),
        _ => false,
    };
    rpc::after(host, pass, answered);
    if report {
        rpc::record(host, what, began.elapsed(), outcome);
    }
    match ended {
        rpc::Ended::Answered(out) => Ok(Answer {
            out: out.stdout,
            err: out.stderr,
            code: out.status.code().unwrap_or(-1),
        }),
        // **The clock, not the machine.** This is the one failure that says nothing about whether
        // the host is there — the question was sent and this box gave up waiting — so it comes back
        // as a miss a caller may ask again rather than as an unreachable host.
        rpc::Ended::TimedOut => Err(Missed::slow(format!(
            "{what} on {host} did not answer within {} s and the ssh this daemon started was \
             killed with its whole process group",
            within.as_secs()
        ))),
        rpc::Ended::Unstartable(e) => Err(Missed::gone(format!("{what}: {e}"))),
    }
}

/// A poll — [`run`] with the record ring left alone, because a heartbeat is not news.
fn run_poll(
    what: &str,
    host: &str,
    program: &Path,
    argv: &[String],
    within: Duration,
) -> Result<Answer, Missed> {
    run(what, host, program, argv, within, None, false)
}

/// The same call, requiring success — the arranging steps (`mkdir`, a `git fetch`), which have no
/// useful non-zero answer.
fn call_ok(ssh: &Ssh, what: &str, script: &str, within: Duration) -> Result<String, String> {
    let argv = ssh.argv(&ssh.remote_command(script));
    let answer = run(
        what,
        &ssh.host().name,
        ssh.program(),
        &argv,
        within,
        None,
        true,
    )
    .map_err(|m| m.why)?;
    if answer.code == 0 {
        Ok(String::from_utf8_lossy(&answer.out).into_owned())
    } else {
        Err(answer.refusal(what, &ssh.host().name))
    }
}

#[cfg(test)]
mod tests;
