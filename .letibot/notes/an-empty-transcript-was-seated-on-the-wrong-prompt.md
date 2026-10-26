<!-- abstract: MEASURED 2026-10-26: the append-only trigger never checked row 0's offset, so a daemon that opened a zero-row transcript appended its first rows at ITS prefix length (s-gatekeeper#t0: row 0 at 32162 in a 24407-token transcript); the repair needs the daemon's prefix row to still be in the store. -->

# An empty transcript was appended under the prompt the NEW build composes — and the trigger could not see it

MEASURED 2026-10-26 against `~/.local/share/letibot/sessions.db` (read-only), branch
`empty-transcript-prefix`.

## The hole, and why nothing caught it

`transcript_item_append_only_insert` (`crates/tokencore/src/store.rs`, the trigger in
`SCHEMA_SQL`) checks three things: `seq` is `MAX(seq)+1`, `tok_offset` continues the
previous row **`WHERE NEW.seq > 0`**, and `tok_len == LENGTH(tokens)/4`. **Row 0 has no
previous row, so row 0's `tok_offset` was never checked by anything.** A transcript whose
stable prefix is 24407 tokens accepted fourteen rows starting at 32162; each one is
correctly hashed and the chain is contiguous *from 32162*, so the only reader that
notices is `TokenLedger::restore`, whose `cursor` starts at `prefix_tokens.len()`:
`row 0 starts at 32162, should start at 24407`.

## Where the wrong offset came from

`Harness::open`'s `None` arm (`crates/harnessd/src/harness.rs`, `resumed` is `Some` only
when `st.items > 0`): for a session stored with zero rows it hardcoded `#t0`, seated the
ledger on **the prefix this daemon composes**, and — because `transcript_exists("#t0")` was
true — rewrote neither `transcript.stable_prefix_id` nor the session's
`prefix_fingerprint`. The store then named one prefix and held rows written under another.

Both prefixes are in the store: the rows' chain reproduces exactly from
`58f93de4…` (32162 tokens, the prefix a *2026-10-25 build* composed) while the row names
`37f6974f…` (24407). That is what makes the repair possible — verified by re-chaining all
14 rows in python from `h_init` upward.

## The fix (branch `empty-transcript-prefix`)

- **Prevention, option (a) re-seat**: an empty transcript on another prompt has
  `transcript.stable_prefix_id` moved to today's prefix (`Store::set_transcript_prefix`,
  which *refuses* when a row exists) and its fingerprint rewritten. (b) — keep the stored
  prompt — buys nothing: with no rows there is no chain and no cache, and the first turn
  prefills cold either way.
- **The anchor**: `Store::assert_anchored`, from `append_item` and `append_items`, refuses
  a first row whose `tok_offset` ≠ prefix length + rows so far. It is Rust, not the
  trigger: adding it to `SCHEMA_SQL` would only reach a **fresh** database, because
  `CREATE TRIGGER IF NOT EXISTS` and the schema is at v30 — closing it in SQL needs a
  version bump and a migration arm.
- **Repair**: `crates/harnessd/src/harness/prefix_repair.rs` — on the exact shape (row 0
  past the end of the named prefix, `found > expected`), find a stored prefix of exactly
  `found` tokens whose `h_init` re-chains every row, then **fork** the session onto it
  carrying items and tokens verbatim (no re-render), old transcript untouched. Unsealed
  then sealed, like every other fork.

## Fixtures that were wrong and now fail loudly

`Store::append_item` checks the anchor, so any hand-built store with a prefix of N tokens
and rows starting at 0 is now refused. Fixed: `tokencore/store/tests.rs`
(`kinds_are_denormalised_correctly` used `TokenLedger::new(&tr, &[])` against a 4-token
prefix), `harnessd/tests/continue_picks.rs`, `harnessd/src/cli.rs`'s `rows_for`,
`harnessd/tests/transcript_tool.rs`. A fixture for the *broken* state has to write its rows
through `store.connection()` raw SQL — that is what
`harnessd/src/harness/tests/prefix_on_open.rs` does.

## Still open

`cargo test -p letibot-turn --test restore` keeps failing on `s-gatekeeper#t0` until a
daemon running this build **opens** that session (the repair lives on `Harness::open`, and
no test may point at the real store). Nothing else in the store is affected: 18 other
transcripts restore.
