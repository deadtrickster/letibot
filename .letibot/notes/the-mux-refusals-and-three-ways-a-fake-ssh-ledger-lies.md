<!-- abstract: MEASURED 2026-10-27 (R99, branch ssh-sessions): the four sentences a refused mux session prints, the measured proof that the old chunked write left 0 bytes while returning no error, and three traps in a fake ssh that counts its own sessions (exec skips the EXIT trap, a mkdir lock is not a lock — 40 callers left 32, and `'$$'` in single quotes is literal). -->

# A refused ssh session, three ways a fixture's ledger lies, and where the 0-byte file came from

MEASURED 2026-10-27, branch `ssh-sessions` (package R99), on lab2x1 with the tools inside a bwrap
boundary and cargo offline. Everything below was forced through `crates/tools/tests/remote_sessions.rs`'s
fake ssh — no network, no real host.

## The four sentences a refused session prints, and why they are not the command's exit

`MaxSessions` is 10 per OpenSSH **connection**, and one `ControlMaster` is one connection. A session
over that limit, or a socket that went away, answers with one of:

```
mux_client_request_session: session request failed
mm_send_fd: sendmsg(1): Message too long          ← measured live in this very session (see below)
channel 3: open failed: administratively prohibited: open failed
Control socket connect /tmp/letibot/ssh-…-call.sock: Connection refused
```

All four arrive **exactly like a dropped connection**: exit 255, nothing on stdout. Classified as
`Dropped`, they cost two things: the sentence the model saw was *lost the connection, it may not have
run* (or, for a write, `Nothing on disk changed`, which was false), and a `run` — which is
at-most-once — was told *it may have run* about a command that never got a channel. They are a
*refusal*, provably pre-execution (`mm_send_fd` failing means the session's fds never reached the
master, so the remote shell never started), which is what makes retrying them legitimate for any
operation. My own tool call hit one mid-session while writing a job launcher: `mm_send_fd:
sendmsg(2): Message too long: … lost the connection before it answered` — from a daemon older than
this fix.

## The 0-byte file, measured on the old code

The old `Ssh::write_abs` sent `ceil(len / 48 KiB)` separate ssh calls, **threw away each chunk's exit
status**, then ran `mv -f tmp target` unconditionally, with the temp name `<path>.letibot-<pid>` —
one per path per *process*. With a fake far side whose `base64` cannot write, a 200 KiB write
returned **no error** and the target was **0 bytes**. That is FB's report (`found no match … Nothing
was written`, then `wc -c` = 0) exactly: an earlier write reported success while writing nothing.

## Three ways a fake ssh that counts its own sessions lies

1. **`exec` skips the `EXIT` trap.** A fake that ends with `exec bash -lc "$last"` never runs its own
   `trap … EXIT`, so a "live sessions" file per session is never removed and the ledger reports a
   session alive for ever — and then the *ceiling* the test enforces refuses real calls. Run the
   command as a child and `exit $?`.
2. **A `mkdir` lock is not a lock.** `while ! mkdir lock; do sleep 0.01; done` / `rmdir lock` around
   a counter lost increments: **40 concurrent callers finished at 32**, because a releaser's `rmdir`
   can land after the next holder's `mkdir` and delete *its* lock. Files that are created and removed
   (one per session) plus an append-only event log and the arithmetic in Rust need no lock at all.
3. **`'{d}/live/$$'` in single quotes is literal.** `$$` does not expand, so every session writes the
   same file and the count is always 1. And a *body* string handed to `format!` as an argument is not
   unescaped either: `{{ … }}` stays doubled and bash sees `{{`.

## And the bound decision, from what could be measured here

A full handshake (KEX + public-key attempt) against lab2x1's own sshd on loopback: **50–70 ms**, five
runs. A muxed session's extra cost is one round trip, which this boundary cannot measure because
letibot's key lives on the Mac — so the choice (one master **per daemon**, pid in the socket name,
plus a second socket for `git fetch`, which the process does not start and cannot count) rests on the
structural argument: 8 sessions per daemon is then a guarantee rather than an average, and the dial
is amortised by `ControlPersist=120`.
