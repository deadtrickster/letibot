<!-- abstract: MEASURED 2026-10-26 on the launcher/daemon lifetime reports: in THIS tree only --continue ever names a session to reopen, no head can stop a daemon, `--new` with no daemon up used to open TWO sessions, and a one-shot's leak is the background job, not the daemon. -->

# The four launcher/daemon lifetime reports, as this tree actually behaved — MEASURED 2026-10-26

The operator's four reports from a stroppy-io session (plain start resumes the latest; `--new` ran
on qwen; exiting a head took the folder's harnessd with it; one-shot daemons outliving their run).
What follows is what the CODE in the tree does, which is not all of what they saw — the box's
installed `~/bin/letibot` has drift no repository has (see `roles.rs`'s note about "262 more lines
of drift on the operator's machine"). Branch `launcher-fixes`.

## A plain start never resumed here, and that is now pinned

`scripts/letibot` sets `SESSION=` for a plain run and passes `${SESSION:+--session}` — so `harnessd`
mints a fresh id and its first session is new. Only `--continue` (via `--latest-session`) and
`--session ID` name one. The operator's plain run that "resumed" must therefore have been either
their drifted launcher or a `--continue`; nothing in this tree could have done it. What WAS real is
that the help text did not say which session a start opens: it does now, and `tests/launcher.sh`
asserts on the DAEMON'S ARGV that a plain start carries no `--session` and `--continue` carries one.

**The rake worth keeping:** the launcher's record key hashes the workspace path with NOTHING
appended (`printf '%s' | sha256sum | cut -c1-12`). A test that pipes `$(cd … && pwd -P)` straight in
hashes a path with a trailing newline, finds no record, and reports a daemon that is alive as gone.
I lost an hour to that in `tests/launcher.sh` (`key_of`).

## No head can stop a daemon — there is no `ClientFrame::Stop` in `crates/tui`

`hub.rs`'s header says it: "Nothing in this file counts heads to decide whether to keep working.
Detaching the last head is a `HeadDetached` event and nothing else." The TUI's quit card sends
`Detach`; `Action::StopDaemon` is the operator choosing *stop the daemon too*, and it is the only
head-side stop. So "exiting the head took the folder's harnessd with it" cannot come from this
tree's head — the killer is a signal (a `--stop`, or the OS). The daemon's own exit is either
`Registry::close` (signal / `Stop` / idle window, default 3600s) or a crash, and a signal leaves NO
line in the log, which is why the 00:18 daemon's log just stops.

What IS a real defect of that shape: `--new` with no daemon up started the daemon (whose eagerly
opened first session is fresh) AND asked the head for another one — two sessions, the first empty,
and that empty one is what the NEXT plain attach lands on. Fixed: the head gets `--new-session` only
when the daemon was already up (`STARTED`).

## `--new` ran on qwen because the session's MODEL was named from the binding

`Sessions::wiring` (`sessions.rs`) and `Harness::open`'s `SessionRecord.model_id` (`harness.rs`) both
wrote `cfg.model` — the local VOCAB BINDING (`qwen-3.8-flash-next`, which the launcher passes on
every start) — instead of `cfg.prompt_model_name()` (who answers: `deepseek/deepseek-flash` under
`[default] provider`). The harness's own config was right all along (measured: an in-process daemon
gives the first AND a head-created session the same provider), which is what made it a two-names
defect: the header's first frame, the ctrl-s picker and `letibot --sessions` said qwen, the `model`
settings row said deepseek. Fixed in both places, with `crates/harnessd/tests/session_model.rs`.

## A one-shot's leak is the JOB, not the daemon

`harnessd --prompt` runs its steps, `daemon.shutdown()` → `Registry::close()` → the run enders
(`end_running("the daemon stopping")`) kill what is running, then it exits. What it could NOT do is
anything about a `bash {"background": true}`: that job lives in the `session` scope, and an
`explicit`-scoped one outlives the process by design. Decision: **a one-shot refuses `background:
true`** (`tools/src/runtime.rs`'s `InvokeCtx::one_shot` ← `Config::one_shot` ← `cli.rs` from the
steps themselves; the refusal is in `bash.rs` and is decided from the REQUEST, so it is testable on a
box whose backend can start no process). The alternative — wait for pending jobs, bounded — was
refused because a one-shot is exactly the mode where nobody wants to sit in front of the long job.
