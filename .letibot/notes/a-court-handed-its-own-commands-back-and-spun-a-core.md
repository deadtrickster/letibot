<!-- abstract: MEASURED 2026-10-26 (B7): a session's `leticode_stop` left the daemon at a full core per session, because `Sessions::dispatch`'s Interrupt/Message arms handed the command back to "the thread that owns it" — and on a court that thread IS this one, so the command ping-ponged on the same queue for ever; also how to measure and sample it. -->

<!-- abstract: MEASURED 2026-10-26 (B7): a session's `leticode_stop` left the daemon at a full core per session, because `Sessions::dispatch`'s Interrupt/Message arms handed the command back to "the thread that owns it" — and on a court that thread IS this one, so the command ping-ponged on the same queue for ever; also how to measure and sample it. -->

# A court handed its own commands back to itself, and spun a core

MEASURED 2026-10-26, branch `bridge-reaps-and-stop-spin`. In the field: seven daemons of
another session's packages sat at ~100% of one core EACH, per stopped session (one at 200% for
two sessions), with CPU time equal to their age, until they were killed. They had been stopped
with the bridge's `leticode_stop` and had run hundreds of rounds with monitor-woken turns before.

## The loop, exactly

`Sessions::dispatch`'s two arms — `CommandKind::Interrupt` and `CommandKind::Message` — carry a
hand-back that exists for the WORKER: both readers take from ONE queue (`Hub::take_own_work` for
the thread that runs the session, `Hub::try_command` for the worker), so the worker can win a
command that belongs to a session served on its own thread, and it puts it back
(`Hub::give_back_to_its_own_reader`) for that thread to take.

The guard was `hub.owner() == Owner::OwnThread` — true on the court TOO, because the court runs
`dispatch` on the very session whose hub it marked. So `court_main` took the Interrupt off the
queue, `dispatch` pushed it back on the same queue, rang the hub's own condvar, and
`take_own_work_until` answered `Command` again. No sleep anywhere: one core per session,
forever, and nothing on the model's side and nothing on the log (the only visible work is
`Hub::refresh_activity`, which the hand-back calls — that is what `sample` shows).

The same arm is how a `task_message` to a ROOT session (one with a court) spun a daemon.

Fix: `Sessions::hand_back_to_the_owner` — the hand-back now also requires
`!self.open.contains_key(session_id)`, the same *held here IS served here* fact `wake_route`
already decides a wake with. A reader must never hand work back to itself.

## How it was found, and how to find the next one

1. A test that measures, not argues: `crates/bridge/tests/no_spin.rs` builds the daemon
   IN-PROCESS (`Daemon::serve`, as `end_to_end.rs` does), so `getrusage(RUSAGE_SELF)` is the
   daemon's own CPU; three-second windows with a 300 ms budget per phase (a spin spends ~3 s).
   Measured before the fix: **2.994 s over a 3 s window**. After: **2.0 ms**.
   **One test per file** — a second test running beside it burns the CPU it is accounting for.
2. `sample <pid> 12 5` on the test binary while it spins names the thread (`session-s-…`) and
   the hot stack (`court_main` → `dispatch` → `give_back_to_its_own_reader` → `refresh_activity`).
   Copy the built test binary into the scratch dir's script and `sample` it — do not guess which
   loop from the shape of the code; the ping-pong looks like ordinary dispatch.

## The rake

A "hand it back to the owner" path needs the *am I the owner* test beside it, or it is a
self-loop with a mutex and a condvar, which reads as fine code. The predicate that already
existed one door over (`wake_route`'s `held_by_the_daemon`) is the one to reuse.
