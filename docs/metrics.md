# Metrics and operator feedback — design

Status: **design, not built.** Filed 2026-10-11 against main `b23a76a`. The TODO item that
schedules phase 1 points here.

The operator, 2026-10-11, verbatim: *"we need metrics design, there is simply not enough data to
make decisions like this. i can't even live feedback so we can analyze sessions, and we need
metrics - how often and what delivered mid turn etc etc etc, with times and usual. and not only
for this topic. MORE METRICS"*.

Two asks, and this document treats them as one system:

1. **Metrics** — every decision filed in R61–R79 was made from a hand-run query, a log grep, or
   one anecdote. Each of those decisions should have been a `SELECT`.
2. **Live feedback** — the operator cannot mark a moment of a session as it happens (*"that was
   a distraction"*, *"this is where it got dumb"*, *"good"*), so nothing ties the operator's
   judgement to the numbers. A mark is an event like any other, and it is the most valuable one:
   it is the label every other metric is calibrated against.

---

## 0. What decisions were made without data — the requirements

Each row is a decision the 2026-10-11 work took, and the number that would have settled it.

| Decision | What was used | The number that was missing |
|---|---|---|
| R79 — hold job/child results instead of delivering mid-turn | one sentence from the operator (*"i think you guys are getting distracted"*) | per delivery: arrived, delivered, by which door, and what the model did in the next round (did it switch to the job, abandon the row it was on) |
| R71 — slap "my context is full" | a 40-row capped regex over the store; calibration hand-labelled once | excuse rate per turn by model and by fill; whether the turn after the slap continued; false-positive rate from operator marks |
| R72/R73 — which unit the wall is judged in | three numbers read off three surfaces (740k / 937k / 3.45M) | per round: provider-reported prompt tokens vs ledger count vs window, and per compaction before/after in both units |
| R66 — 113 GB daemon | one OOM report; memwatch exists but writes untimestamped log lines | rss/heap per daemon over time, joined to turns, sessions open and compactions |
| R61/R76 — queue reliability | a hand review of 22 rows; history cleared once by a commit | `queue_transition` exists now (R61) — but no gate step durations or reviewer latency joined to it |
| R77 — plan-check effectiveness | one operator report | per nag: which row it named, and whether the next turn touched that row |
| "clear context fixes degradation" | the operator's impression | degradation proxies per turn against fill, turns since compaction, and turns since a clear |
| c12d044 — 29–89 s worker queue waits | found by hand with `perf` on a scratch daemon | per command: enqueue → dequeue → done on the worker, every command, always |
| cost | `cost_micros_usd` on the wire, summed nowhere | per turn cost by provider/model, per day |

---

## 1. Inventory — what is recorded today, where, and what it can answer

Measured 2026-10-11 ~20:50 on lab2x1, read-only.

### 1.1 The store — `~/.local/share/letibot/sessions.db`

3.67 GB main file + 346 MB WAL, schema version **24**, shared by **11 running `harnessd`
processes** of possibly different builds (`pgrep -a harnessd`). Tables, row counts, and what each
can answer:

| Table | Rows | Records | Can answer | Cannot answer |
|---|---|---|---|---|
| `session` | 853 | one row per session; `role`, `parent_session_id`, `model_id`, `provider_choice`, `created_at`; **last-value** context columns `context_tokens`, `context_cached`, `context_ledger`, `context_window`, `auto_compact_*` | sessions/day (307 on 10-10, all but 3 subagents), role mix, the tree, current fill | fill **over time** — the columns are overwritten; nothing about turns |
| `transcript` | 999 | one per transcript; forks (`parent_transcript_id`, `forked_at_seq`) | how many compactions/forks a session took | when, why, how long, before/after sizes |
| `transcript_item` | ~310,600 (`max(rowid)`); 2.8 GB+ | every item, `kind`, `item_json`, tokens, `created_at` | content-level forensics; item rate (~50k items on 10-10 by rowid sampling) | anything without a scan — **`created_at` is not indexed** (c12d044's lesson), so per-day/kind counts are a 3 GB walk; no turn id, round, latency, usage |
| `adjudication` | 74,636 | one row per gated tool call: `tool`, `tier`, `action`, `effect`, `verdict_by`, `p_allow`, **`oracle_ms`**, `oracle_model`, `consulted`, `operator_kind`, `operator_latency_ms`, `decided_ms`, `turn_id` | the best-instrumented thing in the system: decisions/day (13,800 on 10-10), oracle latency (mean 89–2200 ms/day, max 300,000 = the timeout), operator agree/disagree (`upheld` 23,706, `granted` 18,005, `revoked` 20) | the tool call's own duration and outcome; the `turn_id` joins to nothing persisted |
| `job` | 1,424 | one row per background job: `command`, `state`, `produced`, `elapsed_ms`, `updated_ms` | jobs/day (526–544 on 10-09/10), exit mix, elapsed | **324 rows say `running`** — last-value, never closed when a daemon died; when the result was *delivered* (R79) |
| `todo` | 43 | one board per session, last value | the current plan | the plan's history; which row a nag named |
| `review_queue`, `merge_queue` (R61) + `merge_queue_v23`, `merge_review_v23` (legacy) | 16 / 3 / 22 / 21 | queue rows, last value; `gate_steps_json` | current queue state | — |
| `queue_transition` (R61) | 134 (all 10-11) | **append-only** history: `queue`, `entry_id`, `from_state`, `to_state`, `at_ms`, `by`, `why` | time in each state, transitions/day, who moved what | gate step durations (they are in `gate_steps_json` on the row, overwritten) |
| `target_health`, `queue_lease` | 2 / 2 | last value | — | — |
| `stable_prefix` | 154 | prefixes | prefix churn | — |

Two design facts from §4.4 of `docs/implementation-plan.md`: the plan **named** an `event` table
(*"the session log, `(session_id, seq)`"*) and a `turn_metrics` table (*"per request:
prompt/cached/predicted tokens, finish_reason, prompt_ms, predicted_ms, draft_n…"*), and
`crates/tokencore/src/store.rs` deliberately left both out for their strands (W6, W7) to write.
**Neither was ever written.** `letibot_turn::TurnMetrics` (`crates/turn/src/metrics.rs:41`) is
computed per request and dropped; the wire's `TurnFinished{usage, timings}` is a lossy copy
(TODO section 4, T13 gap 2).

### 1.2 Per-workspace JSONL — `<workspace>/.letibot/*.jsonl`

Found by `find / -xdev -path '*/.letibot/*.jsonl'` (excluding `docs/evidence` copies):

| File | Writer | Lines | Fields | Can answer | Cannot |
|---|---|---|---|---|---|
| `notes-offers.jsonl` | `harnessd::notes_offer` | letibot 321, pg-noop 17, leticl 3 (+ worktree copies) | `at` (s), `session`, `path`, `score`, `taken` | offer rate, take rate by score | which turn; whether the note helped |
| `context-claims.jsonl` (R71) | `harnessd::context_claim` | pg-noop 3 | `at` (s), `session`, **`sentence` (content)**, `score`, `resident`, `window`, `at_wall`, `slapped`, `repeated` | excuse rate and fill at the excuse | what the model did after the slap; the turn. Note `window: 999999` on all three rows — the sentinel, not the real window — so the fill fraction is not computable from this file |

Both are per workspace, second-resolution, no turn id, no daemon/build, and scattered (a worktree
gets its own copy). They cannot be joined to anything but the session.

### 1.3 The daemon log — `~/logs/harnessd.log`

8.3 MB, 87,331 lines. **No line carries a timestamp** (the only dates in the file are inside 429
bodies). Not every daemon writes here (605 memwatch lines in the file against 11 daemons × 1/min),
so it is one launch's redirect, not the fleet's log. Line shapes, by frequency:

- `  s-N · <who> -> N round(s), N tool call(s)` — one per turn end (2,539 lines; **mean 17.7
  rounds and 18.1 tool calls per turn**). Session and counts; no time, no model, no tokens.
- `memwatch: rss N MiB · heap live N MiB (peak N MiB) · N large …` — every 60 s
  (`LETIBOT_MEMWATCH_SECS`), plus `LARGE alloc` backtraces and census blocks. No time.
- `  todo check -> N session(s)` — the idle plan-check sweep.
- `compacting: N of N tokens resident, less than the N the next turn needs` /
  `compacted: N tokens resident now, was N` / `context wall: stopped after N round(s) at N of N
  tokens` — compaction and wall, with numbers, without time or duration.
- `merge queue: …` refusals, `sqlite: database is locked` (the queue thread dying), `head
  connection ended`, `refused request shape`, `http 429` bodies, and the start banner (~70 lines
  per daemon start; 385 starts in the file).

Can answer: "did it happen, and how often in this file". Cannot answer: **when**, which build,
any duration, anything per round.

### 1.4 In memory only — the hub log

Every session's `Hub` (`crates/sessionlog/src/hub.rs`) carries a typed event stream,
`SessionEvent` (`crates/sessionlog/src/event.rs:1201`), that heads render: `TurnStarted
{model, began_ms}`, `TurnFinished {finish_reason, usage{prompt, cached, predicted,
cost_micros_usd}, timings{prompt_ms, predicted_ms, wall_ms}}`, `ToolStarted`/`ToolFinished
{outcome, inline_bytes, full_bytes, spill, repairs}`, `DecisionRequested`/`DecisionAnswered`,
`Warning {code, detail, compaction}` (135 codes classified in `crates/sessionlog/src/warning.rs`),
`Subagent`, `JobSettled`, `QueueMoved`, `CommandIssued {identity, command}`, `HeadAttached`,
`CompactionProgress`, `TurnInterrupted`, `TurnFailed`. **This stream already contains about half
of what this design wants to record**, timestamped by arrival, and it is bounded and lost on
restart. Nothing persists it.

### 1.5 Summary of the gap

| Want | Today |
|---|---|
| per-turn tokens, latency, tok/s, cost | computed (`TurnMetrics`, `TurnFinished`), never persisted |
| per-tool duration, outcome, bytes, memory peak | on the wire and in the result text; never persisted as numbers |
| deliveries mid-turn / wakes (R79) | nothing — not even a log line |
| nags and what followed (R77) | nothing |
| compaction before/after/duration (R72/R73) | untimestamped log lines; `CompactionReport` on the wire |
| context fill per round | last value on `session` only |
| worker queue wait (c12d044) | nothing; found with `perf` |
| memory over time (R66) | untimestamped log lines from one daemon in eleven |
| errors/warnings by code | on the wire, lost |
| operator actions | `CommandIssued` on the wire, lost; adjudication answers persisted |
| operator feedback | **no way to give it** |
| adjudication + oracle | **persisted, good** — reuse, don't duplicate |
| queue transitions | **persisted (R61), good** — reuse |

---

## 2. Design

### 2.1 Where the events live — a sibling database, `metrics.db`

`~/.local/share/letibot/metrics.db` (WAL, mode 0600), next to `sessions.db`, **not a table in
it**. Four reasons, each measured:

1. **Write contention.** The store is one writer lock shared by 11 daemons; *"merge queue: the
   queue could not be served: sqlite: database is locked"* is in the log three times, each one
   the queue thread ending for good (TODO section 4, merge-queue review (a)). Instrumentation at
   ~60k rows/day must not add itself to the lock every transcript commit waits on.
2. **Schema versioning across mixed builds.** A table in the store is a migration and a
   `schema_version` bump; nine reviews failed on *"store schema is version 23; this build knows
   21"* because an older daemon met a newer store. A metrics file that is append-only with a
   kind-tolerant shape (§2.2) lets a build that knows fewer kinds keep writing beside one that
   knows more, and no store migration is needed for phase 1 at all.
3. **Retention.** Raw events are pruned (§2.4). `DELETE` + incremental vacuum on a ~1 GB sibling
   is cheap; on the 3.7 GB store it fragments the file every transcript read walks.
4. **Privacy and export.** The file holds no content by default (§2.8), so it can be copied,
   exported to a page, or attached to a bug without carrying a single prompt.

Cost: no cross-file foreign keys. Analysis joins with `ATTACH 'sessions.db' AS s` (read-only),
which is how the stats CLI opens it; ids (`session_id`, `transcript_id`, `turn_id`, adjudication
`request_id`, queue `entry_id`) are shared strings. The events are observations, not the record
of truth; a dangling id is acceptable and countable.

What stays where it is: `adjudication` and `queue_transition` are already good append-style
records in the store. The event table gets a thin pointer event where a join needs time order
(e.g. `adjudication.decided` carries `request_id` and `oracle_ms`), not a copy of the row.

### 2.2 The event table

```sql
-- metrics.db, schema 1
CREATE TABLE event (
    id          INTEGER PRIMARY KEY,      -- rowid; append order within the file
    ts_ms       INTEGER NOT NULL,         -- wall clock AT THE OBSERVATION (not at write)
    dur_ms      INTEGER,                  -- for span events: the span; NULL for points
    daemon      TEXT NOT NULL,            -- host/pid/start_ms — the form queue_transition.by uses
    build       TEXT NOT NULL,            -- git sha of the binary (mixed builds are normal here)
    session_id  TEXT,                     -- NULL for daemon-level events (memwatch, worker)
    root_id     TEXT,                     -- the root of the session tree; denormalised so a
                                          -- tree's events are one index range, no parent walk
    transcript_id TEXT,
    turn_id     TEXT,                     -- the harness's turn id (TurnStarted.turn_id)
    round       INTEGER,                  -- 0-based round within the turn
    kind        TEXT NOT NULL,            -- dotted: 'turn.end', 'tool.call', 'delivery', ...
    name        TEXT,                     -- the subject: tool name, warning code, rule, model, door
    outcome     TEXT,                     -- ok | error | refused | interrupted | dropped | ...
    n1 INTEGER, n2 INTEGER, n3 INTEGER,   -- the kind's three headline numbers (see the
                                          -- per-kind table): indexed-free, cheap to SUM
    attrs       TEXT                      -- JSON object, small (< 512 B), the rest of the kind's
                                          -- typed fields; never content unless snippets are on
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

**Typed in Rust, generic in SQL.** The writer takes one Rust enum, `metrics::Event`, one variant
per kind with typed fields; `kind`, `name`, `outcome`, `n1..n3` are projected from it and the rest
is serialised into `attrs`. Readers use one SQL view per kind that names the fields (`CREATE VIEW
turn_end AS SELECT …, n1 AS prompt_tokens, n2 AS cached_tokens, n3 AS output_tokens,
attrs->>'ttfb_ms' AS ttfb_ms …`), shipped by the stats CLI rather than migrated, so a new kind is
a new variant and a new view — never an `ALTER`. An older build that does not know a kind simply
never writes it; a reader that does not know a kind skips it.

Why not one table per kind: 25 kinds × mixed builds × migrations is the store's problem again.
Why not pure JSON: `SUM(n1)` over a month must not parse JSON per row; three integer slots cover
every kind's headline numbers.

### 2.3 Kinds — the vocabulary

`n1/n2/n3` are listed per kind; everything else is `attrs`.

| kind | name | outcome | dur_ms | n1 / n2 / n3 | attrs |
|---|---|---|---|---|---|
| `turn.start` | door: `prompt`/`message`/`wake`/`nag`/`bang`/`task`/`continue` | — | — | fill tokens / window / turns since compaction | model, provider, wake cause (`job`/`child`/`monitor`/`due`), queued_ms (time from the door's trigger to the turn start) |
| `turn.end` | model | `eos`/`length`/`aborted`/`interrupted`/`failed`/`wall`/`stalled` | wall | prompt / cached / output tokens (summed over rounds) | rounds, tool_calls, tool_errors, ttfb_ms (first round), tok_s, reasoning_tokens, cost_micros_usd, provider, fill_end, the degradation block (§2.6) |
| `round` | model | finish reason | request→done | prompt / cached / output | ttfb_ms, prompt_ms, predicted_ms, provider-reported prompt (R72's unit) vs ledger count, id_slot, n_busy_slots, retries, http status of retries |
| `tool.call` | tool name | `ok`/`error`/`refused`/`timeout`/`killed`/`spilled` | dispatch→result | inline bytes / full bytes / memory peak KiB | memory cap KiB, repairs, adjudication request_id, background (bool), exit code, edit (bool) |
| `delivery` (R79) | what: `job`/`child`/`monitor`/`operator`/`peer` | `immediate`/`held`/`batched`/`starved`/`idle`/`dropped` (rule `turn_end` is `batched`; `idle` is a result that arrived between turns and was never held) | arrived→delivered | batch size / rounds the turn had run / held count | rule (`plan-depends`/`failure`/`monitor`/`emergency`/`starvation-cap`/`routine`), handle, door it went through (`steer`/`wake`/`meanwhile`), mid_turn (bool), next_round_touched (bool, filled by the follow-up event) |
| `wake` | cause | `ran`/`coalesced`/`empty` | trigger→turn start | coalesced count | handles |
| `nag` | `plan-check`/`child-check`/`intent`/`mid-reasoning`/`progress` | `sent`/`suppressed` | — | open rows / ladder step / — | named row id, operator_row (bool), next_turn_touched_row (bool, follow-up) |
| `compaction` | `pre-wall`/`pre-emptive`/`refused-door`/`operator`/`restart` | `ok`/`refused`/`failed` | start→done | before / after (provider unit) / halves | before/after in ledger unit, summary tokens, cut-off seq, rounds of the summary turn |
| `wall` | — | `stopped`/`continued` | — | resident / window / headroom | continues used, compaction attempted |
| `context.claim` (R71) | — | `slapped`/`repeated`/`true-report`/`at-wall` | — | resident / window / score×1000 | — (sentence only with snippets on) |
| `context.fill` | — | — | — | resident / window / cached | per round, sampled into `round` instead when rounds exist; a separate event only for idle changes (compaction, restart) |
| `adjudication` | tool | effect | oracle_ms | p_allow×1000 / — / — | request_id, tier, verdict_by, consulted (pointer to the store row) |
| `decision.answer` | `operator`/`oracle`/`timeout` | outcome | requested→answered | — | req_id, late |
| `queue.move` | queue | to_state | time in from_state | — | entry_id, from_state (pointer; `queue_transition` stays the record) |
| `gate.step` | step name | `ok`/`fail`/`timeout` | step | — | entry_id, target, sha |
| `worker.cmd` | verb (`CommandKind::verb`) | `ok`/`failed`/`ignored` | dequeue→done | queued_ms / queue depth at enqueue / — | session, client_request_id |
| `worker.pass` | `review`/`sweep`/`todo-check`/`memwatch` | — | pass | rows touched | only when dur_ms > 250 (otherwise counted in `daily`) |
| `subagent` | role | `started`/`answered`/`failed`/`parked`/`killed` | start→end on the end event | turns / tokens out / — | parent, model, depth |
| `job` | how | exit state | runtime | produced bytes / exit code / — | handle, delivered_at (pointer to its `delivery`) |
| `mem.sample` | — | — | — | rss MiB / heap live MiB / heap peak MiB | threads, sessions, hub log items, heads, job rows; every 5 min + every census step |
| `warning` | code | class (`routine`/`failure`) | — | — | — (detail only with snippets on) |
| `error` | where | — | — | — | error kind (http status, sqlite code, io kind) |
| `operator.action` | `prompt`/`interrupt`/`approve`/`deny`/`new`/`clear` (when a clear-context verb exists — there is none today; the head's `/new` is nearest)/`compact`/`model`/`mode`/`take-back`/`todos`/`attach`/`detach`/`bang` | — | — | prompt chars / — / — | head kind (tui/leticl/http), identity, mid_turn (bool) |
| `operator.feedback` | polarity: `good`/`bad`/`note` | — | — | — | see §2.5 |
| `metrics.dropped` | — | — | — | events dropped since last | the writer's own health |

### 2.4 Volume, size and retention — from today's activity

Measured counts (10-10 is the busiest full day in the store), and the rows they imply:

| Source of the estimate | Per day (10-10) | Event kinds it drives | Rows/day |
|---|---|---|---|
| adjudication rows (≈ every tool call is adjudicated: 73,784 admit of 74,636) | 13,800 | `tool.call` + `adjudication` pointer | ~27,600 |
| tool calls ÷ 18.1 per turn (log mean) | ~760 turns | `turn.start`, `turn.end`, `operator.action`, `nag`, `wake` | ~3,000 |
| 17.7 rounds per turn | ~13,500 rounds | `round` | ~13,500 |
| `job` rows | 526 | `job`, `delivery` | ~1,100 |
| `session` rows | 307 | `subagent` (start + end) | ~600 |
| `queue_transition` | 134 (10-11) | `queue.move`, `gate.step` | ~400 |
| 11 daemons × memwatch every 5 min | — | `mem.sample` | ~3,200 |
| worker commands (prompts, slashes, reads) + slow passes | estimate | `worker.cmd`, `worker.pass` | ~2,000 |
| warnings (not persisted today; R71 budget, compaction, slash replies …) | estimate | `warning` | ~2,000 |
| **Total** | | | **~50–55k rows/day** |

At ~200 bytes per row with indexes, **~10–12 MB/day**. Retention: raw events **90 days**
(~1 GB at today's peak rate), configurable as `[metrics] keep_days`; `daily` rollups forever
(a few thousand rows/day, ~0.5 MB); **every `operator.feedback` event and every event of a turn
a feedback event points at is kept forever** — they are the labelled data. A prune runs once a
day on the writer thread (`DELETE … WHERE ts_ms < ? AND turn_id NOT IN (SELECT turn_id FROM
event WHERE kind='operator.feedback')`, then `PRAGMA incremental_vacuum`).

### 2.5 Operator feedback — marking a moment live

**What the operator does.** In the tui head:

- `/fb <note>` — a note on **now**: the current turn if one is running (and its current round),
  else the last finished turn. `/fb+ <note>` and `/fb- <note>` set polarity (`good` / `bad`);
  plain `/fb` is `note`. Tags are words starting `#` inside the note: `#distracted`, `#excuse`,
  `#dumb`, `#slow`, `#loop`, `#ignored-nag`, `#good` — free vocabulary, counted as written.
- **One key** for the commonest case, no typing: `alt-b` (bad) / `alt-g` (good) marks *now* with
  no note and flashes a one-line confirmation; a second press within 5 s opens the note prompt
  pre-filled `/fb- `. (Key choice is the operator's — open decision 4.)
- **On a past row**: with the view held (R56) and a row selected, `f` opens `/fb` against **that
  row** — the item id, its turn and round — so a mark can be left on the moment it went wrong
  while reading back.
- `/fb list` shows this session's marks; `/fb rm ID` withdraws one (an `operator.feedback`
  event with `outcome = 'withdrawn'` — append-only, like everything else).

In leticl: the same `/fb` line and the same selected-row action, sending the same frame (leticl's
side is its own item; until it lands, `letibot fb` below is its door). From a shell, with no head attached: `letibot fb --session S [--turn T]
[-|+] "note"` writes the same event (useful from the stats page or a phone over ssh).

**What is recorded** (`kind = 'operator.feedback'`):

| field | value |
|---|---|
| `ts_ms` | **the head's** timestamp at the key press, carried on the frame — not the daemon's receipt time |
| `session_id`, `root_id`, `transcript_id` | the session the head is in |
| `turn_id`, `round` | the turn running at the press, or the selected row's turn |
| `name` | `good` / `bad` / `note` |
| `attrs.item_id`, `attrs.seq` | the transcript row it points at (the selected row, or the last row in view) |
| `attrs.tags` | the `#words` |
| `attrs.note` | **the operator's own text** — stored, because writing it is the point; it is the one content field that is on by default, and it is the operator's words, not the model's |
| `attrs.head`, `attrs.identity` | which head, which person (`dead`, `leticl`) |
| `attrs.at_state` | what the session was doing: `streaming` / `tool` / `waiting-approval` / `idle`, and the open tool call if any |

**How it travels — off the worker.** A dedicated frame, `ClientFrame::Feedback { at_ms, turn_id,
item_id, polarity, note }` (a protocol bump, 44 → 45), handled **in the server's connection
thread** (`crates/sessionlog/src/server.rs`, beside the other `ClientFrame` arms) by handing the
event straight to the recorder's channel — it never enters `hub.submit`'s command queue. The
reason is c12d044: the worker can be pinned for tens of seconds, and a mark that waits behind a
review pass lands on the wrong turn. The hub then publishes a routine `Warning {code: "feedback"}`
so both heads draw a small mark in the transcript gutter at that row, and the operator sees it
was taken. (Fallback considered and rejected: riding `CommandKind::Slash` like `/models` needs no
protocol bump, but `Slash` is dispatched by `Sessions::slash` on the worker — exactly the queue to
avoid. Open decision 3.)

**How it is used.** Every analysis in §3 can be filtered to "turns the operator marked bad", and
the degradation proxies (§2.6) are calibrated against marks: a proxy that does not separate
marked-bad turns from unmarked ones is not measuring what the operator calls degradation.

### 2.6 Degradation signals — computed per turn

The operator's working theory is that models degrade over a long session and a "clear context"
fixes it. (There is no `/clear` today — the nearest is the head's `/new`; a clear-context item may be filed beside this one.) That is testable only if "degraded" is a number. Proxies, each computed **on the
writer thread** from text the recorder already sees (the hub's `Delta`/`TranscriptContent`
events), storing **only the numbers**, in the `turn.end` event's `attrs.deg` block:

| Signal | Computation | Why it is a proxy |
|---|---|---|
| glyph density | non-ASCII symbols (emoji, box-drawing, arrows, `✅`-class) per 1k chars of answer text | the visible "decoration creep" of long sessions |
| markup density | headings + bullets + bold spans per 1k chars | the same, in markdown |
| self-repetition | fraction of the answer's 8-token shingles already present in the previous 5 turns' answers (MinHash, 64 hashes kept per turn in memory only) | boilerplate repeats, re-stated plans |
| opener/closer repeat | salted hash of the first and last sentence; repeat count over the session | "Here's what I did:" every turn |
| tool error rate | `tool_errors / tool_calls` this turn | rising error rate is the most direct sign |
| repeated failing call | same tool + same args digest failing twice in a turn | loops |
| empty rounds | rounds with no tool call and < 50 output tokens | stalls before the progress check fires |
| excuse | R71's best score this turn (0 if none) | false context excuses |
| ignored nag | a nag named row R and the next turn did not touch R (todo update or a tool call citing it) | R77 |
| reasoning share | reasoning tokens / output tokens | over-thinking or under-thinking drift |
| guard trips | `repetition_collapse`, progress-check, intent-check, mid-reasoning-check warnings this turn | already detected, never counted |
| output per round, tok/s at equal fill | `round` events | model slowdown vs context growth (separates "dumber" from "slower") |

Analysis then plots each against **fill**, **turns since the last compaction/clear**, and
**session age**, per model — and the before/after of every `compaction` and every operator
`clear` is the natural experiment for the "clear context" question (§3, Q9).

### 2.7 The recorder — cost, threading, batching

The c12d044 lesson: anything on the worker thread's path is paid by every prompt. So:

- **Emission** is `metrics::emit(Event)`: build a small struct and `try_send` it on a bounded
  channel (`std::sync::mpsc::sync_channel(8192)`). No allocation beyond the struct, no lock
  shared with the store, no SQL, **never blocks**: on a full channel the event is dropped and an
  atomic counter incremented; the writer reports the count as `metrics.dropped`. Target cost
  < 2 µs per emit.
- **One writer thread per daemon** (`metrics-writer`), owning its own `rusqlite::Connection` to
  `metrics.db` (WAL, `synchronous=NORMAL`, `busy_timeout=2000`). It drains the channel and commits
  **one transaction per second or per 1,000 events**, whichever first: at ~60 events per turn
  and 11 daemons that is ~11 small transactions/second across the box, on a file nobody else
  locks. A busy or failed commit retries once then drops the batch with a counter — metrics must
  never wedge the daemon.
- **The hub tap** is one line in `Hub::publish` (`crates/sessionlog/src/hub.rs:869`): a
  filtered clone of each `SessionEvent` (the high-rate `Delta`, `TokensGenerated`,
  `PromptProgress`, `JobOutput` excluded) is `try_send`-ed to the writer, which maps the ones in
  §2.3 to events (turn, tool, warning, decision, subagent, job, queue, command, attach,
  compaction). The mapping and the degradation arithmetic (phase 3, from `TranscriptContent` of
  the turn's answer) run on the writer thread, so they cost the publishing thread a clone and a
  send.
- **Direct emits** only where the hub carries nothing: deliveries, wakes, nags, round-level
  TTFB/usage, fill, worker command waits, memwatch samples, gate steps. Each is one `emit` line.
- **Writes per turn**: ~2 turn + ~18 round + ~18 tool + ~18 adjudication pointers + a few
  warnings/deliveries ≈ **60 rows, ~12 KB, inside one or two batched transactions**.
- **Off switch**: `[metrics] enabled = false` makes `emit` a no-op (the channel is never
  created). On by default.

### 2.8 Privacy

- Nothing leaves the box: `metrics.db` is local, 0600, no network path writes or reads it; the
  export (§3.3) is a local file the operator chooses to move.
- **No content by default.** Events carry ids, counts, sizes, durations, codes, and salted hashes
  (salt in `meta`, per box, so a hash cannot be dictionary-reversed elsewhere). No prompt, no
  answer, no tool arguments, no command lines, no warning detail.
- **Opt-in snippets**: `[metrics] snippets = ["context.claim", "warning", "tool.call"]` lets the
  named kinds keep up to 200 characters (`attrs.snippet`) — e.g. the excuse sentence R71 needs to
  tune its floor. `letibot stats export` drops snippets unless `--with-snippets`.
- The one exception is the operator's own feedback note (§2.5), which is the operator's text.
- Today's `context-claims.jsonl` already stores the sentence verbatim; phase 1 moves it into a
  `context.claim` event and the sentence becomes a snippet under that opt-in (on by default for
  that kind only while R71's floor is being tuned — open decision 5).

---

## 3. Analysis

### 3.1 The questions, and the query for each

All against `metrics.db`, with `ATTACH '…/sessions.db' AS s` where the store is joined. Views
(`turn_end`, `tool_call`, `delivery`, …) are the per-kind projections of §2.2.

**Q1 (R79) — how often does something arrive mid-turn, of what kind, how long is it held, and
does the model switch to it?**

```sql
SELECT name AS what, outcome AS how, attrs->>'rule' AS rule,
       COUNT(*) AS n,
       SUM(attrs->>'mid_turn') AS mid_turn,
       CAST(AVG(dur_ms)/1000 AS INT) AS avg_held_s,
       ROUND(AVG(attrs->>'next_round_touched'), 2) AS switched_to_it
FROM event WHERE kind = 'delivery' AND ts_ms > :since
GROUP BY 1, 2, 3 ORDER BY n DESC;
```

**Q2 — wake turns caused by routine completions per session per day (R79's before/after).**

```sql
SELECT date(ts_ms/1000,'unixepoch') d, session_id, attrs->>'cause' cause, COUNT(*)
FROM event WHERE kind='turn.start' AND name='wake'
GROUP BY 1,2,3 ORDER BY 1 DESC, 4 DESC;
```

**Q3 (c12d044) — worker queue wait per command, p95/max per daemon build.**

```sql
WITH w AS (SELECT build, name AS verb, n1 AS queued_ms,
                  PERCENT_RANK() OVER (PARTITION BY build, name ORDER BY n1) AS pr
           FROM event WHERE kind='worker.cmd' AND ts_ms > :since)
SELECT build, verb, COUNT(*) n, MIN(CASE WHEN pr >= 0.95 THEN queued_ms END) p95_ms,
       MAX(queued_ms) max_ms, SUM(queued_ms > 5000) over_5s
FROM w GROUP BY 1,2 ORDER BY max_ms DESC;
```

**Q4 (R71) — false context excuses per 100 turns, by model and fill decile; did the slap work?**

```sql
WITH t AS (SELECT turn_id, name AS model, n1*10/NULLIF(attrs->>'window',0) AS fill_decile
           FROM event WHERE kind='turn.start')
SELECT t.model, t.fill_decile, COUNT(DISTINCT t.turn_id) turns,
       COUNT(c.id) excuses,
       ROUND(100.0*COUNT(c.id)/COUNT(DISTINCT t.turn_id),2) per_100
FROM t LEFT JOIN event c ON c.turn_id=t.turn_id AND c.kind='context.claim' AND c.outcome='slapped'
GROUP BY 1,2 ORDER BY 1,2;
```

**Q5 (R72/R73) — compaction accuracy: before/after in both units, duration, refusals.**

```sql
SELECT date(ts_ms/1000,'unixepoch') d, name AS door, outcome, COUNT(*),
       AVG(n1) before_provider, AVG(n2) after_provider,
       AVG(attrs->>'before_ledger') before_ledger, AVG(dur_ms)/1000 avg_s
FROM event WHERE kind='compaction' GROUP BY 1,2,3;
```

**Q6 (R77) — does a nag work?** Share of nags whose named row was touched in the next turn,
operator rows vs model rows.

```sql
SELECT attrs->>'operator_row' op_row, COUNT(*) nags,
       ROUND(AVG(attrs->>'next_turn_touched_row'),2) acted_on
FROM event WHERE kind='nag' AND name='plan-check' AND outcome='sent' GROUP BY 1;
```

**Q7 — latency and throughput per model/provider:** TTFB p50/p95, tok/s, prefill ms per 1k
prompt tokens, at fill buckets.

```sql
SELECT name model, attrs->>'provider' provider, (n1/100000) fill_100k, COUNT(*) rounds,
       AVG(attrs->>'ttfb_ms') ttfb, AVG(n3*1000.0/NULLIF(attrs->>'predicted_ms',0)) tok_s,
       AVG(1.0*n2/NULLIF(n1,0)) cache_hit
FROM event WHERE kind='round' AND ts_ms > :since GROUP BY 1,2,3;
```

**Q8 — tool reliability:** error rate, p95 duration, memory peak vs cap per tool.

```sql
SELECT name tool, COUNT(*) n, ROUND(AVG(outcome<>'ok'),3) err_rate,
       MAX(dur_ms) max_ms, AVG(n3) avg_peak_kib, AVG(attrs->>'cap_kib') avg_cap_kib,
       SUM(n3 > 0.9*(attrs->>'cap_kib')) near_cap
FROM event WHERE kind='tool.call' GROUP BY 1 ORDER BY n DESC;
```

**Q9 — does the model degrade, and does a clear/compaction fix it?** Degradation proxies per
turn by turns-since-reset, per model; and the paired before/after across each reset.

```sql
SELECT s.name model, MIN(s.n3, 40) turns_since_reset, COUNT(*) turns,
       AVG(e.attrs->>'$.deg.tool_err')     tool_err,
       AVG(e.attrs->>'$.deg.glyphs_per_k') glyphs,
       AVG(e.attrs->>'$.deg.self_repeat')  self_repeat,
       AVG(e.attrs->>'$.deg.excuse')       excuse,
       100.0 * COUNT(f.id) / COUNT(*)      bad_marks_per_100
FROM event s
JOIN event e ON e.turn_id = s.turn_id AND e.kind = 'turn.end'
LEFT JOIN event f ON f.turn_id = s.turn_id AND f.kind = 'operator.feedback' AND f.name = 'bad'
WHERE s.kind = 'turn.start'
GROUP BY 1, 2 ORDER BY 1, 2;
```
(`bad_marks_per_100` is the ground-truth column the proxies are judged against.)

**Q10 — memory (R66):** rss and heap per daemon over time, with sessions open and compactions
overlaid; the daemon whose rss climbs without heap climbing points at SQLite/vocab, not Rust.

```sql
SELECT daemon, ts_ms, n1 rss_mib, n2 heap_mib, attrs->>'sessions' sessions
FROM event WHERE kind='mem.sample' AND ts_ms > :since ORDER BY daemon, ts_ms;
```

**Q11 — queue reliability (R61/R76):** time in each state per entry from `queue_transition`
(in the store, unchanged) plus `gate.step` durations.

```sql
SELECT queue, from_state, COUNT(*), AVG(next_at - at_ms)/60000 avg_min FROM (
  SELECT queue, to_state from_state, at_ms,
         LEAD(at_ms) OVER (PARTITION BY queue, entry_id ORDER BY seq) next_at
  FROM s.queue_transition) WHERE next_at IS NOT NULL GROUP BY 1,2;
```

**Q12 — adjudication and the oracle:** oracle latency p95 and timeouts per day, operator
disagreement rate per tier — from the store's `adjudication` table as it is.

**Q13 — cost:** micro-USD per day per provider/model, per root session tree, per 1k output
tokens. **Q14 — operator load:** approvals asked per hour, operator latency, interrupts per turn,
`/clear` and `/compact` frequency. **Q15 — feedback**: every mark with its turn's numbers beside
it; "show me the 20 turns I marked bad and what was different about them".

### 3.2 `letibot stats` — the CLI (proposed)

Opens `metrics.db` read-only and attaches the store read-only; never writes, never starts a
daemon.

```
letibot stats                          # today: turns, rounds, tool calls, errors, cost, p95 waits, marks
letibot stats turns   [--session S] [--model M] [--since 2d]
letibot stats deliveries [--since]     # Q1/Q2
letibot stats waits                    # Q3, per build
letibot stats tools                    # Q8
letibot stats compactions              # Q5
letibot stats excuses | nags           # Q4, Q6
letibot stats degrade [--model M]      # Q9
letibot stats mem [--daemon D]         # Q10
letibot stats feedback [--bad] [--session S]   # Q15: each mark with its turn's numbers
letibot stats session S                # a timeline: one line per turn, marks inline
letibot stats sql 'SELECT …'           # the views are loaded; read-only
letibot stats export --since 7d -o metrics.json [--with-snippets]
letibot fb --session S [+|-] "note"    # the shell door for feedback (§2.5)
```

### 3.3 A live page (proposed)

The operator reads live pages. Two options, not exclusive:

1. **Export + artifact**: `letibot stats export` writes a JSON of the views (rollups + the last
   N days of turns/deliveries/marks); an HTML page (a published artifact) loads that file and
   draws: turns/day by door, the delivery funnel (arrived → held → delivered, by rule), worker
   wait p95 per build, compaction before/after, the degradation curves per model with marks
   overlaid, rss per daemon, cost per day. Refreshed by re-running the export.
2. **A local page served by the CLI**: `letibot stats serve --port 8790` serves the same page
   reading `metrics.db` directly, polling every 5 s — live, LAN only, read-only.

The page's most important panel is the **session timeline**: one row per turn, fill as a line,
the degradation proxies as small sparklines, deliveries and nags as ticks, and the operator's
marks as flags — the place a mark becomes a question.

---

## 4. Instrumentation points

Locations against main `b23a76a`. "Tap" means the hub tap derives it from an existing
`SessionEvent` and no code at the site changes; "emit" means one `metrics::emit` line at the site.

**Threads, as they are** (there is no async runtime; all std threads): the **worker** is
`Daemon::run` (`crates/harnessd/src/daemon.rs:150`) on the daemon's main thread, and every root
session's turns run on it synchronously; each **subagent** has its own `subagent-<id>` thread
(`harness.rs:14172`); each head connection has a `head-conn` thread (`sessionlog/src/server.rs:532`,
`serve_conn` :622); plus `queues` (`mergequeue/serve.rs:1279`), `memwatch`, `job-watch-*` /
`subagent-watch-*` (`jobwatch.rs:642/654`), `monitor-wake-*` (`turn_tail.rs:287`). The store
(`crates/tokencore/src/store.rs`, `SCHEMA_VERSION = 24` :336) opens one connection per owner,
`synchronous=FULL`, `busy_timeout` 30 s (:1583), no writer thread, no batching — every write is
synchronous on its caller's thread. The metrics writer must not copy that pattern.

**The tap point** is `Hub::publish` (`crates/sessionlog/src/hub.rs:869`): one line there hands a
filtered clone of each `SessionEvent` (not `Delta`, `TokensGenerated`, `PromptProgress`,
`JobOutput` — the high-rate ones) to `metrics::tap`, which `try_send`s it. Everything marked
*tap* below is derived on the writer thread from that.

| # | Instrument → kind | Where (main `b23a76a`) | How | Thread it costs |
|---|---|---|---|---|
| 1 | turn start/end → `turn.start`, `turn.end` | `turn_tail::run_turn` (`crates/harnessd/src/turn_tail.rs:120`): `Door` known at entry, clock at :138 (`begin_turn_clock`), end at :163 (`end_turn_clock`); `Reply {metrics: Vec<TurnMetrics>, rounds, tool_calls, truncated}` built in `Harness::run_rounds` (`harness.rs:9398`, returned at :10119 and :10267); the wire's `TurnStarted`/`TurnFinished {usage, timings}` | emit at :138 (door, fill, wake cause) and :163 (summed `TurnMetrics`, outcome from `out`); tap supplies `cost_micros_usd` | worker / child thread: one `try_send` each |
| 2 | each model round → `round` | after the round in `run_rounds`: `metrics.push(ok.metrics)` at `harness.rs:10057`; `ledger_scale`/`wall_anchor` at :10016–10027 (the provider count vs the ledger count — R72); `persist_context` :10029 | emit at :10057 with `TurnMetrics` (`crates/turn/src/metrics.rs:40`: prompt, cached, predicted, `prompt_tokens_server`, `prompt_ms`, `predicted_ms`, `id_slot`, `wall_ms`) and the ledger count | worker / child |
| 2a | time to first byte | **not measured anywhere today.** Local: inside `stream_turn` (`crates/turn/src/engine.rs:1687`) at the first token frame, against `started` (:642). Cloud: the `on_delta` closure in `run_turn_messages_with_tail` (`engine.rs:~1366`) against `started` (:1332); the provider's `send()` returning (`crates/provider/src/openai.rs:~246`) gives time-to-headers. `SilenceWatch` (`harness.rs:12446`) only warns once on metered routes | add `first_byte_ms: Option<u64>` to `TurnMetrics`, set at those two places; carried by #2 | streaming thread, an `Instant` compare |
| 3 | each tool call → `tool.call` | dispatch loop `harness.rs:10133–10153`, `self.runtime.invoke(&turn_id, call, &mut self.tool_sink)` at :10153; `ToolRuntime::run` (`crates/tools/src/runtime.rs:1883`), `Started` at :2031, `finished_event` :2170/:2209 (outcome, inline/full bytes, spill, repairs, edit); early refusals :1924/:1938/:2007/:2026. Memory: `MemoryUse {cap, peak, oom_kills}` (`crates/tools/src/exec/scope.rs:405`, read from cgroup `memory.peak` at :822), reported by bash via `memory_note` (`builtins/bash.rs:140`) | wrap `invoke` at :10153 with an `Instant` (no duration exists today); add `memory: Option<MemoryUse>` to the finished event so the number travels instead of being parsed out of text | worker / child |
| 4 | deliveries (R79) → `delivery` | **arrival**: `JobWatchers::push` (`jobwatch.rs:694`, rings `bell.ring_wake` :719) — `JobCompletion` (:129) gets an `arrived_ms`. **Mid-turn** (children): `take_mid_turn_completions` (:495) drained in `HubSteering::try_next` (`harness.rs:1182`, at :1269). **Between turns**: `Harness::wake` (`harness.rs:7744`), `take_completions()` at :7800, rendered by `completion_notices` (:1150). R79's hold/batch code adds the `held`/`batched`/`starved` sites | emit at each drain with `now - arrived_ms`, the rule, the round; the follow-up flag `next_round_touched` set when the next round's tool calls or text cite the handle (a second, small event `delivery.followup` keyed by handle) | job-watch thread (arrival, a timestamp only), worker / child (drain) |
| 5 | wakes and their cause → `wake`, `turn.start{door=wake}` | `Sessions::wake` (`crates/harnessd/src/sessions.rs:2722`), route `wake_route` (:840); bell coalescing `Bell::ring_wake` (`crates/sessionlog/src/registry.rs:144`, `BellInner.wakes` :73) | emit in `Sessions::wake` with what it gathered (job / child / monitor / due row / queue notice) and whether it ran or found nothing; coalesced count from the bell | worker |
| 6 | worker queue wait (c12d044) → `worker.cmd`, `worker.pass` | enqueue `Hub::submit` (`hub.rs:1259`, `push_back(QueuedCommand…)` :1524) on the head-conn thread — **`QueuedCommand` (`hub.rs:113`) has no time**, only `at_seq`; dequeue `daemon.rs:275` `sessions.dispatch(&session_id, &cmd)`; idle work (review pass `Sessions::serve_reviews` :2197, sweep, `deliver_due_nags`, parking) in the idle arm `daemon.rs:170–232` | add `enqueued_ms: u64` to `QueuedCommand`; emit around `dispatch` at :275 (queued = start − enqueued, dur = end − start, queue depth); time each idle-arm pass and emit `worker.pass` when > 250 ms | worker: two clock reads and one `try_send` per command |
| 7 | compaction and wall → `compaction`, `wall` | `compact_if_at_the_wall` (`harness.rs:8018`), `compact_because_refused` (:8042) → `compact_at_the_wall` (:8063; log :8102, `Scales{before,after,unit}` published :8122–8138, `compacted_line` :8194, failure :8208); `compact_inner` (:8688); manual `Sessions::compact` (`sessions.rs:2593`); wall stop `wall_stops_round` (:11334) at `run_rounds` :9516 (log :9565), provider length refusal :9931–9947; continuation loop in `turn_tail` | emit at :8063 (start) and :8194/:8208 (end) with both units from `ForkReport` (`harness.rs:1683`) / `CompactReport` (:1748); at :9565 for the wall | worker / child |
| 8 | nags → `nag` | idle plan-check `Sessions::deliver_due_nags` (`sessions.rs:2548`, called from `daemon.rs:183`) → `Harness::nag_turn` (`harness.rs:7285`), text `nag_notice` (:7175) / `nag_notice_naming` (:7199), key `nag_said_as` (:7189); child `harness.rs:13276`; in-turn nudges `close_the_turn` (:10696) | emit in `nag_turn` with the named row and `operator_row`; at the next `turn.end`, `next_turn_touched_row` from the todo diff and tool calls | worker / child |
| 9 | context claims (R71) → `context.claim` | `Harness::judge_context_claim` (`harness.rs:10732`, called per round at :10062), fill `context_fill` (:10766); writer `Finding::log` (`crates/harnessd/src/context_claim.rs:491`, file `LOG` :171) | emit beside `Finding::log` (dual-write in phase 1; the file goes in phase 2); carry the REAL window, not the `999999` sentinel the file shows | worker / child |
| 10 | memory samples (R66) → `mem.sample` | `memwatch::watch` (`crates/harnessd/src/memwatch.rs:176`), summary line every 60 s (:186, `summary` :299), `BOUND BROKEN` :195, `THROTTLED` :213, `CENSUS` :227 | emit at :186 every 5th tick and at every census/bound line | memwatch thread |
| 11 | warnings and errors by code → `warning` | `SessionEvent::Warning {code}` through `Hub::publish`; classification `warning::class()` (`crates/sessionlog/src/warning.rs:555`, `TABLE` :141); `HarnessError` (`harness.rs:290`); guard trips `guards::Trip{code}` (`crates/turn/src/guards.rs`) | tap | — |
| 12 | operator actions → `operator.action` | `ClientFrame` arms in `serve_conn` (`server.rs`: Prompt :1524, Interrupt :1614, CompactSession :1645, Mode :1670, Slash :1697, Answer :1710); `CommandIssued` on the hub; approvals/denials answered on the connection thread by `Answers::answer` (`crates/harnessd/src/answers.rs:699`); model switch `Slash::parse` "models" (`harnessd/src/slash.rs:296`) → `Harness::set_provider` (:6203) / `apply_queued_model` (:10488); **there is no `/clear`** — the nearest is the head's `/new` (`crates/tui/src/app/commands.rs:~471`) | tap `CommandIssued` (identity, verb, mid_turn); emit in `Answers::answer` | head-conn |
| 13 | adjudication + oracle → `adjudication` (pointer) | `AdjudicatedGate::record` (`crates/tools/src/adjudicate.rs:2608`) → `Corpus::decided` (`crates/harnessd/src/corpus.rs:90`) → `Store::record_adjudication` (`store.rs:3155`); oracle `HttpOracle::ask` (`crates/harnessd/src/oracle.rs:195`) | emit in `Corpus::decided`: request_id, tier, effect, `oracle_ms` — the row itself stays in the store | worker / child |
| 14 | queue transitions and gate steps → `queue.move`, `gate.step` | `queue_transition` writes in `crates/tokencore/src/store/queues.rs` (`record` :760, public `record_transition` :1130); gate `gate::run_step` (`crates/harnessd/src/mergequeue/gate.rs:304`) producing `MergeGateStep {started_ms, elapsed_ms, outcome}` (`store.rs:1030`), persisted only inside `gate_steps_json` by `Queue::land` (`mergequeue/serve.rs:918`) | tap `QueueMoved`; emit per step in `run_step` | queues thread |
| 15 | subagent lifecycle → `subagent` | `HarnessTaskRunner::start` (`harness.rs:14115`), `start_worktree` (:14242); `SessionEvent::Subagent` via the publish closure (:15339): opening :15369, failed :15385, running :15676, done :15738; park `Sessions::park_idle_children` (`sessions.rs:2477`); kill :14419 | tap `Subagent`; emit at park | — |
| 16 | jobs → `job` | `JobWatchers::push` (`jobwatch.rs:694`) / `JobRecorder::record` (:843) | emit with state, elapsed, produced, memory clause numbers | job-watch thread |
| 17 | operator feedback → `operator.feedback` | new `ClientFrame::Feedback` arm in `serve_conn` (`server.rs:622`), beside Slash (:1697); tui verb in `App::command` (`crates/tui/src/app/commands.rs:416`, local verbs ~:559–782, `SLASH_COMMANDS` table :1215 with its parity test), `Action` (`tui/src/app/action.rs:16`), driver (`driver.rs` beside Slash :804); `PROTOCOL_VERSION` (`protocol.rs:680`) 44 → 45 | emit on the head-conn thread; publish routine `Warning{code:"feedback"}` | head-conn |

---

## 5. Phases

### Phase 1 — the event table, the ten instruments, feedback capture

Scope:

1. `crates/metrics` (new, small): `Event` enum, `emit`, the writer thread, `metrics.db` schema 1,
   the bounded channel with drop counting, the daily prune; `[metrics]` config (`enabled`,
   `keep_days`, `snippets`).
2. The hub tap (one subscriber per hub) covering the tap rows of the ten instruments.
3. The ten instruments (§6).
4. `/fb`, `/fb+`, `/fb-` and the two keys in the tui; `ClientFrame::Feedback` (protocol 45) handled
   in the server thread; the `feedback` routine warning and gutter mark; `letibot fb`.
5. `letibot stats` with `today`, `turns`, `deliveries`, `waits`, `feedback`, `session S`, `sql`.
6. `context-claims.jsonl` and `notes-offers.jsonl` dual-written as events (the files stay until
   phase 2 removes them).

**Done when:**

- A day of normal use on the operator's box produces `metrics.db` with every phase-1 kind
  present; `letibot stats` prints today's turns, p95 worker wait, delivery counts by rule, and
  marks, and the counts agree with an independent read (turn-end log lines; adjudication rows for
  tool calls ±1 %).
- **No SQL runs on the worker thread** (asserted by a test that runs a turn with the writer
  thread blocked and the channel full: the turn completes, `metrics.dropped` > 0, no stall), and
  a `perf` sample over a busy hour attributes < 1 % of the worker's samples to `metrics::`.
- `/fb- #distracted got pulled off by j975` typed mid-turn lands with the head's timestamp, the
  running turn's id and round, within 1 s even while the worker is pinned (test: a worker blocked
  for 30 s, the mark's `ts_ms` is the press time and it is written before the worker frees).
- leticl can send the frame (its side filed as the matching item there) or, until it does, its
  `/fb` reaches the same event through `letibot fb`.
- R79's measurement is answerable: Q1 and Q2 run and return rows for both the old and the new
  delivery code.

### Phase 2 — breadth

Remaining kinds: `round`-level provider-unit vs ledger counts (R72), `compaction` with both
units, `wall`, `gate.step`, `subagent` lifecycle, `job` with delivered-at, `operator.action` for
every command, `error` by kind, `worker.pass` for slow passes, `decision.answer`; `daily`
rollups; `letibot stats` remaining verbs and `export`; the jsonl files retired. **Done when**
Q1–Q14 all return rows from a day's data and the R72 three-numbers discrepancy is visible as one
query.

### Phase 3 — degradation and the page

The §2.6 proxies in `turn.end`; Q9 with marks as ground truth; the live page (§3.3) — export +
artifact first, `stats serve` if the operator wants it live. **Done when** a week of data shows,
per model, each proxy against turns-since-reset with the operator's bad marks beside it, and at
least one proxy is shown to separate marked-bad turns from unmarked ones (or the result is that
none does — also an answer).

### Phase 4 — closing loops

Use the data in the harness itself, each one its own TODO item and each one a decision for the
operator: R71's floor re-tuned from labelled `context.claim` events; R79's starvation cap set
from Q1's held-time distribution; an automatic "clear context" suggestion when Q9's proxies cross
a threshold the marks justify; a `stats` line on the head's status bar (today's turns, p95 wait,
marks).

---

## 6. Phase 1's ten instruments, in priority order

Chosen for the decisions they unblock now (R79, R71, R72, c12d044, R77, R66), and because each is
one emit at a site already found. Numbers refer to the table in §4.

1. **`worker.cmd` + `worker.pass`** (#6) — the c12d044 class of stall becomes a number on every
   command, per build, permanently. Smallest change with the largest blast radius if missed.
2. **`turn.start` / `turn.end`** (#1) — door, model, provider, tokens, rounds, tool calls, wall,
   cost, fill at start and end. The denominator for everything else.
3. **`round`** with TTFB (#2, #2a) — latency, tok/s, cache hit, and the provider-vs-ledger count
   R72 argued over.
4. **`tool.call`** (#3) — duration, outcome, bytes, memory peak vs cap (R65).
5. **`delivery`** (#4) — arrived / delivered / rule / mid-turn / did the model switch. R79's
   before-and-after.
6. **`wake`** (#5) — wake turns by cause; R79's "one wake turn per completion" counted.
7. **`compaction` + `wall`** (#7) — both units, duration, refusals (R72/R73).
8. **`nag`** (#8) — the named row and whether the next turn touched it (R77).
9. **`context.claim`** (#9) — R71's excuse rate by model and fill, with the real window.
10. **`warning` + `operator.action` + `subagent` via the hub tap** (#11, #12, #15) — one tap line in
    `Hub::publish` that records every warning code, every operator command and every subagent
    transition; the cheapest broad coverage in the design.

Plus **`mem.sample`** (#10) if it fits — it is five lines in `memwatch` and R66 is still open —
and **feedback capture** (#17), which is phase 1 by the operator's ask.

---

## 7. Open decisions for the operator

1. **Sibling `metrics.db` vs a table in `sessions.db`.** This design argues sibling (§2.1). The
   cost is a cross-file join; the gain is no store migration and no added lock contention.
2. **Retention**: 90 days raw (~1 GB at today's peak), rollups and marked turns forever. Longer?
3. **Feedback transport**: a new `ClientFrame::Feedback` (protocol 45, off the worker) vs riding
   `Slash` (no bump, but queued behind the worker — the c12d044 problem). Recommended: the frame.
4. **The feedback keys**: `alt-b` / `alt-g`, and `f` on a selected row — or others.
5. **Snippets**: off for everything by default, except `context.claim` while R71's floor is being
   tuned? The excuse sentence is model output.
6. **Metrics on by default**, with `[metrics] enabled = false` to switch off — or off by default.
7. **The page**: export + published artifact (static, refreshed by re-export) or a LAN page served
   by `letibot stats serve` (live).
