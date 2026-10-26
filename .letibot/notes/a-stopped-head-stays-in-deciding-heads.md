<!-- abstract: MEASURED 2026-10-26 building B6: a head that sends a command and then shuts its socket down (what `leticode_stop` does) can stay in `Hub::attached_heads` FOR EVER and keep `deciding_heads` non-empty, because every reply write in `serve_conn` is a `?` that returns past the `hub.detach` after the loop. -->

# A stopped head can stay in `deciding_heads` for ever, and the `?`s are why

MEASURED 2026-10-26, branch `bridge-attach` (B6), writing the end-to-end test that
attaches a *watching* head to a session another bridge started.

## The measurement

The test asserted the obvious thing after `Bridge::stop`: the stopping head is gone and
the watcher is the only head left, so `deciding_heads()` is empty. It never became
empty — `attached_heads() == 2`, `deciding_heads() == ["letibot-mcp"]`, still 60 s later.
A probe attach (`since_seq = 0`, read `snapshot.heads`) named them: `h1` (the bridge that
started the session, stopped and dropped) and `h6` (the watcher). `h1` was still there.

Confirmed the mechanism by changing ONE thing: a 300 ms sleep at the end of
`Head::stop` (between writing `Detach` and the `Head::drop` that shuts the socket down)
made the assertion pass on the first try — `heads=1 deciding=[]`.

## The mechanism, in the code

`crates/bridge/src/head.rs`'s `Head::stop` writes `Interrupt` then `Detach`; `tools::stop`
then removes the head from the map, `Head::drop` runs `HeadClient::shut_down()`
(`shutdown(Both)`). In `crates/sessionlog/src/server.rs` the connection loop answers the
interrupt with `writer.lock().unwrap().write(&f)?` — and that write now fails with
`EPIPE`, because the client's socket is already shut. **`?` returns from
`serve_conn`**, so the `seat.hub.detach(&seat.head_id)` that follows the loop never runs,
the queued `Detach` frame is never read, and the head stays in the hub for the life of
the daemon.

A `wait` head does not do this, which is why the leak is invisible everywhere else: a
`wait` only closes its socket, and EOF takes the `break Ok(())` path that does detach.

## Why it matters more than tidiness

`Hub::deciding_heads()` is what `Answers::ask` and `ask_question` read **before** posting
a card: empty means `gate:no-head` in microseconds, non-empty means post it and wait out
the deadline. A stopped head that stays in that list makes every later ask on that
session wait five minutes and come back as *"nobody answered"* — the *"real answer given
for a fake reason"* `Caps::can_decide` exists to prevent. Same family as B6's watcher cap,
which is why it was found here.

## Not fixed in `bridge-attach`, on purpose

The fix is small — `impl Drop for Seat { fn drop(&mut self) { self.hub.detach(&self.head_id); } }`
(safe across a `Switch`: `leave` already detached the old id, and `Hub::detach` on an
unknown id is a no-op) — but it is a daemon lifetime change, not a bridge one, and the
obvious regression test *races*: a test where the client sends `Interrupt` and shuts the
socket down only proves the leak when the daemon loses the race, so it passes with the
bug. The deterministic shape is a `UnixStream::pair()` + `sessionlog::server::serve_conn`
on a thread, with the client calling `shutdown(Shutdown::Read)` (not Write): the daemon's
reply then fails with `EPIPE` while its reader never sees EOF. That test plus the fix is
its own package.

## Also measured, and worth knowing

`Sessions::open_first` calls `ensure_gatekeeper`, so **every** daemon holds `s-gatekeeper`
from startup with nobody attached — `leticode_attach {"session": "gatekeeper"}` therefore
works against any folder's daemon, and that session is the one place a watcher is the
*only* head (so `deciding_heads()` empty is observable there).
