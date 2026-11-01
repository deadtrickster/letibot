<!-- abstract: MEASURED 2026-11-01: `main`'s red `room_reminders` was the claim's self-acknowledgement written through `waiting` into `Claim.waits_on` — the one field the reminder reads to decide the holder must still act — so every claim in every room read as "nothing outside moves this" and the nag never fired; protocol 5 splits it into `Claim.said`. -->

# The acknowledgement was written into the field that silences the reminder — MEASURED 2026-11-01, branch `agent/the-edge-does-not-say-working` (`main` at `4d617b78`).

`main` was RED at `cargo test -p letibot-harnessd --test room_reminders`, 0.12 s, alone: a holder that
had declared **no** wait got `TodoNeed::Room { working: true }`. Measured with a temporary `eprintln!`
in `remind::rows`, on the failing test, before any change:

```text
Holding { item: 1, tag: "ram", body: "8 GB, ~10 min", claim_ms: 1793492998486,
          beat_ms: 1793492998486, held_ms: 1,
          waits_on: Some(Wait { text: "seen #1; working on it", since_ms: 1793492998487 }) }
```

`the_room_is_working_on` is `!stale && waits_on.is_some()`. So the *wait* was the claim's own
acknowledgement — `SessionRoom::ack` (`crates/harnessd/src/room/link.rs`), landed by `b1675aca`
("a claim that acks itself") and sent through `Request::Waiting`. The brief's suspect `4d617b78` is a
NOTES commit; the bell's `route`/`missed` rewrite is not implicated (`stale: false` there was right).

## The rake

**A field that makes a reminder quiet is a claim about what is NOT yours to move; an acknowledgement
is the opposite claim and must never be written into it.** `Claim::waits_on` had one reader with a
decision in it (`remind::the_room_is_working_on`) and one writer that meant something else, and
nothing type-checked the difference: the effect was that *every* claim went silent, which is the
nag machine in reverse — the pass wrote rows and then filtered all of them out.

The fix is two fields, not a better predicate: `Request::Acknowledge` → `Claim::said` (what the
holder is DOING), `waiting` keeps `waits_on` (what it is WAITING on), `Holding` carries both.
ROOM_PROTOCOL_VERSION 4 → 5, and `server.rs`'s `the_room_speaks_protocol_four` is the pin that has to
move with it (the tree spells the number once, as a literal, on purpose).

## Two more things in this area that read as activity but are not

* **`stale` is the TURN's liveness, not the item's.** `link.rs`'s `turn_started`/`turn_ended` start
  and stop one beat for the whole session, so `beat_ms` moves while *any* turn runs — a session that
  claims `ram` and then works on something else keeps the claim fresh. Nothing in the room can tell.
  Only the declared wait can make a row quiet, which is why `working` needs both halves.
* **A reconnect gap rings a bell.** `note_gap` pushes a sentence and rings, i.e. an ABSENCE starts a
  turn — deliberate (the room keeps no log, so *you missed N* is the only honest answer), but it is
  a wake with no event behind it.
