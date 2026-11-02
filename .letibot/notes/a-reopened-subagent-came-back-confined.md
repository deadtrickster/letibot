<!-- abstract: A `task` child's row records the SEAT (`coder`) and not its standing, so a parked child reopened from the store came back confined - silently on Linux, and as an unopenable session on a Mac. -->

# A reopened subagent came back confined, because `unconfined` is inherited at the spawn and nowhere recorded

A `task` child is `Seat::Coder` unless the call names a role, and its session row records that seat. `Config::unconfined` is set for `Seat::Leticode` and inherited by its subagents, because `sub_cfg` is built with `..self.base.clone()` and the parent's base already had it. So a leticode session's coder child is unconfined at the spawn only.

The reopen goes through `Harness::open`, which reads the row's `role` (`coder`) and takes `unconfined` from `Sessions::base`, which is false. On Linux that loses the boundary silently; on a Mac the confined backend cannot be built at all, so every leticode session's parked child was unreopenable.

The fix is `unconfined_by_inheritance` (`crates/harnessd/src/harness/open.rs`): a session whose row has a parent stands where its ancestors stand, walking `parent_session_id` for a recorded `leticode`. A walk that cannot read a link leaves the standing unproven, which is confined.

Rake: the reopen answers "was reopened" while the worker's open fails and the message is dropped as `message_idle`, so a test that says nothing about why needs `eprintln!` on the wait loop and every `SessionEvent::Warning`.
