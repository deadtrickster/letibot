<!-- abstract: The append-only trigger checks `tok_offset` only for rows after row 0, so a daemon opening a zero-row transcript appended its first rows at the prefix IT composes - and the repair forks onto the prefix the rows were really written under. -->

# An empty transcript was appended under the prompt the NEW build composes — and the trigger could not see it

`transcript_item_append_only_insert` (the trigger in `SCHEMA_SQL`, `crates/tokencore/src/store.rs`)
checks `seq`, checks that `tok_offset` continues the previous row `WHERE NEW.seq > 0`, and checks
`tok_len`. Row 0 has no previous row, so row 0's offset was checked by nothing: a transcript whose
stable prefix was 24407 tokens accepted fourteen rows starting at 32162, each correctly hashed and
contiguous FROM 32162, and only `TokenLedger::restore` noticed.

Where it came from: `Harness::open`'s `None` arm (`resumed` is `Some` only when `st.items > 0`) took a
session stored with zero rows, hardcoded `#t0`, seated the ledger on the prefix this daemon composes,
and - because `transcript_exists("#t0")` was true - rewrote neither `transcript.stable_prefix_id` nor
the fingerprint.

Three parts to the fix. An empty transcript on another prompt has its prefix re-seated
(`Store::set_transcript_prefix`). `Store::assert_anchored` refuses a first row whose `tok_offset` is
not prefix length plus rows so far - in Rust, not in the trigger, because `CREATE TRIGGER IF NOT
EXISTS` only reaches a fresh database. `crates/harnessd/src/harness/prefix_repair.rs` then finds a
stored prefix whose `h_init` re-chains every row and forks the session onto it.
