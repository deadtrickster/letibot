<!-- abstract: A model hears about the queue only through three `Harness` functions - `queue_notices`, `queue_emergency_notices`, `handover_notices` - because every other queue event is a hub `SessionEvent` a head draws and a model never reads. -->

# The queue's model-facing notices are exactly three functions, and `Why::Emergency` is dead

A head and a model are told different things. In `crates/harnessd/src/harness.rs`, called one after
another by `Harness::wake`: `queue_notices` (`:2914`, the target has no `## Merge gate` section),
`queue_emergency_notices`
(`:2961`, a red / timed-out / unjudged target, and a review row parked for a person) and
`handover_notices` (`:3065`, a branch handed back under `[queue] merge_via_session`).

Everything else - `target_red`, `queue_blocked`, `merge_queued`, every `QueueMoved`, the watcher's
relayed moves - is a `SessionEvent` on a hub that a head draws and a model never reads, so "heads may
still show the alert while the model hears nothing" is exactly *do not compose these three*.
`ring_repository`/`ring_project` only WAKE a session; the words are read off the store at the wake.

`delivery::decide` has `if f.emergency { Immediate(Why::Emergency) }`
(`crates/harnessd/src/delivery.rs:128`), and `Facts::emergency` is set nowhere but `delivery/tests.rs`,
so an emergency reaches a model through the notice list and never through the completion sorter.
