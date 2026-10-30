<!-- abstract: MEASURED 2026-10-30: a head files a warning under (code, ts, detail-hash), so a second publish with a bigger count is a SECOND row — "one row per outage with a live counter" is impossible daemon-side, and the honest shape is one row written when the outage ends. -->

# A warning row cannot be revised, so the counter is written when the outage ends

MEASURED 2026-10-30, branch `agent/one-retry-row-with-a-counter-again` (the operator: *"i dont need
walls of this … a coutner is what is need"*, three `model_endpoint_retry` rows in three seconds for
one endpoint).

## The fact that decides the design

`App::note` files a warning under **`note_key`** = `w/"<code>"|"<ts>"|<fnv1a(detail)>`
(`crates/tui/src/app/notes.rs`), and `holds()` refuses only an IDENTICAL triple. So a daemon that
re-publishes the same code with a bigger count and the latest sentence is not updating a row — it is
adding one. **There is no daemon-side "revise this row"**: `SessionEvent` has no live/replace
variant, transcript rows are append-only by trigger, and the only in-place update in the whole head
is `Note::Pane`'s `closed` flag, in `crates/tui/src/app/`.

So a counter whose number climbs **while the outage lasts** needs the head. What a daemon can do is
write the row once, where the count is already final.

## What was built, and why the row is at the END

`harnessd::harness::Outage` (private): `{host, attempts, began_ms, last_ms, said}`. Opened by the
first failed attempt, **changed by every later one and published never**; `settle_outage` TAKES the
record after the retry loop and writes exactly one row —

* the round ended on anything but an HTTP failure ⇒ `model_endpoint_answered`:
  `… answered again after 3 attempts / 2m 14s. The last thing it said, at 10:31:25, was: http 502: …`;
* the ladder gave up ⇒ `model_endpoint_retry`: `… did not answer (attempt 3, last at 10:31:25): http 502: …`;
* `attempts == 1` and no answer ⇒ **nothing**: with `http_retries = 0` the turn's own `turn_failed`
  is the whole of the news.

Both sentences are shaped for `notice_card::warning_notice`, which draws `first_clause` of the first
sentence: the count and the time sit before the first `": "`, so the one-line card keeps them and the
provider's sentence goes behind the click.

## The rake, found by reading the loop rather than by a test

**Every exit from the retry loop must settle the record, or a later outage inherits its clock.** The
loop's one early `?` (`self.obtain_key(...)?` in the 401/403 arm) returns out of `rounds()` with the
record still in the field — and the next turn's first failure then CONTINUES it, so a 45-minute-old
`began_ms` lands in a fresh "answered again after 2 attempts / 45m" row. It is a `match` now, and the
settle runs before the return.

Also: `outage_failed` is called AFTER the key arm on purpose — a key the provider refused is that
card's news, not the endpoint's weather, and counting it would have a key card followed by a
"answered again after 1 attempt" row.
