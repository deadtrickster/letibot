<!-- abstract: Nothing on `Hello`, `SessionWiring` or `QueueSnapshot` names the daemon a head is attached to - `QueueSnapshot.here` is the repository - so "is this claim ours?" can only be answered by comparing a row's `host/pid` with the repository's lease holder. -->

# The head has no daemon identity, so "this daemon" is the repository's lease holder

`QueueReview.owner`, `QueueMerge.owner` and `QueueLeaseView.owner` are one shape:
`mergequeue::lease::this_daemon()` (`crates/harnessd/src/mergequeue/lease.rs:58`) mints
`host/pid/started_ms` once per process, so a row's claim and the lease's claim are comparable as
`host/pid`, which is what `owner_word` (`crates/tui/src/ui/panes/queue.rs:267`) draws.

Nothing on the wire says which daemon the head is talking to. `Hello`, `SessionWiring` and
`QueueSnapshot` (`crates/sessionlog/src/event.rs:235`) carry no such field - `QueueSnapshot.here` is the
REPOSITORY the asking session is a session of, filled per connection - and `hostname` appears nowhere
in `crates/tui`.

So the only answerable question is *is this row claimed by whoever holds this repository's lease?* A
real "is this us?" needs the daemon to put its own `host/pid` in the snapshot, which an added, defaulted
field does without a protocol bump.
