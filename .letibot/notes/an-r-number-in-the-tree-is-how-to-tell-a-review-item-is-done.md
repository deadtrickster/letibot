<!-- abstract: MEASURED 2026-10-31: R89, R91 and R92 are almost entirely fixed in the tree — each done item carries its `R9x.n` at the site, so `grep -rn 'R91\.\|R92\.\|R89' crates` is the census, and the TODO.md entries are stale; the three items that were genuinely open were an interval measured to the wrong clock, a review row for a branch with nothing on it, and a pane that hid a waiting row's reason. -->

# An `R9x.n` in the tree is how to tell a review item is already done

MEASURED 2026-10-31, worktree `agent-the-delivery-and-lifecycle-reviews` (`main` at
`4fb39324`), working R89/R91/R92 from `TODO.md` — entries filed 2026-10-12 against
`50b3e55` whose line numbers have drifted by thousands.

## The census, in one command

```sh
grep -rn 'R89\|R91\.[0-9]\|R92\.[0-9]' --include=*.rs crates | grep -o 'R9[12]\.[0-9]*' | sort -u -V
```

Every item this tree has *fixed* carries its number in a comment at the fix site (and
usually a test named after the defect). On this tree that is **R91.1, .2, .3, .5, .6, .7,
.11, .12, .13, .14, .17** and **R92.1–.7, .14, .17–.19, .21, .24, .25, .27–.29, .31, .33**;
R89 has no per-item numbers left at all because the whole review became one module
(`crates/tui/src/ui/transcript/notice_card.rs` + `line_cards.rs`, and `walls.rs` as its
measuring stick). So: **grep before working an item.** A TODO.md entry marked OPEN is not
evidence that anything in it is still broken.

## The three that were genuinely open, and what each taught

* **R91.8** — the restore threshold was measured stop → the daemon's **start**, while the
  operator's rule (*"more than 1 hours after the stop … simply remind"*) and the module's own
  rationale (*"a subagent resumed hours later works on a tree that moved"*) name stop → **the
  open**. They coincide for the session a daemon opens as it starts and are hours apart for one
  opened later (a head attaching, a `/resume`, a picker row) — the exact case the threshold is
  for. Fixed in `5a13eaea`: `Stopped::restores(opened_ms, …)`, and `restart::header` names both
  intervals when the open is over a minute after the daemon's return.
  **Test trick worth keeping**: a harness-level test cannot backdate `Registry::started_ms()`,
  so stage the divergence in miniature — the old daemon stops *at the instant the new one
  started* (`life_stopped(OLD, registry.started_ms())`), the session is opened 20 ms later, and
  `cfg.restore_within = 1 ms`. Both readings are distinguishable and the test runs in 2 s.
* **R92.11/.16** — `enqueue_finished` read the head with `unwrap_or_default()`, so a branch
  this host does not have, or one still at its base (a VM child commits in the VM's copy; an
  uncommitted child leaves the same branch), became a review row nothing can act on: the row
  re-waits for ever because `start_reviewer` proves its clone at `head_sha`. Fixed in
  `2a6bcca8` as a refusal, which the caller already turns into `merge_not_queued`.
* **R92.34** — the queue's detail pane drew a row's `failure` only under `attempts > 0`, and
  the rows that most need a reason (a failed clone, a VM slot wait) carry one with
  `attempts` still 0 — `review::wait_for_tree`'s doc even says "visible in the queue's own
  pane". Same commit; the failure is its own field now.

## What I left alone, and why (so the next session does not re-derive it)

* **R91.4** (an interrupt is followed at once by a turn nobody asked for) is the operator's own
  R79 ruling, quoted at `JobWatchers::turn_ended`: *"once turn ends no reason to hold"*.
* **R91.6's `persist_context`** still takes a bare `?` while its neighbours go through
  `store_patiently`. Untestable in reasonable time: the store retries a busy write for 60 s
  itself before the harness can see `is_busy()`, so a test needs a wedged store for 90 s.
* **R91.10** (a busy-store wait ignores an interrupt): ending the turn there abandons rows that
  are only in memory. A trade, not an obvious fix.
* **R92.22** (the quit card under-counts what a stop kills) needs a field rano's `QuitCard`
  does not have; **R92.26** (a box-wide OOM judged RED) needs a detection design; **R92.15's**
  "a child that never enqueued leaves a tree nothing reaps" is a sweep with real deletion risk.
