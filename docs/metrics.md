# Metrics and operator feedback

`~/.local/share/letibot/metrics.db` is an append-only event file beside the session store, written by every
daemon and read by `letibot stats`. It exists because the decisions of the design period were each made from a
hand query, a log grep or one anecdote, and each should have been a `SELECT`.

On by default; `[metrics] enabled = false` in `providers.toml` makes `emit` a no-op. The file is local, mode
0600, and holds no content except the operator's marks and the kinds named in `[metrics] snippets` (§2.8). The
code is `crates/metrics` (the row, the channel, the writer, the schema, the reader),
`crates/harnessd/src/instruments.rs` (every emit site in the daemon, one function each),
`crates/sessionlog/src/metrics_tap.rs` (the hub's own events, and the one place a mark is placed) and
`crates/letibot/src/stats.rs` (`letibot stats`, `letibot fb`).

## 1. Why the facts were not already queries

**The store** (`sessions.db`) records *state*: a session's fill, a job's state and a queue row are last-value
columns, overwritten in place, so "how full was this an hour ago" is unanswerable and a job's `elapsed_ms` is
joined to no turn. `adjudication` and `queue_transition` are the two append-style exceptions and they are good:
metrics points at them and does not copy them.

**Per-workspace JSONL** has second resolution, no turn id, no daemon or build, and a worktree gets its own copy;
`context-claims.jsonl` writes `window: 999999`, the sentinel, which makes the fill fraction uncomputable. **The
daemon log** carries no timestamp on any line, so it answers "did it happen" and never "when" or "how long".
**The hub's `SessionEvent`** already carries about half of what this design records - turn start and end with
usage and timings, tool start and finish with bytes and spill, warnings by code, decisions, subagent and job
transitions, queue moves - timestamped by arrival, bounded, and lost on restart. Nothing persisted it.

Two rules follow, and they are the whole design: **the hub's stream is the source where it already says the
thing** (a tap in `Hub::publish` costs a clone and a send), and **nothing on the worker's path may touch SQL**.

## 2. Design

### 2.1 A sibling database, not a table in the store

1. **Write contention.** The store is one writer lock shared by every daemon on the box, and "sqlite: database
   is locked" is how the queue thread has died. Tens of thousands of rows a day must not join that lock.
2. **Schema versioning across mixed builds.** A table in the store is a migration and a `schema_version` bump,
   and an older daemon meeting a newer store refuses to open it. A kind-tolerant table (§2.2) lets a build that
   knows fewer kinds write beside one that knows more.
3. **Retention.** Pruning and vacuuming are cheap on a small sibling file, and they fragment the 3 GB store
   every transcript read walks.
4. **Privacy.** The file holds no content by default (§2.8), so it can be copied without a prompt.

The cost is no cross-file foreign keys: analysis joins with `ATTACH 'sessions.db' AS s` and the shared ids
(`session_id`, `transcript_id`, `turn_id`, `request_id`, `entry_id`) are strings. A dangling id is acceptable -
these are observations, not the record of truth.

### 2.2 The event table

`crates/metrics/src/store.rs`'s `DDL`, schema 1. Every statement is `IF NOT EXISTS`: every daemon on the box
opens this file and the first one creates it.

```sql
CREATE TABLE event (
    id            INTEGER PRIMARY KEY,   -- rowid; append order within the file
    ts_ms         INTEGER NOT NULL,      -- wall clock AT THE OBSERVATION, not at the write
    dur_ms        INTEGER,               -- the span, for span events; NULL for points
    daemon        TEXT NOT NULL,         -- host/pid/start_ms, the form queue_transition.by uses
    build         TEXT NOT NULL,         -- git sha of the binary: mixed builds are normal here
    session_id    TEXT,                  -- NULL for daemon-level events (memwatch, worker)
    root_id       TEXT,                  -- the session tree's root, denormalised so a tree's
                                         -- events are one index range and no parent walk
    transcript_id TEXT,
    turn_id       TEXT,                  -- a turn is a prompt; its id is its first round's
    round         INTEGER,               -- 0-based round within the turn
    kind          TEXT NOT NULL,         -- dotted: 'turn.end', 'tool.call', 'delivery', ...
    name          TEXT,                  -- the subject: tool, warning code, door, model, verb
    outcome       TEXT,                  -- ok | error | refused | interrupted | ...
    n1 INTEGER, n2 INTEGER, n3 INTEGER,  -- the kind's three headline numbers (§2.3)
    attrs         TEXT                   -- small JSON; no content unless snippets are on
);
CREATE INDEX event_kind_ts    ON event (kind, ts_ms);
CREATE INDEX event_session_ts ON event (session_id, ts_ms);
CREATE INDEX event_root_ts    ON event (root_id, ts_ms);
CREATE INDEX event_turn       ON event (turn_id) WHERE turn_id IS NOT NULL;

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);  -- schema, salt, retention
CREATE TABLE daily (                                           -- rollups, kept forever
    day TEXT, kind TEXT, name TEXT, model TEXT, outcome TEXT,
    n INTEGER, sum_dur_ms INTEGER, p50_dur_ms INTEGER, p95_dur_ms INTEGER, max_dur_ms INTEGER,
    sum_n1 INTEGER, sum_n2 INTEGER, sum_n3 INTEGER,
    PRIMARY KEY (day, kind, name, model, outcome)
);
```

**Typed in Rust, generic in SQL.** A site builds one `Event` - kind, name, outcome, three `n` slots, an `attrs`
list - and the writer projects it onto the row. Readers get **one temporary view per kind** naming the slots
(`VIEWS`, same file), made by the reader rather than migrated, so a new kind is never an `ALTER`:

The views are `turn_start`, `turn_end`, `round_event`, `tool_call`, `delivery`, `wake`, `worker_cmd` and
`feedback`; a kind without one is queried from `event`. Why not a table per kind: many kinds times mixed builds
times migrations is the store's problem again. Why not pure JSON: `SUM(n1)` over a month must not parse JSON.

### 2.3 Kinds - the vocabulary

`crates/metrics/src/lib.rs`'s `kind` module is the list. `dur_ms` is the span where a row is a span; everything
not in `n1/n2/n3` is in `attrs`.

| kind | name | outcome | n1 / n2 / n3 | attrs |
|---|---|---|---|---|
| `turn.start` | the door: `prompt`, `message`, `wake`, `nag`, `census`, `bang`, `reseat`, `fallback` | - | fill tokens / window / rounds | model, cause |
| `turn.end` | model | `eos`, `length`, `aborted`, `wall`, `stalled`, `loop-bound`, `failed` | prompt / cached / output, summed over rounds | door, rounds, tool_calls, tool_errors, ttfb_ms, cost_micros_usd |
| `round` | model | finish reason | prompt / cached / output | ttfb_ms, prompt_ms, predicted_ms, ledger_tokens, server_prompt_tokens, id_slot, provider |
| `tool.call` | tool | the outcome's own tag: `ok`, `abstained`, `backgrounded`, … | inline bytes / full bytes / peak KiB | cap_kib, oom_kills, repairs, edit, spilled |
| `delivery` | `job`, `child`, `monitor` | `immediate`, `batched`, `starved`, `idle` | batch size / rounds run | rule, handle, mid_turn, door |
| `wake` | first cause: `queue`, `monitor`, `result`, `due`, `held`, `none` | `ran`, `empty` | notices gathered | causes, each count |
| `nag` | `plan-check` | `sent` | - | row, row_status, operator_row |
| `nag.followup` | `plan-check` | `touched`, `untouched` | - | row, status_before, status_after |
| `compaction` | `pre-wall`, `refused-door`, `operator` | `ok`, `failed` | before / after, shown unit | before_ledger, after_ledger, unit |
| `wall` | - | `stopped` | resident / window / rounds | unit, ledger_resident, ledger_window, continues |
| `wall` | - | `helped`, `escalated`, `stuck`, `released` | before / measured / window | fold, next, unit |
| `context.claim` | - | `slapped`, `repeated`, `true-report`, `at-wall`, `unmeasured` | resident / window / score x1000 | snippet |
| `worker.cmd` | the verb | `ok`, `compacted`, `failed`, `ignored`, `handed-on` | queued_ms | identity |
| `worker.pass` | `sweep`, `room`, `todo-check`, `queue-census`, `park`, `review` | - | rows touched | - (over 250 ms only) |
| `mem.sample` | - | - | rss / heap live / heap peak, MiB | threads, sessions, heads, harness_mib |
| `warning` | the code | `routine`, `failure`, `refused` | - | - |
| `operator.action` | the command, `attach`, `detach` | - | - | identity, mid_turn, head |
| `subagent` | role | state | - | child, model |
| `job` | the handle | settled state | produced bytes | - |
| `decision.answer` | who answered | option id, `cancelled`, `timed-out` | - | req_id, late |
| `queue.move` | queue | to_state | - | entry_id |
| `operator.feedback` | `good`, `bad`, `note` | - | - | note, tags, item_id, head, identity, at_state, tool |
| `metrics.dropped` | - | - | events dropped | - |
| `metrics.suppressed` | the suppressed kind | - | held back / seen | key, budget |

`turn.start` is written with the turn's end and stamped with the moment the turn **began**, so a turn's two
rows are one write and the start row still says when.

### 2.4 Retention

Raw rows are kept 90 days (`[metrics] keep_days`), rolled into `daily` and deleted once a day on the writer
thread; `daily` is kept forever. **Every `operator.feedback` row and every row of a turn a mark points at are
never deleted** - they are the labelled data. Age alone does not bound the file, which is what §2.9 is about.

### 2.5 Operator feedback - marking a moment live

Marks are the label every other number is calibrated against, so they are the one kind whose content is stored.
`/fb <note>` marks now - the running turn and its round, or the last finished one; `/fb+` and `/fb-` set the
polarity; `#words` are tags, counted as written. `alt-g` / `alt-b` mark without typing, and a second press within
five seconds opens the composer pre-filled (`alt-b` is readline's word-left everywhere else, so it only marks on
an empty composer). Scrolled back, `/fb` marks the row the view is held on. With no head attached, `letibot fb
--session S [--turn T] [+|-] "note"` writes the same row straight into the file.

The row carries `ts_ms` from the head's clock at the press (a clock more than a minute off the daemon's is not
believed and the receipt time is used), the turn and round, the item id, the tags, the note, the head and
identity, and `at_state` - `streaming`, `tool`, `waiting-approval` or `idle`, with the open call if any.

It travels on `ClientFrame::Feedback` and is answered **on the server's connection thread** by
`metrics_tap::mark`, never through `hub.submit`'s command queue: the worker can be pinned for tens of seconds
and a mark that waits lands on the wrong turn. The hub then publishes a routine `Warning { code: "feedback" }`,
which every head draws, so the operator sees it was taken. `mark` is also the one place that knows which turn a
transcript row belongs to (the last 4,096 rows), so a mark on an older row keeps the row id and says it has no
turn.

### 2.6 Degradation signals - designed, not built

Testable only if "degraded" is a number. The proxies the design names are computable from text the writer
already sees and would live in `turn.end`'s `attrs.deg`: glyph and markup density, self-repetition (8-token
shingles against the previous five turns), opener and closer repeat, tool error rate, empty rounds, R71's excuse
score, an ignored nag, reasoning share, guard trips, output per round at equal fill. **None of it is
implemented**, so Q9 in §3.1 answers nothing yet.

### 2.7 The recorder

- **Emission** is `letibot_metrics::emit(Event)`: build the struct, `try_send` it on a bounded channel
  (`CHANNEL`, 8,192 rows). No lock shared with the store, no SQL, and it never blocks: on a full channel the
  event is dropped and a counter incremented, reported later as `metrics.dropped`.
- **One writer thread per daemon**, with its own `rusqlite::Connection` (WAL, `synchronous=NORMAL`,
  `busy_timeout` 2 s), committing one transaction per second or per 1,000 rows. A failed commit is tried once
  more, then the batch is dropped and counted: metrics must never wedge the daemon.
- **The tap** is one line in `Hub::publish`: every `SessionEvent` reaches `Tap::observe`, whose first branch is
  the `enabled()` check. It emits warnings, operator commands, attach and detach, subagent states, job
  settlements, decisions answered and queue moves, and keeps the session's "now" - turn, round, what it is doing
  - so a mark is placed without asking the worker. The high-rate events (`Delta`, `TokensGenerated`,
  `PromptProgress`, `JobOutput`) only move the "now".
- **Direct emits** where the hub carries nothing (§4): rounds and tool calls, turn start and end, deliveries,
  wakes, nags, compaction and wall, context claims, worker commands and passes, memwatch samples.
- **A turn, for metrics, is a prompt.** Every round the harness runs for one door is one turn, whose id is the
  engine id of its first round; every round of one prompt carries the same `began_ms`, which is how the tap
  tells a new round from a new turn. The harness keys its own rows the same way, which is why a mark and the
  turn it marks join on `turn_id`.

### 2.8 Privacy

Nothing leaves the box: local, 0600, no network path. **No content by default** - ids, counts, sizes, durations,
codes and salted hashes (salt in `meta`, per box); no prompt, no answer, no tool arguments, no command lines, no
warning detail. `[metrics] snippets` names the kinds that may keep up to 200 characters as `attrs.snippet` and
the writer strips a snippet from every other kind; the default is `context.claim` alone, because R71's floor is
tuned from the excuse sentence. The other exception is the operator's own mark (§2.5).

### 2.9 The two caps - a rate and a size

Age is not a bound on the file: a loop that writes a thousand rows a second is ninety days of that. So the writer
holds two limits, both on the writer thread - **not** in the emitter, because coalescing means holding events back
until an interval closes, which a fire-and-forget `try_send` cannot do.

**The rate cap.** Key `(session_id, kind, name, outcome)`. Budget `[metrics] rate_per_sec`, default 20 rows per
key per second, five times the fastest honest key in the tree (`worker.pass` has a 250 ms floor and cannot exceed
4/s; `round` and `tool.call` run at about 0.2/s); `0` is no cap. Under it the rows are byte-identical to what the
writer wrote before the cap existed. Over it, the rest of that second becomes one row of the same kind with
`attrs.suppressed` = the count held back and `attrs.window_ms`, and **`n1..n3` left NULL**, so
`SUM(attrs->>'suppressed')` beside `COUNT(*)` stays the truth and `SUM(n1)` is not polluted. A suppressed key
also writes one `metrics.suppressed` row per minute and one log line; it wakes nothing, because a metrics problem
must not become a turn.

**The size cap.** `[metrics] max_mb`, default 2048, checked every 10 s and at start - not on the prune's day,
because a storm can grow the file a gigabyte an hour. Over it: roll the oldest whole day into `daily`, delete it
in 50,000-row id ranges at most 250,000 rows and 1,024 vacuumed pages a pass, repeat until the live size
(`page_count - freelist_count`) is under 80 % of the cap, then vacuum and checkpoint. **The cap beats the age
rule, deliberately** - the rows it deletes may be minutes old, and `daily` keeps the counts. Marks and the rows of
marked turns are still never deleted.

Four things a reader will otherwise "simplify" back: `daily` holds exactly what `event` no longer has, so the
rollup covers only the deleted rows and ADDS to a group rather than `INSERT OR IGNORE`-ing it; the ceiling is the
oldest DELETABLE row, not the oldest row, or a cap whose oldest row is a mark can never pass; `PRAGMA
incremental_vacuum` yields one row per page it frees, so the rows must be drained or one statement frees one page;
and `auto_vacuum` cannot be switched on after the first table, so an older file needs one `VACUUM` (the writer
says so at start, and `Writer::shrinks()` is how a test stands in that state). Neither cap needs a schema change,
so `SCHEMA` stays 1.

## 3. Analysis

### 3.1 The questions, and the query for each

Against `metrics.db`, with `ATTACH '.../sessions.db' AS s` where the store is joined. `:since` is the
millisecond bound `letibot_metrics::stats::floor` computes.

**Q1 (R79) - what arrives mid-turn, by which rule, how long is it held?** Group the `delivery` view by `what`,
`how` and `rule` over `COUNT(*)`, `AVG(held_ms)`, `SUM(mid_turn = '1')` and `AVG(batch)`.

**Q2 - wake turns by cause:** group `wake` by `cause` and `outcome` over `COUNT(*)` and `SUM(items)`.

**Q3 (c12d044) - worker queue wait per verb, per build.**

```sql
SELECT build, verb, COUNT(*) n,
       MIN(CASE WHEN pr >= 0.95 THEN queued_ms END) p95_queued_ms,
       MAX(queued_ms) max_queued_ms, SUM(queued_ms > 5000) over_5s
FROM (SELECT build, verb, queued_ms,
             PERCENT_RANK() OVER (PARTITION BY build, verb ORDER BY queued_ms) pr
      FROM worker_cmd WHERE ts_ms > :since)
GROUP BY 1, 2 ORDER BY max_queued_ms DESC;
```

**Q4 (R71)** - excuses by model and fill decile: group `kind = 'context.claim'` by `name`,
`n1 * 10 / NULLIF(n2, 0)` and `outcome`, with `AVG(n3) / 1000` as the score. **Q5 (R72/R73)** - compaction
accuracy: group `kind = 'compaction'` by `name` and `outcome` over `n1`, `n2`, `attrs->>'before_ledger'` and
`AVG(dur_ms)`. **Q6 (R77)** - does a nag work: join `kind = 'nag'` to `kind = 'nag.followup'` on `turn_id`,
average `outcome = 'touched'`, group by `attrs->>'operator_row'`.

**Q7 - latency, throughput and R72's two counts side by side:** group `round_event` by `model` and `provider`
over `AVG(ttfb_ms)`, `output_tokens * 1000 / predicted_ms`, `cached_tokens / prompt_tokens` and
`ledger_tokens / prompt_tokens` - the last being the R72 discrepancy as one column, the box's count of the
prompt against the number the provider reported for it. **Q8 - tool reliability:** group `tool_call` by tool over
`AVG(outcome <> 'ok')`, `MAX(dur_ms)`, `AVG(peak_kib)`, `AVG(attrs->>'cap_kib')` and a count of calls within
10 % of the cap.

**Q9 - degradation (§2.6)** answers nothing until `attrs.deg` exists. **Q10 to Q15:** memory (R66) is
`mem.sample`'s rss, heap live and `attrs.harness_mib` per daemon over time - a daemon whose rss climbs while the
heap does not is pointing at SQLite or the vocabulary, not at Rust. Queue reliability (R61/R76) is time in each
state from the store's `queue_transition`, `LEAD(at_ms) OVER (PARTITION BY queue, entry_id ORDER BY seq)`
against `at_ms`. Adjudication and the oracle come from the store's `adjudication` table as it is. Cost is
`SUM(cost_micros_usd) / 1e6` per day and model. Operator load is approvals asked per hour and interrupts per
turn. Feedback is `letibot stats feedback`.

### 3.2 `letibot stats` - the CLI

Opens `metrics.db` read-only; never writes, never starts a daemon. `--since 2d|12h|30m|all` (default `1d`),
`--db PATH` (default `$LETIBOT_METRICS_DB`, else `~/.local/share/letibot/metrics.db`).

```
letibot stats [VERB] [--since 1d] [--session S] [--db PATH]

  (none) | today          turns, rounds, tool calls, tool errors, cost, worker wait p95 and max,
                          deliveries and how many mid-turn, wakes that ran, nags, compactions,
                          marks good / bad / note, events dropped
  waits                   worker queue wait per command verb, per build (c12d044)
  deliveries              results reaching the model: rule, door, mid-turn; wakes by cause
  tools                   tool calls: error rate, duration, memory peak against cap
  models                  time to first token, tok/s and cache hit per model
  turns [--limit N]       the turns, newest first, with their marks
  feedback [--bad]        every mark with its turn's numbers
  session ID              one session's timeline
  sql 'SELECT ...'        anything, with the per-kind views loaded (read-only)
```

The bare verb is `today`, so `letibot stats` is the one command to remember, and `stats sql` is the door for a
question this table has no verb for.

## 4. Instrumentation points

Every site in the daemon calls **one function in `crates/harnessd/src/instruments.rs`**, on one line, so a merge
that moves the site moves one line and the arithmetic stays put. The hub's own events are the tap (§2.7); the
rest are direct emits.

| instrument | function | site |
|---|---|---|
| turn start and end | `turn_begins`, `turn_ends` | `turn_tail::run_turn` |
| each round | `TurnMeter::round` via `Harness::meter_round` | `harness/turns.rs` |
| each tool call | `TurnMeter::tool` | `harness/turns.rs` |
| time to first byte | `TurnMetrics::first_byte_ms` | the engine, at the first token |
| deliveries (R79) | `delivered_all` | `DeliveryLog::record` |
| wakes | `wake` via `Harness::meter_wake` | `Harness::wake` |
| worker queue wait | `worker_cmd` with a `Clock` | `Daemon::run`, `sessions.rs` |
| slow idle passes | `worker_pass` | the idle arm: sweep, room, todo-check, queue-census, park, review |
| compaction and wall | `compaction`, `operator_compaction`, `wall`, `wall_change` | `Harness::meter_compaction`, `Sessions::compact`, the turn tail, `harness/wall.rs` |
| nags | `TurnMeter::nag` via `Harness::meter_nag` | `harness/nags.rs`; the follow-up by `turn_ends` |
| context claims | `context_claim` | `harness/turns.rs` |
| memory samples | `mem_sample` | `memwatch`, every fifth tick and at every census or bound line |
| warnings, operator actions, subagents, jobs, decisions, queue moves | the tap | `Hub::publish` |
| marks | `metrics_tap::mark` | the `ClientFrame::Feedback` arm of `serve_conn` |

`instruments::start_writer` runs from `cli.rs` before any session opens, so a daemon either records from its
first turn or says why it cannot.

## 5. What the design names and the tree does not have

The degradation proxies (§2.6) and Q9. The live page: `letibot stats export` to a file a page reads, and
`letibot stats serve` for a LAN page polling the file - neither verb exists. The per-workspace JSONL files are
dual-written as events but not retired (`notes-offers.jsonl` and `context-claims.jsonl` still exist). A `stats`
line on the head's status bar.

## 6. The instrument and the decision it unblocks

Each of these was a decision taken from an anecdote; the kind is what turns it into a query. `worker.cmd` and
`worker.pass` settle whether a command waited tens of seconds on the worker (c12d044), per command and per build.
`turn.start` and `turn.end` are the denominator for everything else, and `round` adds latency, tok/s, cache hit
and R72's provider-versus-ledger count. `tool.call` gives error rate, duration and memory peak against cap (R65);
`delivery` and `wake` give R79; `compaction` and `wall` give R72/R73; `nag` and `nag.followup` give R77;
`context.claim` gives R71; `mem.sample` gives R66. `warning`, `operator.action` and `subagent` are one tap line
covering every warning code, operator command and subagent transition, and `operator.feedback` is the label that
filters every analysis above to the turns the operator marked.

## 7. Open decisions

1. **The page**: export plus a static artifact, or `letibot stats serve`. Neither is built.
2. **Retention**: 90 days of raw rows, rollups and marked turns forever. Longer is a `[metrics]` line and a
   bigger `max_mb`.
3. **The degradation proxies** (§2.6): which are worth building, and whether a proxy that does not separate
   marked-bad turns from unmarked ones is a finding worth acting on.
