<!-- abstract: MEASURED 2026-10-26 (package G2): the review-queue id is per workspace and a session has one writer, claimed at Harness::open — a heartbeat that only DATES a claim means a dead pid must be taken over at once; `merge_review.session_id` is the CHILD that did the work, not the review host; and four tests simulated a restart by opening a second harness while the first was alive. -->

# One writer per session, and the review queue is per workspace — MEASURED 2026-10-26, branch `gatekeeper-per-daemon`

Package G2: `s-gatekeeper` was one constant in every daemon on the box, every daemon shares one
store, and two of them appended to `s-gatekeeper#t0`. The id is now
`s-gatekeeper-<workspace key>` (`letibot_sessionlog::gatekeeper_session`) and the store keeps a
`session_writer` row (schema 32) so a second OPEN is refused by name instead of half-writing.

## A heartbeat that only DATES a claim cannot decide liveness

The row has `heartbeat_ms`, and **nothing ticks it** — it is the claim's timestamp. So "the holder
has not beaten recently" says nothing about whether the process lives. The first cut of the
takeover rule reused `mergequeue::lease::gone`'s shape (`age > TTL` **and** pid dead) and that
refused a restart for the whole TTL:

    prompt_history_resume: `the resumed snapshot lost 'first prompt' — Up would not recall it: []`

— a real `harnessd` child killed by the test's `Drop`, its claim a second old, the next daemon
refused. The rule is now: **on this host the pid is the whole fact** (alive ⇒ never taken over,
dead ⇒ taken over at once, cutoff `u64::MAX`); a claim from ANOTHER host expires on
`TTL_MS` alone. `mergequeue::lease::LEASE_TTL_MS` is still the number.

**`u64::MAX as i64` is `-1`.** The column is an INTEGER and the compare-and-swap in SQL is
`heartbeat_ms < ?cutoff`, so the cutoff is clamped to `i64::MAX` in
`Store::claim_session_writer` — without the clamp "take it over" is a comparison that never fires,
which looks exactly like a rule that does not work. Pinned by
`store::writers::tests::the_largest_cutoff_means_take_it_now`.

## The claim's identity is per CLAIM, not per process

`owner = host/pid/<now_ns>`. Two `Sessions` over one store file inside one test process must
contend the way two daemons do, and a pid cannot tell them apart. `sessions::writer_tests::
a_second_session_cannot_write_one_transcript` is the whole guard, driven through `Sessions`.

## `merge_review.session_id` is the CHILD that did the work, NOT the review host

`mergequeue::review::enqueue_finished` writes `session_id: handle` — the `task_start` child whose
branch is up for review. `sessions/gatekeeper.rs`'s module doc says *"the session that reviewed
it"*, and that is **wrong** about this column (the column is not the host anywhere in `serve.rs`;
the host is `Pool::host`, written nowhere on the row). So **no review row ever names the old
`s-gatekeeper` id**, and item 2 of the brief needed no migration at all:

* `Store::recover_queue` (right after a lease claim) moves a `reviewing` row whose **owner** is a
  gone daemon back to `waiting`, clearing `reviewer`;
* `RepoQueue::review_upkeep` moves a `reviewing` row whose **reviewer child** is not in this
  daemon's registry back to `waiting`, counting an attempt — and a child of the old id
  (`s-gatekeeper-sub-…`) is exactly that, because no daemon of this build holds it.
  `serve/tests.rs::a_reviewing_row_whose_reviewer_is_not_alive_is_re_seated_on_the_next_pass` now
  carries an old-id reviewer and asserts the same sentence.

## Tests that "simulate a restart" by opening a second harness are now refused

Four, each fixed by dropping the first (which is what a restart is):
`restart_restore::a_session_reopened_by_the_daemon_that_wrote_its_records_restores_nothing`,
`prefix_fingerprint::an_unchanged_prompt_is_silent_and_a_changed_one_names_reseat`,
`gatekeeper::tests::the_session_is_started_once_and_not_twice`, and `prompt_history_resume`
(a real child process, so the dead-pid takeover had to be right — see above).

## `Sessions::open` is a private field

A test that needs `&mut Harness` (to append a row without a model: `note_to_model`) must live in a
**child module of `sessions`** — hence `crates/harnessd/src/sessions/writer_tests.rs`, next to
`gatekeeper/tests.rs` and `review_pass.rs`, and not beside `crate::writer`.
