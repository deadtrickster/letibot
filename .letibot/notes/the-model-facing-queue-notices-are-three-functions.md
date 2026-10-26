<!-- abstract: MEASURED 2026-10-26 building harnessd's --no-queue: a model sees queue news only through three Harness functions (SessionEvent warnings are heads-only), Why::Emergency in delivery.rs is unreachable in production, and Registry::new() already returns an Arc. -->

# The queue's model-facing notices are exactly three functions, and `Why::Emergency` is dead

MEASURED 2026-10-26, branch `bridge-no-queue` (package Q1), asking the question a `--no-queue`
daemon has to answer: *which paths put a queue notice in front of a MODEL, as opposed to a head?*

## Three, and they are all in `Harness`

`crates/harnessd/src/harness.rs`, called one after another in `Harness::wake`:

* `queue_notices` — *the target has no `## Merge gate` section*, so a branch waits.
* `queue_emergency_notices` — the red / timed-out / unjudged target, and a review row parked for a
  person. This is the one that reached package P1's model before its brief.
* `handover_notices` — a branch the queue handed back to the project's main session
  (`[queue] merge_via_session`).

Everything else the queue says is a `SessionEvent` on a hub: `target_red`, `queue_blocked`,
`merge_queued` (`HarnessTaskRunner::enqueue_finished`), every `QueueMoved`, and the store watcher's
relayed moves (`mergequeue::watch` → `Registry::broadcast`). **A head draws those and a model does
not read them** — a model's only door is the notice list `wake` composes. So "heads may still show
the alert while the model hears nothing" is exactly *do not compose these three*.

`QueueDoor::ring_repository` / `ring_project` only WAKE a session; the words are still those three,
read off the store at the wake. A daemon that spawns no `QueueService` never rings anyway — but it
would still have composed them at its next wake, which is why the flag has to be read in the
`Harness` too and not only in `mergequeue::spawn_for`.

## `Why::Emergency` (`crates/harnessd/src/delivery.rs`) is UNREACHABLE in production

`delivery::decide` has `if f.emergency { Immediate(Why::Emergency) }`, and `Facts::emergency` is set
nowhere but `delivery/tests.rs` — `facts_of` builds every `Facts` from `Facts::routine(..)` and
never sets it. So the "an emergency is delivered immediately" row of that module's own table is a
shape with no producer: a target emergency reaches a model through the notice list, not through the
completion sorter. Worth knowing before grepping delivery.rs for a leak that is not there.

## Two smaller rakes

* `letibot_sessionlog::registry::Registry::new()` **already returns `Arc<Registry>`** — `Arc::new(
  Registry::new())` is a type error that reads as one (`expected Registry, found Arc<Registry>`
  because `Arc::new` expects a `Registry` and got an `Arc`).
* `mergequeue::spawn_for` builds its `QueueEnv` (and so `lease::this_daemon()`) BEFORE the
  `--no-queue` return. Harmless — nothing is claimed — but a reader looking for "where does it
  decide" finds the eprintln, not the env.
