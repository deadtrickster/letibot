<!-- abstract: MEASURED 2026-10-26 on lab2x1 building B8: a Peeked snapshot has no client id (a refusal is `peek:<session>`), a row cursor must count against `items_dropped`, a bridge fixture that seats `--bash` cannot open a session on this box (no delegated cgroup), a session is registered before it is open (so a `Settings` read can have no `mode` row), and the warning-code guard cannot see a code passed through a helper. -->

# B8's footnotes: a peek has no request id, and a fixture's seat decides whether it can open

MEASURED 2026-10-26 on lab2x1 (Linux; tools on lab2x1, model on the Mac), building
`leticode_transcript` and `leticode_mode` in `crates/bridge` with `PROTOCOL_VERSION` 54.

## `Peek`/`Peeked` is the one pair with no correlation id — both directions are keyed by
## something else

`ClientFrame::Peek { session_id, shape }` carries no `client_request_id` (it is a read, not a verb),
and `ServerFrame::Peeked` carries none either: the fields are `session_id`, `dropped`, `events`,
`snapshot`. So a reader that sends one and waits has exactly two facts to match on:

* **the answer** is matched by `session_id == the session it asked for`;
* **the refusal** (a session the daemon does not hold) is a `ServerFrame::Rejected` whose
  `client_request_id` the SERVER mints as `format!("peek:{session_id}")`. Nothing else on the wire
  spells that; `crates/sessionlog/src/server.rs`'s `Peek` arm does.

A bridge head that waits on a counter of answers only (what `crates/bridge/src/head.rs`'s
`State.peeks` does) would sit out its whole patience on a refused read and then report a timeout
about an answer that had already arrived. That is why the fold keeps `peek_error: Option<(session,
reason)>` beside `peek: Option<(session, Snapshot)>`.

## A row cursor is a SESSION row number, and `items_dropped` is what makes it one

A `Snapshot` is the TAIL of a conversation under the view's bounds (2000 rows / 8 MB), so
`items[0]` is row `items_dropped` of the session. A cursor that counted window positions would
silently re-read (window moved back) or skip (window moved on) rows. `crates/bridge/src/rows.rs`'s
`page()` is: `skip = since.saturating_sub(items_dropped)`, and `next = items_dropped + len`, with a
`dropped` count when `since < items_dropped` — the same rule `leticode_events` follows for its ring.

## A bridge fixture that seats `--bash` cannot open a session on this box

`cfg.seat = Seat::Leticode` + `cfg.allow_bash = true` picks `HostBackend::executable("/")`, and that
needs a delegated cgroup v2 subtree. MEASURED here, in `crates/bridge/tests/{end_to_end,no_spin}.rs`
(both seat `--bash` and both are unmodified):

```text
the daemon opens its first session: Setup("whole-host backend: io: cgroup mkdir failed on
`/sys/fs/cgroup/user.slice/user-1000.slice/session-31173.scope/letibot.1633901.0`:
Permission denied (os error 13) — a delegated cgroup v2 subtree is needed; … it is not writable")
```

Then the bridge's `leticode_start` follows it with `no harnessd: set $LETIBOT_BIN …`, because the
worker died before the socket served anything. **A new bridge test that does not run commands should
set `cfg.allow_bash = false`**: `Leticode` + no bash takes `HostBackend::writable("/")`, opens
anywhere, and still has no `Prereq::Confinement` — so `allow-all` still needs a person, which is
what `tests/mode_consent.rs` is about. (The related standing note is the Mac one:
`which-seat-a-test-must-fixture-on-a-mac` — `Seat::Coder` there, the cgroup here.)

## A session is REGISTERED before it is OPEN, so its settings can be empty

`sessions.db`/the registry takes the session id at `NewSession`; the harness — and therefore
`Config::settings` — is published when the WORKER opens it. A head that attaches in that window and
sends `ClientFrame::Settings` gets a frame with no `mode` row (not an error, an honest empty list).
`tests/mode_consent.rs`'s probe retries every 25 ms until the row appears, which is what a pane does.

## The byte vocabulary makes message zero ~54k "tokens" — so a 32k window compacts on every turn

A provider-less or byte-vocab bridge fixture with `cfg.context_window = Some(32_768)` decides the
resident prompt (53849) is over the window before every turn and publishes
`compaction FAILED: this session's PROMPT is 53797 token(s) of a 32768 token window … Message zero
is the system prompt and the tool schemas, and compaction never rewrites it`. Harmless and very
noisy; `Some(200_000)` is the quiet fixture.

## A warning code that reaches the wire through a variable is INVISIBLE to the guard

`crates/sessionlog/tests/warning_codes.rs` scans this tree for `Warning { code: "…" }` literals and
`import_note("…", …)` and fails until each is classified in `letibot_sessionlog::warning`. It does
NOT see a code passed as an argument to a helper — `Harness::warn_mode(code: &str, …)` or
`Sessions::warn(session, "…", …)` — so **a new code emitted that way is unclassified, defaults to
`Class::Failure` (red), and nothing fails.** B8 added `mode_confirmed`, `mode_declined`,
`mode_unconfirmed`, `mode_confirm_refused`, `mode_consent_refused` and classified all five by hand;
the next person adding one through a helper has to remember the table.
