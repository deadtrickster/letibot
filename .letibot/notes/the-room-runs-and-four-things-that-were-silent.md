<!-- abstract: MEASURED 2026-10-29 building the room's slice 2 (the coordinator runtime): `Room::answer` drops the events, so a server built on it answers and rings nothing; a client whose reader thread holds a strong `Arc` keeps its own socket open and the coordinator never sees the close; the arbiter is routed every event and must be woken by only three; and a lease file parsed with `trim` reads a leading tab as a field boundary. -->

# The room runs — and the four things that were silent until a test caught them

**MEASURED 2026-10-29**, branch `agent/the-room-runs-the-coordinator`, building slice 2 of
`crates/room`: the `--room` coordinator, the socket, the lease, the tool, the beat-on-behalf and
the wake. `docs/room.md` § *Slice 2, as built* is the map; this is the four rakes.

## 1. `Room::answer` returns the reply and THROWS AWAY the events

Slice 1's dispatch was `pub fn answer(&mut self, who, request, now) -> Response` — the events a verb
produced lived in `Outcome` and were dropped at the end of each arm. A server written on it answers
every frame correctly and **rings nobody**, which is a room that only answers whoever asked, and no
test of the frames can see it. The fix is `Room::answer_traced -> Answer { response, events }`, with
`answer` as a one-line wrapper so the 22 slice-1 tests did not move.

The rake: **when a state machine hands back a reply, ask what it did with the rest of the outcome.**

## 2. A dropped client did NOT close its socket — the reader thread owned it

`Client` spawned a reader thread holding a `Client` clone (a strong `Arc<Shared>`) and a
`BufReader<UnixStream>` from `try_clone`. Dropping every `Client` the caller had left the socket
open, because the reader still held a descriptor on it and was blocked in `read_line`; the
coordinator therefore never saw EOF and `note_gone` never ran. MEASURED as a failing test: a session
dropped its client, and the claim it held was still `Claimed` five seconds later.

Two things are needed and either alone is not enough:

* the reader holds a **`Weak`**, so it is never what keeps the client alive;
* `impl Drop for Shared` calls `shutdown(Both)` on the writer — `shutdown` acts on the **socket**,
  not on one descriptor, so the reader's blocked `read` returns and the peer sees EOF. Dropping the
  writer alone does nothing while a dup of it is open.

## 3. The arbiter is routed every event, and must be WOKEN by three

The server sends every event to the arbiter (`Server::with_arbiter`), because that is how a question
reaches it and it is never a poster. But the wake path is per event: a `Posted` about somebody
else's item would ring the coordinator's own session and buy a turn — one model call per item, on
the one session whose job is to answer questions. So `SessionRoom::connect` recognises the arbiter
(it compares the session with `room::session_id()`) and drops `Posted`/`Claimed`/`Done` before the
push. Routing is about *who may see it*; the wake is about *what is worth a turn*. Two decisions.

## 4. `parse` with `trim` ate the empty first field

The lease file is `owner\tpid\tbeat_ms\tstarted_ms`. A line whose owner is empty (`"\t1\t2\t3"`) must
read as *no lease at all* — and `text.lines().next()?.trim()` strips the leading tab, so the pid was
read as the owner and a junk file became a holder. `trim_end()` is the fix, and the test that pins it
is `a_lease_file_nobody_can_read_is_no_lease_at_all`.

## Two smaller ones, both about the daemon's own seams

* **`Registry::bell()` returns `&Arc<Bell>`**, not an owned one. A closure that captures
  `let bell = registry.bell();` borrows the registry and does not outlive the function —
  `Arc::clone(registry.bell())` is the spelling.
* **A room daemon must not idle-exit.** `Config::idle_exit_after` defaults to an hour, which is right
  for a folder's daemon (everything it is lives in the store and the next `leticode` resumes it) and
  wrong for the coordinator: its room is memory, so an idle exit is a room that silently emptied.
  `room::coordinator::configure` sets it to `None`.

## And the shape that made the tests possible

`Server` needs no daemon: `Server::serve(path, Room::new())` plus `letibot_room::client::Client` is a
real socket with real frames, so `crates/room/tests/socket.rs` proves the transport's own facts
(subagent refused at the door, a tag as a lock across two connections, a bell with an address and not
for a reader, a close that gives a claim back at once, the clock taking back a silent claim, the
daemon's beat keeping one alive, the arbiter hearing everything, a reconnect answering the repeat)
with no model, no store and no harnessd. The coordinator's own test then composes the lease, the
server and the tie-breaker prompt in one process.

## The fifth rake: the shared target dir, again — and its symptom this time

`cargo build -p letibot-harnessd --lib` **succeeded** while `cargo test -p letibot-harnessd --lib`
said `cannot find \`room\` in \`builtins\`` — twice, from two files, with no other error. The rlib
`harnessd`'s test unit linked (`libletibot_tools-184a12bdc61fe8ff.rlib`, dated the same minute as
my edit) had been built from the **main checkout**, which has no `builtins::room`, because
`CARGO_TARGET_DIR` points at `~/Projects/letibot/letibot/target` and both worktrees' `letibot-tools
v0.5.1` hash to one unit. `strings …/x.rmeta | grep -c builtins/room` answered `0`, which is how to
tell a stale artifact from a source problem in one command.

The cure that worked: `cargo clean -p letibot-tools` in the shared target (it removed 22.3 GiB and
every dependent — the main checkout rebuilds), then rebuild. **A `cargo check` passing is not
evidence about a `cargo test` build**: check needs only `.rmeta`, test links the `.rlib`, and the two
can come from different trees. The note `two-worktrees-share-one-target-dir-and-one-out-dir.md` says
the rule; this is the second symptom to recognise it by.
