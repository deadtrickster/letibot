<!-- abstract: MEASURED 2026-10-26 (T2, thread-per-session): every session the daemon holds is served by its own thread (`sessions::Court`, armed by `Daemon::run`), and the four traps that came with it — `open` must treat "in `courts`" as open, `Sessions::wake` needed no forwarding, two back-to-back prompts become ONE turn, and `ulimit -n 256` sets the HARD limit in `sh`. -->

# One thread per session: the shape, and the four traps

MEASURED 2026-10-26, branch `thread-per-session`. The operator: *"one daemon, several sessions …
give a thread to each session and that is it"*. Before: one worker took one command at a time
across every session (a prompt to B queued from 14:08 to 14:10:52 behind A's turn).

## The shape

`sessions::Court` is one session's thread, built the way a subagent already was
(`harness::serve_child`): the hub is marked `Owner::OwnThread`, the harness MOVES to the thread
(`Sessions::court_of` moves the harness, the nag clock, the bang state, the plugin host, the seat
slot/condition and `armed`), and the thread loops on `hub.take_own_work_until(next_idle_at())`:
`Command` → `dispatch`, `Wake` → `wake`, `TimedOut` → sweep / nag / park / plugin tick.
`OwnWork::Closed` parks it for one more job, because a daemon going away still has children to stop
(`drain_subagents` sends `Follow::Finish`; a *closed session* is told apart by
`registry.get(id).is_none()` and finishes at once).

The daemon keeps the doors a queue does not carry: `Court::ask` (blocks, for tests and `cli.rs`'s
banner/`--prompt`) and `Court::post` (fire and forget, for the review pass). **Nothing on the
daemon's idle pass may `ask` a court** — that is the old serialisation wearing a lock.

## Trap 1 — `open_as` must count a court as open

`Registry::create` rings `Work::Open`, and by the time the daemon's loop drains that ring the court
may already hold the harness. `harness_as` then refuses the hub it finds (`Owner::OwnThread`) with
*"session … is a subagent served by its own thread, so the daemon's worker will not open a second
harness on it"*. `open_as` and `open_the_gatekeeper` therefore ask
`open.contains_key(id) || courts.contains_key(id)`.

## Trap 2 — `Sessions::wake` needed no forwarding at all

`wake_route` is two facts: *does this struct hold the harness* and *who owns the hub*. A court's
session is not held and is `OwnThread`, so the existing `ItsOwnReader` arm already hands the ring to
`hub.wake_its_own_reader()` → the court's `OwnWork::Wake` → its own `Sessions::wake` → `Drive`.
That is why the daemon's `Work::Woken` arm never blocks on a session.

## Trap 3 — two prompts submitted back to back are ONE turn

The second sits in the hub queue and the running turn picks it up at a round boundary as a steering
follow-up item, so an ordering test that counts `TranscriptItem::Assistant` rows sees 2, not 3.
Assert the ORDER of the requests the fake provider was asked (`thread_per_session.rs` holds its
socket), not a row count. And to reproduce "queued behind a turn", hold the first at the model.

## Trap 4 — `ulimit -n 256` in `sh` sets the HARD limit

The gate's "256 open files" is the SOFT limit. `(ulimit -n 256; …)` also drops the hard limit, and
then `session_lifecycle`'s fifty-subagent fixture fails on its own sentence — *"open files 256 →
256 (hard 256), 512 asked for"* — which is `room_for` saying the box cannot give the room, not a
regression. Use `ulimit -Sn 256` (hard unlimited) to reproduce the gate.

## What is still on the daemon, and why the tests did not have to change

`Sessions` is unchanged for a fixture that drives it by hand: `threaded` is armed ONLY by
`Daemon::run` (`serve_each_on_its_own_thread`), so `drive_worker`-style suites run the same
single-threaded path they always did. The suites that DO call `daemon.run` (run_off_worker,
promote_midflight, loop_closes, plugins_pane, model_change_mid_retry, steering_survives_retry,
restart_seats, idle_clock, bridge/end_to_end) pass with courts underneath them.

## UPDATE 2026-10-26 (gate): a `Sessions` that drops leaves its courts running — and that is the SIGSEGV

MEASURED with the S1 rebase, `ulimit -Sn 256`, `LETIBOT_REQUIRE_APPARATUS=1`, the test binaries run
directly so the load is real: `--test promote_midflight` printed `test result: ok. 1 passed` and
then died of `Segmentation fault: 11` — **2 runs in 10**, and never a `panicked at` anywhere,
because the crash is the *exit* and not the test. A court owns its session's `Harness` and its own
owned `Parts`; `Sessions` dropped the channel senders and walked away, so the court woke on the
disconnect and was still freeing the vocabulary when `exit()` ran the ELF finalizers.

`impl Drop for Sessions` is the fix: take the `Court`s (dropping each `jobs` sender FIRST, so a
parked court wakes on `Disconnected`), then wait for the threads — bounded by `COURT_TEARDOWN`
(2 s, one deadline for all of them) — **only when `registry.is_closed()`**, because a `Sessions`
dropped while its daemon still serves is a caller giving up a handle and its sessions need their
reader. `Sessions::close` joins the court it closes for the same reason. 20/20 under load after.

**Rake:** a thread a `Sessions` armed must not outlive it at exit, and the symptom is silence — a
passing suite that segfaults on the way out. `promote_midflight` is the canary (it drops `Sessions`
without ever calling `drain_subagents`).

## UPDATE (same day): the session_channel flake was a POSITIONAL script, not a court

`--test session_channel`'s `a_refusing_verdict_goes_back_to_the_child_that_did_the_work` timed out
once under load at `wait_for`. **No court runs in that binary** (`serve_each_on_its_own_thread` has
one caller, `Daemon::run`, and `session_channel` never constructs one), so it was not the threads.
The cause is the fixture: three sessions talk to one stub, and the stub answered by ARRIVAL ORDER.
After the reviewer hands its verdict in, the reviewer's second round and the child's turn on the
complaints both start on their own threads; when the child's request overtook the reviewer's, the
child was answered with the reviewer's afterword — the hub dump shows the child's turn generating
*"I read the diff against the brief…"* — and the answer the test waits for had gone to the reviewer.
Fixed with `Stub::answering_by`: a `(marker, script)` rule matched against the detokenized prompt
("message from your parent session"), tried before the positional script, which keeps its own
cursor so a rule never eats a later request's place.

**Rake:** a stub shared by several sessions that answers by position is a race with extra steps;
answer by *who is asking*, or make the racing answers interchangeable.
