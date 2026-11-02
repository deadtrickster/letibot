<!-- abstract: What this tree does about four launcher/daemon lifetime reports: only `--continue` or `--session` names a session, no head can stop a daemon, `--new` asks for a second session only when one was up, and a one-shot refuses `background: true`. -->

# The four launcher/daemon lifetime reports, as this tree actually behaved — MEASURED 2026-10-26

A plain start opens a NEW session: `scripts/letibot` sets `SESSION=` and passes `${SESSION:+--session}`,
so only `--continue` and `--session ID` name one, and `tests/launcher.sh` asserts it on the daemon's
argv. The trap beside it: the record key hashes the workspace path with nothing appended, so a test that
pipes `$(cd … && pwd -P)` straight in hashes a trailing newline, finds no record, and reports a live
daemon as gone - `key_of` (`tests/launcher.sh:590`) is the fix.

No head can stop a daemon: there is no `ClientFrame::Stop` in `crates/tui`, the quit card sends
`Detach`, and `Action::StopDaemon` is the person choosing it. A signal leaves no line in the log, which
is why a daemon's log sometimes just stops. `--new` with no daemon up used to open two sessions - the
daemon's own first one is fresh - so the head gets `--new-session` only when the daemon was already up.


A one-shot's leak is the JOB, not the daemon: a `background: true` job lives in the session scope, so a
one-shot refuses it (`crates/tools/src/builtins/bash.rs:307`), decided from the request.
