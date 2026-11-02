<!-- abstract: What the queue's raise sites actually guard on (so a new raise knows whether it will fire every 30 s), which code is re-said while it stands, and the three rakes under the notice coalescing. -->

# A wall of notices: what the raise sites actually do, and the three rakes

`RepoQueue::pass` runs six steps every 30 s, and every `self.raise(` in `mergequeue/serve.rs` and
`mergequeue/review.rs` sits behind one of four guards: `first_time(key)` (the store's `tell_once`,
once per key and across restarts), a `said` slot in memory, a successful CAS move, or the branch's own
streak. So the queue has no per-pass raise, and the question for a NEW raise is which of the four it
is behind - `none` means it fires every 30 s.

`store_busy_wait` is the one code re-said while its condition holds, at most every 120 s with the
seconds waited in the sentence. That is why it is the one entry in `tui::app::notes::LIVE`: a burst
counts things that happened, and a standing condition is not one.

Three rakes. `denied:<request id>` is a code, not a kind, so `note_kind` must read
`warning::family(code)` or five refusals are five kinds. A burst's row lives at its first live
member's seam, so rewind with `invalidate_history_from(head_seam.saturating_sub(1))`, one BEFORE the
seam, because `hist_marks[k]` records the state after seam `k` was drawn. And a chain's members are
not the index range between its ends: filter on `chained_ts(notes, j, kind).is_some()`.
