<!-- abstract: MEASURED 2026-10-30 (branch agent/finish-the-slow-host-fix): the queue halt was POLL_BOUND (10 s), not gate_deadline; a missed poll is now retried — but a timed-out call still counts as a breaker FAILURE, so three consecutive 30 s misses open the breaker and the fourth poll comes back "gone"; and the first/second gate on lab2x1 measured 1.42 s transport + 35.7 s cold build (37.5 s) vs 0.21 s + 1.2 s (2.0 s). -->

# The gate had two clocks, and a poll that missed still feeds the breaker

MEASURED 2026-10-30, branch `agent/finish-the-slow-host-fix` (worktree of the same name, `main` at
`18bfbce5`), finishing `6855bab9`. `[queue] gate_host = "lab2x1"` is live in the operator's
`leticode.toml`, so this is the merge gate's real path.

## 1. The ten seconds that halted the queue was `POLL_BOUND`, not `gate_deadline`

The queue's own words were *"asking how `lab2x1` is doing on lab2x1 did not answer within 10 s and
the ssh this daemon started was killed with its whole process group"*. `asking how … is doing` is
`Remote::poll`; its bound was `POLL_BOUND = 10 s`, and it was the SAME constant that bounded a
step's fate — one miss returned `StepEnd::Infra`, which `finish_health` wrote as
`HealthState::Blocked`. So a poll on a box whose own `cargo test --workspace` held 25 of 32 cores
killed a step four minutes in and halted both queues.

Three things had to change together, and any one alone is not the fix:

* `POLL_BOUND` is now 30 s and is *only* the health question;
* a missed poll is **retried** (the answer is `tail -c +<offset>`, so nothing is lost) — only the
  step's own deadline, or a call that reached no machine, ends a step;
* `GateVerdict::Infra` writes the target back to `gating` with a 60 s retry window
  (`UNJUDGED_RETRY_MS`), never `blocked`. `blocked` is only for a tip nobody can judge (no
  `Merge gate`, no landing tree) and for a step killed at the gate's memory cap — that one RAN.

## 2. A timed-out call still counts against the breaker — so the retry is bounded at 3 misses

`gate_host::run` ends with `rpc::after(host, pass, answered)` where `answered` is
`Ended::Answered && code != 255` — a **`TimedOut` is `answered: false`**, i.e. a failure on the
breaker's streak. `BREAKER_THRESHOLD = 3`, `BREAKER_WINDOW = 60 s`, `BREAKER_COOLDOWN = 30 s`, so
three consecutive 30 s misses (31 s apart, inside the window) open the breaker and the FOURTH poll
is refused by `rpc::before` → `Missed::gone` → the step is killed and UNJUDGED.

That is the honest reading of "a machine that will not fork a shell for ninety seconds is not
answering", and the outcome is a retry rather than a halt — but it is the breaker, not
`POLL_BOUND`, that finally ends a step on a saturated host. If that ever needs to be longer, the
change is in `crates/tools/src/rpc.rs` (a timeout should not feed the streak at all), not here.

## 3. The first and second gate on lab2x1, measured with the cost line in place

One step, `cargo build --offline --bins -p letibot-harnessd`, the gate's directory removed between
the runs (a `--shared` clone of this repo under the session's scratch dir, so the gate dir on the
host was `measure-repo-bca0a0944641`, NOT the operator's `letibot-77ef789ede8c`):

| | transport | the step | in all |
|---|---|---|---|
| the first gate, nothing on the host | 1.42 s (1.28 bundle, 0.15 checkout, 0.14 tree) | **35.7 s** cold `target/` | **37.5 s** |
| the second gate of the same tree | 0.21 s (nothing to send) | 1.2 s warm | **2.0 s** |

The line every gate now writes into its first step's log:

```text
[gate on lab2x1: the repository 1.28 s, the checkout 0.15 s, the tree 0.14 s — 1.42 s of transport
 out of the gate's 30 min; build cache COLD — this gate made it, so the first step below pays for
 the whole dependency tree; 209.7 GiB free on that filesystem]
```

`COLD` is asked of the host in the same call that `mkdir`s the directories — *before* `target/`
exists — so it says whether THIS gate makes the cache rather than guessing from a size.

## The rake

**A bound in a sentence has to name the thing it bounded.** `did not answer within 10 s` was true
and useless: it did not say *a poll*, so the reader could not tell a slow machine from a slow build.
Every timeout sentence on this path now carries its own clock — `the gate's own deadline passed
(30 min)` for a step, `not answered within 30 s` for a poll, and the poll's bound is the one the
last poll was actually given (`min(POLL_BOUND, what is left of the gate)`), not the constant.
