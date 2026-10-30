<!-- abstract: MEASURED 2026-10-30: no code in this tree raises per queue pass (every raise is behind `first_time`, a `said` slot, or a successful CAS move), `store_busy_wait` is the one code re-said while it stands (every 120 s), `merge_queue_unreadable` is in the register with NO raise site, and a refusal's code carries its request id so `note_kind` must read the FAMILY or five refusals are five kinds. -->

# A wall of notices: what the raise sites actually do, and the three rakes

MEASURED 2026-10-30, branch `agent/a-wall-of-notices-becomes-a-count` (`main` at `692b153c`).

## 1. Nothing in the queue raises once per PASS — and the guards are four shapes

`RepoQueue::pass` runs `reap_gate` → `adopt_orphans` → `review_upkeep` →
`settle_landed_by_hand` → `release_terminal_resources` → `land_next`, every 30 s
(`REVIEW_PASS_SECS`). Every `self.raise(` in `mergequeue/serve.rs` and `mergequeue/review.rs`
(28 sites) is behind one of:

| guard | sites |
|---|---|
| `first_time(key)` — the store's `tell_once`, once per key per daemon AND across restarts | `alarm` (`target_red`), `kept:` (`queue_branch_kept`, three sites), `unjudged:` (`queue_blocked` for an Infra verdict), `queue_settings_unreadable`, the settled-transition note |
| a `said`/`Option<String>` slot in memory | `StoreTrouble::said` (`merge_queue_store_error`), `settings_said`, `gatekeeper_said` (`review_queue_not_open`), `not_target_said` |
| a successful CAS move (`review_to`/`merge_to`/`gating_to`/`mv`) — a row leaves the state it was raised in | `queue_blocked` ×10, `merge_failed`, `merge_conflict`, `queue_push_failed`, `queue_install_failed`, `queue_needs_human`, `reviewer_could_not_start`, `refusal_undelivered` |
| the branch's own streak | `queue_waiting_for_vm` (`wait.first`), `queue_vm_slots_exhausted` (`long && !wait.raised`) |

So **coalescing was not needed to hide a per-pass bug** — there is none in the queue. If a new
raise is added, the question to ask is *which of those four it is behind*, and the answer
`none` means it will fire every 30 s.

## 2. `store_busy_wait` is the one code re-said while its condition holds

`Harness::persist`/`persist_reporting` say it at most every 120 s, with the seconds waited in the
sentence (`crates/harnessd/src/harness.rs`, two sites). It is therefore the one entry in
`tui::app::notes::LIVE` — a burst is a count of things that HAPPENED, and a standing condition is
not one. Everything else that repeats repeats once per event.

## 3. `merge_queue_unreadable` is in the register and nothing emits it

`sessionlog/src/warning.rs` declares it (`Class::Failure`), `TODO.md:1351` claims it was wired at
`Registry::merge_queue_for` — and **`merge_queue_for` does not exist in this tree**, nor does any
other raise site. `crates/sessionlog/tests/warning_codes.rs` only checks codes that ARE emitted,
so a declared-but-unraised code is invisible to the guard. (This is the mirror of the guard's
other hole: a code reaching the wire through a variable.)

## 4. The rake that cost the first cut: `denied:<request id>` is a code, not a kind

`app/events/asks.rs` files a refusal under `code: format!("denied:{request_id}")`. `note_kind`
returned `w.code`, so **five refusals were five kinds** and no count could ever form — the wall
of red the operator photographs was the one wall coalescing could not touch. `warning::class`
already strips the id (`split_once(':')`, `Some(("denied", _))`), so the rule existed; it is now
`warning::family(code)` and `note_kind` reads it. One definition, because two would drift.

## 5. The second rake: a burst's row must be redrawn when a member joins

The history walk is incremental (`hist_marks`, `note_upto`). A burst's row lives at its first
LIVE member's seam, so a second member arriving changes a row that is already drawn.
`file_note` rewinds with `invalidate_history_from(head_seam.saturating_sub(1))` — **one before**
the seam, because `hist_marks[k]` records the state *after* the notes at seam `k` were drawn, so
rewinding to the head's own seam skips the note whose row has to change.

And the look-back for "does this note join a burst" must be the previous note **of the same
kind**, not `notes.last()`: the wall interleaves a `queue_blocked` with a `queue_branch_kept`,
and a look at the last one filed misses every second member.

## 6. The third rake: a chain's members are not the index range between its ends

`burst_at` collected `(head..=end)` — an index range. With kinds interleaved, a
`queue_branch_kept` sitting between two `queue_blocked` notes was counted as a member of the
`queue_blocked` burst: five notices drew `3 reviews could not run`. Filter on
`chained_ts(notes, j, kind).is_some()`.

## Where the coalescing lives

* `tui/src/app/notes.rs` — `COALESCE_MS` (60 s), `LIVE`, `COUNTED_AS`, `coalescable`, `chains`,
  `burst_head`/`burst_end`, `burst_at` → `Burst::{Drawn, Single, Counted}`.
* `tui/src/ui/transcript/window.rs` — the walk's note arm, three ways instead of one.
* `tui/src/ui/panes/notes.rs` — `burst_card` / `burst_popup` / `burst_lines`.

At ONE member nothing changes: the notice's own card, byte for byte. A burst is ≥2.
