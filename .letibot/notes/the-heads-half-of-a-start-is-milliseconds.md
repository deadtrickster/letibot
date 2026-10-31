<!-- abstract: MEASURED 2026-11-01 (release, the operator's build): a 2000-row session costs the head 0.14 ms to load, 43 ms for the first draw and 0.18 ms per draw after; the daemon's Hello snapshot is 0.4 ms / 545 KB; and the FIRST picker draw costs 1.48 s because it builds the plugin host (every built-in wasm compiled) — so "the head takes a minute" is not the replay, the draw or the snapshot. -->

# The head's half of a start is milliseconds, and the first picker draw is 1.5 s

MEASURED 2026-11-01, branch `agent/the-head-shows-what-it-is-doing`, worktree of the same name,
`CARGO_TARGET_DIR=/data/letibot-target-head`, `cargo test --release -p letibot-tui --lib` with a
throwaway timing test (deleted afterwards). Release is the profile that matters: `scripts/letibot`
picks `target/release`, so the operator runs an optimised head.

## The numbers, release (debug in brackets where it differs)

| what | 200 rows | 2000 rows |
|---|---|---|
| `App::load(snapshot)` — the replay | 0.19 ms | **0.14 ms** [0.36 ms] |
| the FIRST `App::screen(120,40)` after it | 4.8 ms | **43 ms** [128 ms] |
| every draw after that | 0.18 ms | 0.18 ms |
| `Hub::snapshot()` — the daemon's `Hello` payload | 42 µs | **408 µs**, 545 KB of JSON, 681 µs to serialise |

and `App::new` is 23 µs. So the head's whole half of a `--continue` — load plus first draw — is
**under 50 ms for a 2000-row conversation**, and the replay is ~0.07 µs a row.

## The one head-side cost that is not milliseconds

`App::picker_rows()` (and `/todo`, and the queue pane's layer) calls `App::plugin_host()`, which
**builds the plugin stack: discovery plus a wasmtime compile of every built-in layer**. Measured:
**1.48 s release [12.5 s debug]** on the first call, 0.2–0.4 ms on every call after. It is paid once
per head per folder, on whichever of those three asks first — so "opening `/sessions` takes ages"
is a real thing even though the list itself is instant.

## The rake

**A "why is it slow" question about this head has to name which of the four it is** — the replay,
the first draw, the daemon's snapshot, the plugin host — because three of them are microseconds and
one is a second and a half. The instrument is a `--release` timing test with `Instant`, and the
debug profile is not a substitute: the wasm compile is 8× slower and the first draw 3× slower there,
which is enough to send a session chasing the wrong half. The daemon's own open/restore (the store
walk, `Harness::open`) was NOT measured here and is the only remaining candidate for a minute.
