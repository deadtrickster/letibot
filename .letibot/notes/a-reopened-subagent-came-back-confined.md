<!-- abstract: MEASURED 2026-10-26 on the Mac: a `task` child is seated `coder` and only its SPAWN makes it unconfined (via `..self.base.clone()`), so a parked child reopened from the store re-seats `coder` + confined — a silent re-confinement on Linux and a refusal to open on macOS. -->

# A reopened subagent came back confined, because `unconfined` is inherited at the spawn and nowhere recorded

MEASURED 2026-10-26 on the Mac, `crates/harnessd/tests/session_lifecycle.rs`'s
`a_message_to_a_parked_child_reopens_it_from_the_store` (the merge gate's second-to-last red):

```
DEBUG worker took s-root-lifecycle-sub-…: ignored
DEBUG warning resume_failed: the `coder` role needs a confined execution backend and one
  could not be built over "/tmp": … macOS has no namespace boundary …
DEBUG warning session_unavailable: a message from `s-root-lifecycle` arrived and this
  session could not be opened to hear it: <the same sentence>
DEBUG warning message_idle: a message … could not be delivered: `…-sub-…` is gone
```

The reopen itself worked (`task_message` answers *"was reopened"*), the WORKER's
`Sessions::open` failed, and the message was then dropped as `message_idle` — so the test's
"no turn ran" was three warnings deep in the hub, not in the fixture.

## The chain

* a `task` child is always `Seat::Coder` unless the call names a role
  (`run_to_completion`: `if role.is_empty() || role == "coder" { Seat::Coder }`), and its
  SESSION ROW records that seat (`Harness::open` writes `role: Some(cfg.seat.as_str())`);
* `Config::unconfined` is set for `Seat::Leticode` and **inherited by its subagents** —
  `sub_cfg` is built with `..self.base.clone()`, and the parent's `base` already had
  `unconfined = true` from `Harness::open`'s `cfg.unconfined = … || seat == Leticode`;
* so a leticode session's coder child is unconfined **at the spawn only**. `reopen_parked`
  (`HarnessTaskRunner::send`) → `Registry::create_under` → the worker's
  `Sessions::harness_as` → `Harness::open`, which reads the row's `role` (`coder`) and
  takes `unconfined` from `Sessions::base` (false) — there is no column for the standing.

Linux only loses the boundary silently (the confined backend builds and the subagent is
re-confined to the tree its parent left, which is the defect the inheritance exists to
prevent); on a Mac it is a refusal, so **every leticode session's parked child was
unreopenable**.

## What the fix is (branch `mac-last-two`)

`Harness::open`, where `unconfined` is resolved: a session whose stored row has a parent
also stands where its ancestors stand — `unconfined_by_inheritance` walks
`session.parent_session_id` up and answers true if any ancestor recorded `leticode`. The
store's `role` is the SEAT and not the standing, and this is the one place the two facts
differ. A row the walk cannot read (vanished ancestor, unparseable role, >32 links) leaves
the standing unproven, i.e. confined, which is what every session had before.

## Where the diagnosis came from, and how to get it again

The test's panic says nothing about why. Three `eprintln!`s in the wait loop — the
`drive_worker` return, `sessions.opened()`, and every `SessionEvent::Warning` on the
reopened hub — named it in one run. `Outcome` has no `Debug`, so match it into words.
