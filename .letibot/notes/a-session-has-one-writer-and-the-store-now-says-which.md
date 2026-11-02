<!-- abstract: One writer per session, claimed at `Harness::open` and recorded in `session_writer`; on this host the holder's pid is the whole liveness fact, and `u64::MAX as i64` is why the cutoff is clamped. -->

# One writer per session, and the review queue is per workspace — MEASURED 2026-10-26, branch `gatekeeper-per-daemon`

The gatekeeper id is per workspace (`letibot_sessionlog::gatekeeper_session`), and
`Store::claim_session_writer` (`crates/tokencore/src/store/writers.rs`, schema 32) refuses a second
open of a session by name. `crates/harnessd/src/writer.rs::claim` is its one caller.

`session_writer.heartbeat_ms` is never ticked, so its age decides nothing. The rule: on this host
the pid is the whole fact - alive is never taken over, dead is taken over at once (cutoff
`u64::MAX`) - and another host's claim expires on `TTL_MS` alone.

`u64::MAX as i64` is `-1`, and the compare is `heartbeat_ms < ?cutoff`, so the cutoff is clamped to
`i64::MAX`; without the clamp "take it over" never fires and reads as a rule that does not work.
Pinned by `store::writers::tests::the_largest_cutoff_means_take_it_now`.

The owner is `host/pid/<now_ns>`, not the pid, so two `Sessions` over one store file contend the way
two daemons do: `sessions::writer_tests::a_second_session_cannot_write_one_transcript`.

`merge_review.session_id` is the `task_start` child whose branch is up for review
(`mergequeue::review::enqueue_finished`), not the session that reviewed it.
