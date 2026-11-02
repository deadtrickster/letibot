<!-- abstract: The claim's own acknowledgement was sent through `waiting` into `Claim.waits_on`, the one field `remind::the_room_is_working_on` reads to decide the holder must still act, so every claim in every room went quiet; protocol 5 splits it into `Claim.said`. -->

# The acknowledgement was written into the field that silences the reminder — MEASURED 2026-11-01, branch `agent/the-edge-does-not-say-working` (`main` at `4d617b78`).

`remind::the_room_is_working_on` is `!stale && waits_on.is_some()`, and `SessionRoom::ack`
(`crates/harnessd/src/room/link.rs:406`) sent the holder's "seen; working on it" as a
`Request::Waiting`, which writes `waits_on`. So a holder that had declared NO wait still read as
waiting on something, and the nag never fired: the pass wrote rows and then filtered all of them out.

The fix is two fields, not a better predicate: `Request::Acknowledge` → `Claim::said` (what the holder
is DOING), `waiting` keeps `waits_on` (what it is WAITING on), `Holding` carries both.
`ROOM_PROTOCOL_VERSION` went 4 → 5, and `the_room_speaks_protocol_four` is the pin that moves with it.

`stale` is the TURN's liveness, not the item's: `link.rs`'s `turn_started`/`turn_ended` beat once for
the whole session, so a session that claims `ram` and then works on something else keeps the claim
fresh. Only a declared wait can make a row quiet.
