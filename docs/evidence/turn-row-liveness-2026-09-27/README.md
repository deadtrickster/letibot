# The turn row that could not say the work was going — 2026-09-27

The operator, of a turn that was working the whole time: **"still thinkking glued to Responding"**.

They were reading this row, on the deepseek (`messages`) backend:

```text
  ⠹ Responding · 2m19s · 145 chars
  ⠼ Responding · 2m34s · 145 chars      header: 15.1s · 2826 out · 188 tok/s
```

Two and a half minutes, one number, unmoved — while the header's own count for a single
fifteen-second window of the same turn was **2826 output tokens**. The head was not stuck. The row
was counting the answer's **prose**, and an agentic round has almost none: it is reasoning and
tool-call markup, and neither was in the number.

## What was measured, and how

Two heads on **one** daemon (`42ce9f1aae08`), same session, same live turn, sampled at the same
instant every five seconds — so the two rows are two renderings of one fact:

| window | binary | commit |
|---|---|---|
| tmux `1:2` | `c91dd476…` | `3378375` — before this fix |
| tmux `1:13` | `ce1fd5ef…` | `2aada71` — after |

The sample is `two-heads-sampled.txt`. **Its timestamps are garbled** (`1H:1M:1S`, `2H:2M:2S`):
the sampler passed `date +%H:%M:%S` through `xargs -I%`, which substituted every `%` in the
command with the input line. The pairing is still one sample per five seconds and the sample
number is the clock; the wall-clock is the one thing in here that is not evidence.

## What it shows

```text
       OLD  (prose only)              NEW  (every channel)
 1     ⠙ 7m41s · 107 chars            ⠙ 7m41s · 6384 chars
 2     ⠼ 7m46s · 130 chars            ⠸ 7m46s · 2638 chars
 3     ⠦ 7m51s  — no count at all     ⠦ 7m51s ·  809 chars
 4     ⠇ 7m56s ·  15 chars            ⠏ 7m56s · 1989 chars
 5     ⠙ 8m01s  — no count at all     ⠙ 8m01s  — none yet
 6     ⠸ 8m06s  — no count at all     ⠼ 8m06s · 2876 chars
 7     ⠦ 8m11s · 217 chars            ⠧ 8m11s · 7119 chars
 8     ⠇ 8m16s  — no count at all     ⠏ 8m16s · 1716 chars
 9     ⠙ 8m21s  — no count at all     ⠹ 8m21s  — none yet
10     ⠴ 8m26s · 160 chars            ⠴ 8m26s · 2129 chars
```

Three things fall out of it.

**The old row was silent more often than not.** Five of the ten samples have no count at all —
samples 3, 5, 6, 8, 9 — because those rounds were writing tool calls, and a tool call is not prose.
A row with nothing on it but a spinning glyph and a climbing clock is exactly what *glued* looks
like, and the operator was reading a correct rendering of an uninformative number.

**Where both spoke, they differ by 20–400×.** 217 against 7119; 15 against 1989. The old number was
not a conservative estimate of the work — it was a different quantity, and the smallest of the
three channels.

**The count *falls*, and that is the engine's shape, not a bug.** `run_turn_steered` runs inside
the daemon's round loop and does `turn_seq += 1`, so `TurnStarted` fires once per **round** while
`began_ms` is stamped with the whole turn's start. A fresh pane is built per round: the counter
zeroes, the duration does not. `6384 → 2638 → 809 → 1989` under a clock that only climbs. The
duration is the turn's and this number is the round's, and the field says so now.

## The fix

`TurnPane::out_chars` → **`arrived_chars`**, incremented by all three channels the log carries —
the answer delta, the reasoning delta and the tool-call delta — instead of the answer's alone.

On the local id-decoding path this is invisible, because `TokensGenerated` fires per frame and the
row shows *tok*; its own docstring says what it is for — *"a head tells a hang from a model that is
still emitting… which is exactly the case a long tool-call write is, where no visible text moves at
all."* But `run_turn_messages` emits `Delta` for text and reasoning and **drops `ToolCall`**, and
sends no token counter at all — so on that backend the fallback is the only number the row has, and
it was the one channel those rounds do not use.

`crates/tui/src/app.rs::a_turn_that_only_thinks_and_writes_calls_still_moves_its_own_row` is this
measurement written as a test: a turn with nothing but reasoning and a tool call must move its own
row.
