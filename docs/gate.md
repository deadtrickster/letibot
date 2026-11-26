# The gate

A repository declares its gate in its `AGENTS.md`, as fenced blocks of one shell command per line.
A project can declare **two tiers**:

- `## Quick gate` - what a branch must pass to LAND, with the review.
- `## Full gate` - what `main`'s own health is judged by, after the landings.

A project with one `## Merge gate` section is unchanged: that section is both tiers, so a branch is
gated by it before landing and a tip nobody landed is gated by it when it moves. The sections are
read from `main`'s copy, so a branch cannot weaken the gate it is judged by. A repository that
declares none of the three has no gate, and the queue holds every branch of it until somebody writes
one (`merge_gate` asks the operator what it should be).

## Two tiers: what lands, and what main's health is judged by

Review stays per branch and so does the landing: an approved branch is rebased onto the target's tip
in the queue's own landing worktree and the **quick** gate runs there. Green: the landing is pushed,
the install steps run, and the branch is on `main`. Red: the branch is parked `failed`, as ever.

`main`'s health is the **full** gate's, and it is run nowhere near a landing:

- **It is batched.** One run covers every landing since the last tip a full gate judged, and it
  starts when the queue has nothing left to land, so a busy queue keeps landing and main is judged
  in the gaps. Landings an hour apart are one run each; five in ten minutes are one run over all
  five.
- **It yields the box.** The run is in the gate's one slot, so it never runs beside a landing's
  gate; on a declared host it is started `nice -n 10` inside a scope weighted a fifth of systemd's
  default CPU and IO weight, and on this box it is a cgroup of its own (memory-capped) with the
  box's gate lock keeping it the only gate running.
- **While the full gate is owed, the tip is green.** Its row says so in as many words - *the full
  gate is owed since `<sha>`* - because nothing about that tip halts the queue. Landings go on.
- **Red on a batch is bisected.** The step that failed is re-run, alone, at the midpoints of the
  batch (`git rev-list base..tip`'s middle, then the middle of the half that answers), so the first
  red landing is found in `log2(n)` runs of one step. That landing is reverted on `main` with a
  commit of its own, `revert: <subject> - full gate red at <sha>`, pushed as a landing is pushed, and
  the row it landed as goes `failed`. The session that landed it is woken with the failing step and
  the end of its output - the same door a gate's own verdict uses.
- **A red no landing caused is parked.** If the commit that broke `main` is not one the queue landed
  (somebody pushed it), or every landing passes the step when re-run on its own, nothing is
  reverted: the target goes `red` and says a person has to act, exactly as a red tip always has.
  The same park follows a red that comes after a run nobody could judge (a host that did not answer,
  a run that was stopped): the batch's base is what the last *judged* tip recorded, and a run that
  judged nothing does not carry one.
- **Red `main` halts nothing but new landings onto it**, which is the rule it has always been: an
  approved branch goes back to review, a fix lands alone, and reviews go on.

The full gate's tree is the target's tip, so a step that only makes sense on main (a deploy check,
a smoke test against the built binaries) belongs there rather than in the quick gate.

## What every step is told: `LETIBOT_GATE_BASE` and `LETIBOT_GATE_CHANGED`

Every step of either tier runs with two facts about the run in its environment, so a project's step
can aim itself instead of running the world:

- `LETIBOT_GATE_BASE` - the commit this run is compared against: for a landing, the target tip the
  branch was rebased onto; for the full gate, the last tip anybody judged; for a bisection's own
  re-run, the commit before the midpoint.
- `LETIBOT_GATE_CHANGED` - the path of a file listing the paths this run changes, one per line, as
  `git diff --name-only <base> <tip>` prints them. A file rather than a variable because a file is
  what every tool can read, and it is always written: an empty file means *this run changed
  nothing*, where an unset variable could only mean *nobody worked it out*.

A step that ignores both is a step that runs the world, which is what a gate did before.

```sh
# Rust: the crates the change is in, and everything that depends on them.
crates=$(sed -n 's#^crates/\([^/]*\)/.*#\1#p' "$LETIBOT_GATE_CHANGED" | sort -u)
expr=$(for c in $crates; do printf 'rdeps(letibot-%s) + ' "$c"; done)
cargo nextest run --profile gate -E "${expr% + }"

# Go: the packages the change is in, and their importers.
go test $(sed -n 's#^\([^/]*/[^/]*\).*#\1#p' "$LETIBOT_GATE_CHANGED" | sort -u | sed 's#^#./#')...

# Python: the tests under the packages that changed.
pytest $(sed -n 's#^\(src/[^/]*\)/.*#\1#p' "$LETIBOT_GATE_CHANGED" | sort -u | sed 's#^src/#tests/#') \
  || pytest

# Shell: the suite for the scripts that changed, and only those.
for f in $(grep '^scripts/' "$LETIBOT_GATE_CHANGED"); do sh "$f" --self-test; done
```

This repository's own quick test step is `scripts/quick-tests.sh`: it maps the paths to crate names
and runs `cargo nextest run --profile gate -E 'rdeps(<crate>) + …'`. `sh scripts/quick-tests.sh
--print` prints the expression without running it. A change to a manifest or the nextest profile
runs everything; a change that is only prose runs nothing and says so.

## Who runs it, and when

The merge queue, on an approved branch, after rebasing it onto the target's tip. It runs in a
landing worktree of its own (`~/.local/share/letibot/landing/<repo>-<hash>`, never inside `.git`)
with `CARGO_TARGET_DIR` pointed at that worktree's `target`, so a branch is never judged with the
main tree's artifacts and nothing it builds reaches the main tree's `target/release` before the
install step.

- The first step that fails parks the branch. The row names the step and carries its output.
- The target's own tip is judged by the full tier on the same terms - in that landing worktree, with
  the same deadline and the same rows - one batched run at a time (above).
- A gate on this machine first takes the box's gate lock, the one `scripts/gate-at.sh` takes
  (`/build/gate.lock`, or `LETIBOT_GATE_LOCK`), and holds it to its last step.
- The daemon pokes a step every 5 s and the step's own launcher watches for the pokes. A step that
  is not poked for four of them (20 s) ends itself, with its whole process group: however the
  daemon went - a stop, a crash, `SIGKILL`, a box that rebooted or slept - the step goes with it,
  and nothing is left for the next daemon to find. Nothing calls back the other way, so a gate
  machine with no egress (an AWS cluster driven from a laptop) is watched the same way. The poke
  rides the poll that fetches the step's output, so a step on a declared host is asked once every
  5 s and not every second.
- Nothing is written down about a running step and nothing reaps one: the watchdog is the whole
  of it (`crates/harnessd/src/mergequeue/gate_tick.rs`), and the record-and-reaper that used to
  catch a dead daemon's step (`mergequeue/gate_orphans.rs`) is gone.
- One deadline covers the whole run, not each step, counted from when the lock is held: thirty
  minutes, or `[queue] gate_deadline_minutes`. Past it the step in flight is killed with its
  process group and the row is `timed_out`. Each of a bisection's re-runs is a run of its own, with
  the deadline.
- A step named `install-gate-build` runs only after the landing was pushed, and it is the landing's:
  the queue splits a tier's install steps out of its list and runs them after the push, so a
  two-tier project declares the one it wants a landing to run (`Quick gate`). It refuses when the
  build it was told about is the directory the launcher already starts from, and when that build
  is older than the newest `harnessd` it can see: both name `CARGO_TARGET_DIR`, because a step
  that lost it used to copy every binary onto itself and still say installed.
- With rows in line the queue fetches `origin`'s target (at most every 30 s) and fast-forwards the
  local target and its checkouts when origin is ahead. A push refused because origin moved since
  puts the local target back at origin's tip and requeues the row, which is rebased and gated
  again. `landed_local` is left for a push that fails with origin unreachable.

A branch is gated when it is ready to land, not on every save. `queue status` says where a row is,
and a green line names the commit and the machine that judged it.

A session that asked for a gate is woken with its answer instead of polling for it. Putting a
branch up (`enqueue`), putting a parked row back in line (`restart`) and re-gating a target
(`regate`) all write down the asking session; when that gate reaches a verdict the session gets one
row, through the same door a finished job uses, so an idle session starts a turn and a busy one
reads it at its next boundary:

```text
[queue] the gate of row `r-abc` at 1a2b3c4 is failed: <the first line of the why>; queue status for the evidence
```

One row per verdict: a `restart` is what asks for the next one. A session that has ended is
skipped, and a verdict nobody is waiting on is the pane's alone. A Claude Code seat needs no wait
of its own: the bridge pushes every move of the queue it follows to its channel
([claude-code-bridge.md](claude-code-bridge.md), *The queue, from the orchestrator's side*).

## Where it runs

On this machine, or on a host named in the project's `leticode.toml`:

```toml
[queue]
gate_host = "lubuntu1"    # a [host.…] from ~/.config/letibot/hosts.toml; empty is this machine
```

`--gate-host NAME` on the daemon's command line wins over the file, and `--gate-host local` puts it
back here. The name is read once at daemon start. The landing worktree and the target branch stay
here; a host only runs the steps, in a copy of the tree ([remote-hosts.md](remote-hosts.md), *The
merge gate on a declared host*).

`gate_in = "firecode"` in the same table runs each step in a firecode VM on the gate machine: on a
Mac `firecode sh -c` (project mounted read-only, nothing copied back), elsewhere `firecode run`
(files sent in, nothing copied back). Deadline, logs and verdicts are the ordinary gate's. A machine
without a working firecode leaves the tip unjudged with a sentence; the steps never fall back to the
host. The default is `gate_in = "host"`.

A step that runs a `[boundary] firecode_only` program always runs in firecode, whatever `gate_in`
says. When the gate host cannot start a VM, the steps run in this machine's VM instead.

`~/.config/letibot/hosts.toml` is the operator's and never in a repository. A `[host.NAME]` table
has `hostname`, `user`, `key` (the identity file ssh is given, never read into a session or a log),
`root`, `path`, `reserve`, `flags`, `mise`, `never_touch`, `rustc_wrapper` and `sccache_dir`. The
`hosts` tool lists them and, with `probe`, reports cores, load and spare cores. A name not in the file
is refused by name.

## Running it by hand

The `AGENTS.md` sections are the definition. By hand, on this box, the full gate is:

```sh
sh scripts/check-fmt.sh main
sh scripts/check-test-attrs.sh
sh scripts/clippy-gate.sh --workspace --all-targets --features local -- -D warnings
LETIBOT_REQUIRE_APPARATUS=1 cargo nextest run --workspace --no-fail-fast --features local --profile gate
LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --doc --features local
cargo build --release --bins --features local
sh scripts/check-dist-names.sh --built
```

and the quick one is the same file's `## Quick gate` block - or, through the script that runs either
with the gate's own environment and lock:

```sh
sh scripts/gate-at.sh <worktree> /build/target-<name> origin/main           # the full gate
sh scripts/gate-at.sh <worktree> /build/target-<name> origin/main --quick   # what a branch needs
QUICK=1 sh scripts/gate-at.sh <worktree> /build/target-<name> origin/main   # the same
```

`sh scripts/quick-tests.sh --print` says which crates the quick test step would run, from the same
`LETIBOT_GATE_CHANGED` a gate writes.

- **`--features local` is not optional on this box.** `local` is off by default, because it is
  llama.cpp and the default install is cloud-only; this box serves local models, so every step names
  it. `install-gate-build.sh` refuses to install a cloud-only `harnessd` over a local one, so a step
  that loses the flag is a red gate rather than a daemon with no local models.
- Name both llama.cpp variables on anything that builds a LOCAL build: `LETIBOT_LLAMA_DIR` and
  `LETIBOT_LLAMA_LIB`. Each `build.rs` bakes the llama.cpp it found into an rpath that cargo does not
  track, so a worktree keeps linking the first one. A run of vocabulary failures is usually this.
  The gate's HOME is a scratch with no checkout: `gate-env.sh` sets `LETIBOT_REAL_HOME` to the real
  one and keeps both variables by name, and the build also looks under the home above `CARGO_HOME`
  and `RUSTUP_HOME`, so a box with the conventional checkout needs no variable.
- `scripts/clippy-gate.sh`, not `cargo clippy`. Cargo's fingerprint ignores the arguments after
  `--`, so a unit built by plain clippy is fresh for `-D warnings` and nothing is linted.
- `LETIBOT_REQUIRE_APPARATUS=1` turns a test that skips itself (no vocabulary GGUF, no delegated
  cgroup) into a failure, so a skip cannot read as a pass.
- **The test step is `cargo nextest`, and its profile is `.config/nextest.toml`'s `gate`.** Each
  test runs in a process of its own and many at once, where libtest ran one binary at a time and
  `--nocapture` made even that serial: the `letibot-harnessd` lib suite is a quarter of an hour that
  way and about a minute when its tests may overlap. No fail-fast, no retries, and a test still going
  after ten minutes is killed as a hang rather than left to hold the gate.
- **A box without `cargo-nextest` cannot gate.** `scripts/gate-at.sh` refuses a nextest step by name,
  with `cargo install cargo-nextest --locked`, instead of letting `cargo` answer `no such command`.
- A test that cannot share the box with another test process - a fixed port, a fixed path, a socket
  at a well-known name - goes in the profile's `serial` group (`max-threads = 1`). Fixing it (its own
  temp dir, its own port) is the better answer; the group is for the ones that cannot be fixed.
- The quick gate's test step is `scripts/quick-tests.sh`: the crates the change touches and what
  depends on them (`-E 'rdeps(…)'`), not the workspace. A full workspace run is the full gate's, on
  `main` afterwards; a branch that needs one can be run by hand.
- Doc tests are libtest's and nextest does not run them, hence the `cargo test --doc` step beside it.
  Without that line a doctest is in no gate at all.
- The shell suites `tests/launcher.sh` and `tests/rrun.sh` run inside the nextest step, from
  `crates/tools/tests/shell_suites.rs`. They need `bash` and `python3`.
- `check-fmt.sh` takes the ref the change started from. `main` is that ref for a branch about to
  land.

The `queue` tool's `restart` puts a parked row back in line, and `regate` gates the target's tip
again once the cause of a red is gone. Neither is a way past a red.

## A red tip's automatic re-gates are a budget: `[queue] gate_regates`

A daemon that takes a repository's queue used to gate a red tip again at once, so a lease that
flapped (a laptop sleeping, a restart, a lapsed heartbeat) ran the whole suite over and over on a
`main` nobody had fixed, and the gate machines ended up fighting each other. Now:

- A tip whose last verdict was `red` or `timed_out` is gated again at most `gate_regates` times
  (default 2), each after a growing wait (ten minutes, doubled, at most four hours).
- Then the tip is PARKED: its row keeps the verdict it earned (a red tip still halts its review
  queue) and says a person has to act. One warning, once per tip.
- A new tip is a new question and is gated as usual, and `regate` restarts the budget - the person
  who fixed the cause owns the next two automatic gates.

## One gate per repository per machine

A run directory is `<gate root>/gate/<repo>-<hex>/run` on the machine the steps run on. It holds a
`lock/` (one atomic `mkdir`, taken by whoever starts a gate there), a `lock/owner` record (who
started it, which tip, which step) and a `beat` file every poll rewrites. A daemon that wants the
directory either

- takes it, and records itself before the first step and again as each step starts;
- WAITS for the run already there and applies ITS verdict, read off the step files it leaves - one
gate, one verdict, however many daemons wanted to run it; or
- takes it from an owner that is gone: the beat older than 90 s is a daemon that is not there, and
  the step group it left is ended first.

`scripts/gate-at.sh` writes the same `key=value` record (owner `hand`) under the box's gate lock,
`$LETIBOT_GATE_RUN_DIR` (default `$(dirname /build/gate.lock)/gate-runs`), and removes it on the
way out.

## A run's own life: its deadline, its row, and the tip it judges

- One deadline covers the WHOLE run - the arranging, every step and any re-launch - and the worker's
  clock keeps it as well as the runner's: a gate whose thread is stuck inside one call is still ended
  when the budget passes, and its verdict is `timed_out` whatever the runner had got to.
- The published row is the run's: a pass that finds no row, or another run's, puts its own back, and
  a daemon holding the lease clears a row no run is behind. A head counts `so far` from that row, so
  a row nobody is running cannot say a gate has been going for days.
- A target's run judges ONE tip. Each pass (one fetch per 30 s) asks the target's remote where it is;
  if the tip moved - another machine landed - the run is stopped as `superseded: the target moved to
  <sha>` and the new head is gated as usual.

## Batched landings: `[queue] gate_train`

This is a different batching from the full gate's, above: a TRAIN is several branches judged by ONE
quick gate before they land, where the full gate's batch is one run over the landings a tip already
holds. Review stays per branch; the gate batches. With `gate_train = N` in `leticode.toml`, the
queue takes up to N approved rows, rebases them one onto the last onto the target's tip in queue
order (a branch cut from another in the stack goes after it), and gates the final tree once. The
default, 1, is one branch and one gate.

- Green lands every row in order, one commit each, so each row's `landed_sha` is its own commit.
  The push and the install run once.
- Red lands nothing. The stack is split in halves and each row's evidence names its half
  (`TRAIN_SPLIT [half 0f2d9550:ab]`). The first half is gated as its own stack. Green lands it and
  the second half is split again without a gate, or parked `failed` when it is one row. Red splits
  the first half again and the second goes back in line as plain rows. One bad row is parked within
  log2(n)+1 gate runs after the red stack.
- A parked row is `failed` like a lone branch. Its author fixes it and the new head goes back to
  review.
- A fix for a red target always lands alone. A stack forms only while the target is green.

Agents enqueue, they do not gate: the `queue` tool with `{action: "enqueue", branch, why}`, then
`{action: "status", id}`.

## The local stack gate: `scripts/gate-at.sh`

For a repository whose queue does not batch. Agents test the crates they touched and push a branch;
the lead stacks the branches on `origin/main` in one worktree and runs:

```sh
env -i HOME=$HOME PATH=$PATH sh scripts/gate-at.sh <worktree> /build/target-<name> origin/main
```

It runs the merge gate `BASE:AGENTS.md` declares, read through `scripts/gate-steps.sh` (the
queue's reading of it), every step but the install. `check-fmt.sh main` is handed `BASE`. Each step
runs in `scripts/gate-env.sh`'s env with `LETIBOT_REQUIRE_APPARATUS=1`, and the verdict is its exit
status: the first that exits non-zero prints its failing lines and exits 3. A `sessions.db` or
`shared.db` anywhere under the scratch is a test that wrote where the operator's store lives, and
exits 3 too. `GREEN` and exit 0 otherwise.

- One workspace run per box: it takes `flock` on `/build/gate.lock` (`LETIBOT_GATE_LOCK` names
  another) and a second caller waits. Never remove the lock; end the stuck gate.
- Isolated home: `gate-env.sh`, the same builder the queue uses. `hosts.toml`, `known_hosts` and
  the keys `hosts.toml` names are copied in read-only; `providers.toml` is not.
- Stale builtins output: `crates/builtins/build.rs` watches absolute paths, so the script deletes
  every `letibot-builtins-*` output whose `rerun-if-changed` is not this worktree.
- Logs are `<name>.step<N>.log`, with a step's apparatus refusals in `<name>.step<N>.refusals`,
  under `$LETIBOT_GATE_LOGS` (default `$TMPDIR/letibot-gate`).
- On red, bisect the stack, fix the failing commit, gate again, then push `main`.

`TARGET_DIR` goes on `/build`, never the root disk, and is deleted after the landing. A link that dies
with `signal 7 [Bus error]` is a full `/build`. `letibot doctor` warns about a gate lock held over 2h
and lists `/build/target-*` dirs no process works in.

## On a host, by hand

```sh
ssh -i ~/.config/letibot/keys/lubuntu1 dead@lubuntu1
```

`~/.local/bin` and `~/.cargo/bin` are not on a non-interactive ssh command's `PATH`, so call tools
by full path. Usually set `[queue] gate_host` instead, which also transfers the tree and keeps the
build cache.

## What a red is, and what it is not

- Red is a verdict about the branch: a step exited non-zero.
- `timed_out` is the run passing its thirty-minute deadline; the step in flight is killed with its
  process group.
- `Infra` is not a verdict. A machine that did not answer or a tree that would not arrive sends the
  target back to `gating` with the reason, retried after a minute. The one `Infra` that stays
  `blocked` is a step killed at the gate's memory cap, because that step ran.
- `blocked` is a tip nobody can judge: no `## Merge gate` section, or a landing tree that cannot be
  made. It stops both queues and needs fixing by hand.

## Never on GitHub Actions

Actions are rate-limited for this account, so the gate runs in the queue, here or on a declared host,
and no workflow in this repository runs it. A workflow judges a push, which is not the commit that
lands. `.github/workflows/release.yml` is not a gate: a tag starts it and it builds the assets
`install.sh` downloads. `crates/tools/tests/gate_not_on_github.rs` refuses a workflow that runs a gate
step or answers a `pull_request`.

## What the gate does not see

Apparatus the platform can never have (`apparatus::applies`) prints `THIS IS NOT A PASS` and the
step still passes. `crates/turn/tests/restore.rs`'s real-store tests
read the operator's `sessions.db` and run only with `LETIBOT_CHECK_REAL_STORE=1`, by hand. The
acceptance specs (`tests/acceptance/run.sh`, the real head in tmux) need a terminal, a release build
and, for `--local`, firecode; run them by hand when a change reaches the head.

## A test cannot write the operator's store

`letibot_tokencore`'s store doors refuse a store in the default data directory
(`letibot_paths::refuse_the_default_store`): a debug build refuses by itself, because a debug build
of this tree is a test or a debugging session and the installed daemon and head are release bins, so
an agent's `cargo test` needs no ceremony. `LETIBOT_REFUSE_DEFAULT_STORE=1` says the same for a
release build, and `=0` says a debug run means the operator's store. The refusal comes before the
open, so it creates nothing: a test that forgot its own data directory fails, rather than leaving
rows that `letibot --sessions` afterwards shows beside the real ones.

A refusal ENDS the process (the sentence on stderr, then an abort) rather than panicking. A store is
opened from watcher, link and client threads a test never joins, where a panic is a test waiting for
an answer that never comes; an abort cannot be missed by being on the wrong thread. Fix it by giving
the test the data directory it means - `letibot_testenv::isolate_data()` for a bridge's seat, a
tempdir in `XDG_DATA_HOME`, or `--store`.
