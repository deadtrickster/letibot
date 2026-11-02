<!-- abstract: A "hand the command back to the thread that owns it" path needs the am-I-that-thread test beside it, or a session served on its own thread takes its own command back for ever and burns a core. -->

# A court handed its own commands back to itself, and spun a core

`Sessions::dispatch`'s `Interrupt` and `Message` arms hand a command back to "the thread that owns it",
because the worker and a session's own thread take from one queue. The guard was
`hub.owner() == Owner::OwnThread`, which is true on the court too - the court runs `dispatch` on the
very session whose hub it marked. So the court took the interrupt off the queue, `dispatch` pushed it
back on the same queue, rang the hub's condvar, and the loop answered `Command` again: one core per
stopped session, for ever, with nothing on the model's side and nothing in the log.

The fix is `Sessions::hand_back_to_the_owner`, which also requires `!self.open.contains_key(session_id)`
- the same *held here IS served here* fact `wake_route` already decides a wake with. A reader must never
hand work back to itself.

The next one is found by measuring, not by reading: `crates/bridge/tests/no_spin.rs` builds the daemon
in-process so `getrusage` is its own CPU (2.994 s over a 3 s window before the fix, 2.0 ms after), with
one test per file because a second test burns the CPU it accounts for. `sample <pid> 12 5` then names
the thread and the hot stack.
