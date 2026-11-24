# The gate

The gate is the `## Merge gate` section of a repository's `AGENTS.md`: the first fenced block under
that heading, one shell command per line. It is read from `main`'s copy, so a branch cannot weaken
the gate it is judged by. A repository without the section has no gate, and the queue holds every
branch of it until somebody writes one (`merge_gate` asks the operator what it should be).

## Who runs it, and when

The merge queue, on an approved branch, after rebasing it onto the target's tip. It runs in a
landing worktree of its own (`~/.local/share/letibot/landing/<repo>-<hash>`, never inside `.git`)
with `CARGO_TARGET_DIR` pointed at that worktree's `target`, so a branch is never judged with the
main tree's artifacts and nothing it builds reaches the main tree's `target/release` before the
install step.

- The first step that fails parks the branch. The row names the step and carries its output.
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
  process group and the row is `timed_out`.
- The same steps, without the install, gate the target's own tip whenever it moves.
- A step named `install-gate-build` runs only after the landing was pushed. It refuses when the
  build it was told about is the directory the launcher already starts from, and when that build
  is older than the newest `harnessd` it can see: both name `CARGO_TARGET_DIR`, because a step
  that lost it used to copy every binary onto itself and still say installed.
- With rows in line the queue fetches `origin`'s target (at most every 30 s) and fast-forwards the
  local target and its checkouts when origin is ahead. A push refused because origin moved since
  puts the local target back at origin's tip and requeues the row, which is rebased and gated
  again. `landed_local` is left for a push that fails with origin unreachable.

A branch is gated when it is ready to land, not on every save. `queue status` says where a row is,
and a green line names the commit and the machine that judged it.

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
`root`, `path`, `reserve`, `flags`, `mise`, `never_touch`. The `hosts` tool lists them and, with
`probe`, reports cores, load and spare cores. A name not in the file is refused by name.

## Running it by hand

The `AGENTS.md` section is the definition. By hand, on this box:

```sh
sh scripts/check-fmt.sh main
sh scripts/check-test-attrs.sh
sh scripts/clippy-gate.sh --workspace --all-targets --features local -- -D warnings
LETIBOT_REQUIRE_APPARATUS=1 cargo nextest run --workspace --no-fail-fast --features local --profile gate
LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --doc --features local
cargo build --release --bins --features local
sh scripts/check-dist-names.sh --built
```

- **`--features local` is not optional on this box.** `local` is off by default, because it is
  llama.cpp and the default install is cloud-only; this box serves local models, so every step names
  it. `install-gate-build.sh` refuses to install a cloud-only `harnessd` over a local one, so a step
  that loses the flag is a red gate rather than a daemon with no local models.
- Name both llama.cpp variables on anything that builds a LOCAL build: `LETIBOT_LLAMA_DIR` and
  `LETIBOT_LLAMA_LIB`. Each `build.rs` bakes the llama.cpp it found into an rpath that cargo does not
  track, so a worktree keeps linking the first one. A run of vocabulary failures is usually this.
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
- Doc tests are libtest's and nextest does not run them, hence the `cargo test --doc` step beside it.
  Without that line a doctest is in no gate at all.
- The shell suites `tests/launcher.sh` and `tests/rrun.sh` run inside the nextest step, from
  `crates/tools/tests/shell_suites.rs`. They need `bash` and `python3`.
- `check-fmt.sh` takes the ref the change started from. `main` is that ref for a branch about to
  land.

The `queue` tool's `restart` puts a parked row back in line, and `regate` gates the target's tip
again once the cause of a red is gone. Neither is a way past a red.

## Batched landings: `[queue] gate_train`

Review stays per branch; the gate batches. With `gate_train = N` in `leticode.toml`, the queue takes
up to N approved rows, rebases them one onto the last onto the target's tip in queue order (a branch
cut from another in the stack goes after it), and gates the final tree once. The default, 1, is one
branch and one gate.

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
