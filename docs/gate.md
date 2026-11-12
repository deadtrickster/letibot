# The gate - how a branch is judged, here and on the fleet

**The gate is the `## Merge gate` section of a repository's `AGENTS.md`, and nothing else is.** It is
read from `main`'s copy, so a branch cannot weaken the gate it is about to be judged by, and it is
a list of shell commands - one per line - in the first fenced block under that heading. A
repository without the section does not have a weak gate: the queue holds every branch of it until
somebody writes one (`merge_gate` is the tool that asks the operator what it should be).

## Who runs it, and when

The merge queue, on an approved branch, after the reviewer's verdict and after rebasing the branch
onto the target's tip - never on the branch's own base, which is what makes the answer a fact about
the landing rather than about the push. It runs in a landing worktree of its own
(`~/.local/share/letibot/landing/<repo>-<hash>`, never inside `.git`) with `CARGO_TARGET_DIR`
pointed at that worktree's `target`, so a branch is never judged with the main tree's artifacts -
and nothing a branch builds reaches the main tree's `target/release` while it is still a candidate:
the sixth step does that, after the push.

- The first step that fails **parks** the branch; the row says which step and carries what it wrote.
- A step still running after thirty minutes is killed with its process group and the row is
  `timed_out`.
- The same steps, without the install, gate the **target's own tip** whenever it moves.
- A step named `install-gate-build` runs only **after the landing was pushed**, so nothing is
  installed that is not on the remote.

The gate's cost is a compile, so it is not run on every save: a branch is gated when it is ready to
land. `queue status` says where a row is, and the row's green line names the commit and the machine
that judged it - `` `main` is green at 0f2d9550 (lab2x1 gate) ``.

## Where it runs

Two places, and a project picks with one line in its own `leticode.toml`:

```toml
[queue]
gate_host = "lubuntu1"    # a [host.…] from ~/.config/letibot/hosts.toml; empty is this machine
```

or `--gate-host lubuntu1` on the daemon's command line, which wins over the file, and
`--gate-host local` to put it back here for one run. The name is read once at daemon start. The
landing worktree is made here either way, and the target branch moves here; a host only runs the
steps, in a copy of that tree ([docs/remote-hosts.md](remote-hosts.md), *The merge gate on a
declared host*, has the transport, the two clocks, the one-line time report and what a cold cache
costs).

**`gate_in = "firecode"` runs each step in a firecode VM.** Opt-in per repository, in the same
`[queue]` table. On the gate machine (this one, or the `gate_host`) every step becomes a `firecode`
call over the gate's checkout: on a Mac `firecode sh -c` (project mounted read-only with a
throwaway layer, nothing copied back, the VM deleted), elsewhere `firecode run` (files sent in,
nothing copied back, the VM discarded after the gate). Deadlines, logs and verdicts are the
ordinary gate's, since the step is still the unit. A machine without a working firecode (no CLI, no
`/dev/kvm` on Linux, `firecode doctor` failing) leaves the tip UNJUDGED with a sentence. The steps
never run on the host instead. Default is `gate_in = "host"`, which is today's behaviour.

**A step that runs a `[boundary] firecode_only` program always runs in firecode.** The gate reads
each step the way the seat does, through scripts it calls, and a hit puts the whole gate in a VM
whatever `gate_in` says. When the gate host cannot start a VM, the steps run in this machine's VM
instead. They never run bare.

**`~/.config/letibot/hosts.toml` is the operator's, and is never in a repository.** A machine is a
`[host.NAME]` table - `hostname`, `user`, `key` (the identity file ssh is given; the key itself is
never read into a session or a log), `root`, `path` (directories put in front of the remote
`PATH`), `reserve`, `flags`, `mise`, `never_touch`. The `hosts` tool lists them and, with `probe`,
reports cores, load and the **spare** cores, which is how a busy box is passed over for a free one.
A name that is not in that file is refused by name, with the file's path, rather than being taken
as a hostname and dialled.

## Running it by hand, on this box

The section in `AGENTS.md` is the definition; this is what it means to type, and the two things
that make a by-hand run disagree with the queue are called out under it:

```sh
sh scripts/check-fmt.sh main
sh scripts/check-test-attrs.sh
sh scripts/clippy-gate.sh --workspace --all-targets -- -D warnings
LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture
cargo build --release --bins
```

- **Name both llama.cpp variables** on anything that builds: `LETIBOT_LLAMA_DIR` and
  `LETIBOT_LLAMA_LIB`. Each `build.rs` searches for a checkout and bakes the answer into an rpath,
  and `cargo` cannot see that input - so a worktree that linked one llama.cpp keeps linking it.
  A surprising run of vocabulary failures is usually this.
- **`scripts/clippy-gate.sh`, not `cargo clippy`.** Cargo's fingerprint ignores the arguments after
  `--`, so a unit already built by a plain `cargo clippy` is fresh for `-D warnings` and the step can
  exit green having linted nothing.
- **`LETIBOT_REQUIRE_APPARATUS=1` is the project's own demand**, written into the step so a test
  that skips itself - no vocabulary GGUF, no delegated cgroup - cannot read as a pass. Without it,
  ten review tests that never ran report `10 passed`.
- **The shell suites are inside that `cargo test`.** `tests/launcher.sh` and `tests/rrun.sh` run
  from `crates/tools/tests/shell_suites.rs`: `scripts/letibot` and `scripts/rrun` are shell, so no
  `#[test]` reaches them, and four launcher cases drifted with nothing running the file. Both need
  `bash` and `python3`; without them the test skips through `apparatus`, which the step's
  `LETIBOT_REQUIRE_APPARATUS=1` turns into a failure.
- **`check-fmt.sh` takes the ref this change started from.** `main` is that ref for a branch
  about to land, which is why the section says `main`; checking something else, name its base.

The `queue` tool's `restart` puts a parked row back in line, and its `regate` gates the target's tip
again once the cause of a red is gone. Neither is a way past a red - a branch that cannot pass the
gate is not landed, which is the whole point of having one.

## On a host, by hand

```sh
ssh -i ~/.config/letibot/keys/lubuntu1 dead@lubuntu1
```

**Tools by full path**: `~/.local/bin` and `~/.cargo/bin` are not on the `PATH` of a
non-interactive ssh command, so `~/.cargo/bin/cargo`, `~/.local/bin/act`. Most of the time the
answer is not to run it by hand at all: set `[queue] gate_host` and let the queue do it, which also
transfers the tree, keeps the build cache and reports the time.

## What a red is, and what it is not

- **Red** is a verdict about the branch: a step exited non-zero, and the row parks with its output.
- **`timed_out`** is a verdict about the step: thirty minutes with no answer, killed with its
  process group.
- **`Infra` is not a verdict.** A machine that did not answer, a key that is not installed, a tree
  that would not arrive - the target goes back to `gating` with the sentence that stopped it and is
  tried again after a minute. A host that is down costs the queue a minute per attempt, not a
  `/queue restart`. The one `Infra` that stays `blocked` is a step killed at the gate's memory cap,
  because that step *ran*.
- **`blocked`** is a tip nobody can judge at all: no `## Merge gate` section, or a landing tree that
  cannot be made. It stops both queues, and it is the one state that is somebody's to fix by hand.

## Never on GitHub Actions

The operator, 2026-10-11: *"i've been rate limited on github recently with actions, so _act_ is our
friend. we can use other machines like lubuntu2 to run tests"*, and *"for runners id prefere
lubuntu1,2 only"*. So the gate runs in the queue - here, or on a declared box over `ssh -i` - and
**no workflow in this repository runs it**. A workflow judges a push, which is not the commit that
lands; its red is read by nobody; and it spends a budget that is rationed.

`.github/workflows/release.yml` remains, and is not a gate: a tag starts it, it builds the assets
`install.sh` downloads, and no branch ever triggers it. `crates/tools/tests/gate_not_on_github.rs`
refuses a workflow that runs the gate's steps or answers a `pull_request`, so the second gate
cannot come back by accident.

## What the gate does not see

A machine without an apparatus the tests demand prints `THIS IS NOT A PASS` with the reason and the
step still passes - that is a fact about the machine, not about the diff, and it is why the gate
says so loudly. And `crates/turn/tests/restore.rs`'s real-store tests read the operator's own
`sessions.db` and run only with `LETIBOT_CHECK_REAL_STORE=1`, by hand, on the box that has the
store: a red there is a fact about the store rather than about the code, so it is not in the gate.

And the acceptance specs - the real head driven in tmux, `tests/acceptance/run.sh` - are not a step
of the gate either: they need a terminal, a release build and, for `--local`, this box's firecode.
Run them by hand when a change reaches the head.
