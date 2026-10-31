<!-- abstract: MEASURED/DECIDED 2026-10-31: the census now counts what a session's HARNESS holds (transcript text, token region, trail) with a per-session ceiling of max(1 MiB, window × 16 + 144 KiB); the CI plateau test measures 1.02 heap bytes per row byte; and the brief's 50 × 200 × 1 MiB probe is an `--ignored` test plus a live-daemon reading — with the finding that a turn's own door never consults the wall. -->

# R66's full probe: what to run, what to read, and which number proves which answer

MEASURED/DECIDED 2026-10-31, branch `agent/the-census-names-the-session`, following
`e8fe1613` (a head's queue held to a byte budget). The census now reports what a session's
HARNESS holds — `Footprint` on the hub, printed per session by `memwatch` — so the 113 GB
question has a name on it. This note is the instrument for the scale that does not fit in CI.

## What the census now counts, in one line each

| term | counts | does not |
|---|---|---|
| `items_text_bytes` | `TranscriptItem::bytes` summed over `Session::items` — the one definition the view's byte bound already uses | ids, enums, struct overhead, the `Vec`'s spare capacity |
| `ledger_region_bytes` | the token region's MAPPED bytes (`TokenRegion::bytes`) = capacity × 4 + header | pages not yet written; the ledger's rows and hashes |
| `trail_bytes` | the authorisation trail's text, capped at 60 utterances × 600 chars since this branch | the `Said` structs around them |
| `ceiling_bytes` | `max(1 MiB, window × 16 + 144 KiB)`, or 64 MiB for a session with NO window | — |

`window × 16` is 8 bytes of text a token (a real vocabulary averages ~3.5; a byte vocabulary is
1) plus 8 bytes of region a token (4 per `TokenId`, doubled because capacity doubles). For this
box's ordinary 32 768-token session the floor wins at 1 MiB; for a 200 000-token window it is
3.3 MiB. Multiply by the 492 sessions one repository's daemon held before R68.3 and the ceilings
sum to ~2 GB — which is the arithmetic's own answer about 113 GB: **it was never sessions sitting
at their ceiling.**

## Half A — the synthetic probe (50 sessions × 200 turns × 1 MiB), behind `--ignored`

```sh
R66_SESSIONS=50 R66_TURNS=200 R66_TURN_BYTES=1048576 \
  cargo test --release --offline -p letibot-harnessd --test footprint_plateau \
  -- --ignored --nocapture
```

Needs ~15 GiB (10 GiB of rows plus their token regions, which are `RssShmem`) and ~20 minutes;
raise the tool's memory cap or run it on a box with room. A quick smoke run of the same code:

```sh
R66_SESSIONS=3 R66_TURNS=6 R66_TURN_BYTES=65536 \
  cargo test --offline -p letibot-harnessd --test footprint_plateau -- --ignored --nocapture
```

MEASURED at that smoke scale (3 × 6 × 64 KiB): heap 2.5 → 3.6 MiB, Σ footprint 7.7 MiB,
**1.03 bytes of heap per byte of row**, slope 0.99× (early vs late), every session inside its
ceiling. The CI-scale sibling (`the_heap_plateaus_at_what_the_sessions_hold`, 6 × 12 × 64 KiB)
measures 1.02 bytes of heap per byte of row and a 0.75× late/early slope.

### What each number proves

| number | 1.0–1.75 means | a number above means |
|---|---|---|
| `VERDICT 1`: heap growth ÷ bytes appended | the daemon holds the work and nothing else | a term above the rows — **this is where R66 lives** |
| `VERDICT 1b`: Σ region ÷ Σ tokens | 4 (at capacity) to 8 (just after a doubling), and `RssShmem` grew | the capacity arithmetic is wrong, or the region is never written |
| `VERDICT 2`: per-turn cost late ÷ early | flat — a turn costs one row | growth that feeds on itself (a per-turn copy of the conversation) |
| `VERDICT 3`: worst session vs its ceiling | the transcript is bounded | the session `memwatch` names with `BOUND BROKEN` |

### The one thing half A cannot answer

It drives `Harness::system_update`, which **does not consult the wall**: compaction is attempted
at the between-turn doors (`wake`, `nag_turn`, `run_after_reseat`, `continue_after_fallback`) and
on a provider's refusal. So half A measures the UNBOUNDED case — and its answer, if VERDICT 1 is
≈1 and VERDICT 3 says NO, is *"the daemon holds exactly the work, and the work has no bound but
the session ending"*. That is a finding about the wall, not about the census.

## Half B — a live daemon, where every turn is followed by a wake

```sh
grep -E 'memwatch: (rss|BOUND BROKEN|CENSUS)' ~/logs/harnessd-<id>.log | tail -40
```

Read three things:

1. **the once-a-minute line's `harness <total>`** — the sum of every live session's footprint. If
   it climbs while `sessions` is flat, a session is growing and the census block names it.
2. **`sessions whose harness holds most (of its own ceiling)`** — the block the census prints when
   resident memory crosses a step. The top row is the answer to *which session grew*; the three
   terms under it say which PART (transcript, region, trail).
3. **`BOUND BROKEN in <session>: this session's harness holds X against its Y ceiling`** — once
   per session and per bound (`memwatch`'s `said` set), and published to the session as a
   `memory_bound_broken` warning so the person sees it where they are working.

The arithmetic to apply to a live daemon: `Σ footprint ≈ sessions × (window × ~5)` for sessions
sitting at their wall (text 1×, region 4× with a byte vocabulary), and `≈ sessions × 2 × window ×
3.5` for a real vocabulary. If the log's `harness` total is a large multiple of that, the extra is
a session past its wall — which is what the arm names.

## Rakes

* **`lsof`/`RssAnon` will not show the region.** A memfd's pages are `RssShmem`; measured on
  lab2x1, 256 MiB into a fresh memfd reads back `RssAnon: 3140 kB`. R66's `anon-rss:113190136kB`
  can never be a token region, however far it grows.
* **A count over bodies of variable size is not a bound.** The trail held every utterance for the
  life of the session while reading only the last 60 — `TRAIL_SCAN` is now both, and each stored
  utterance is clipped to the 600 characters `trail()` already emitted.
* **A restatement that misses a site is stale, not wrong.** The gauges are rewritten by a full walk
  at every `reconcile` (every append, and the compaction fork's swap), and the plateau test
  compares them to a fresh walk at every session — so a missed site is a red test, not a silent
  number.
* **A prompt turn's own rows do not pass the wall.** `submit_items_then` appends and runs the
  rounds; only the between-turn doors call `compact_if_at_the_wall`. A fixture that drives
  `system_update` therefore never compacts, whatever `context_window` says.
