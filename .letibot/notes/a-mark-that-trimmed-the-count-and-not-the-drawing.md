<!-- abstract: MEASURED 2026-10-30 (branch agent/landed-thinking-is-drawn-once): the live pane drew a landed thought twice with the fold open because `reasoned_upto` trimmed only the COUNT; the fix makes the pane's buffer *be* the unlanded text (a prefix walk over `appended`, re-seated because a frozen-prefix lexer cannot be sliced), and the count was also wrapping at the wrong width — 4 columns optimistic. -->

# A mark that trimmed the count and not the drawing — and the count's width

MEASURED 2026-10-30, branch `agent/landed-thinking-is-drawn-once` (`crates/tui`), from the
operator's *"yeah when reasoning is open wiered things happen, saw it too"* and the previous
branch's `a-streamed-reply-and-its-row-are-two-buffers-and-the-snapshot-door-never-closed` (the
reply's half; this is the thinking's).

## The defect, as the screen showed it

At `Profile::LOUD` + `thinking=open` (what `ctrl-r` writes) with one thought landed as a row:

```text
  ▾ Thought · 1 line · ctrl-r
  ┃ THINK-ALPHA one short thought

  ▾ Thinking… · 1 line · ctrl-r
  ┃ THINK-ALPHA one short thought
```

Two headers, two bodies, one thought. `TurnPane::reasoned_upto` was a byte offset into the whole
round's reasoning and **only `live_work` (the count) honoured it**; `ui::transcript::live` rendered
the whole buffer. A mark that trims a number and not the thing the number is about is worse than no
mark: the two are guaranteed to disagree, and the disagreement is invisible while the fold is shut
(the pane then draws one line of the same text, a smaller duplicate).

## What replaced it

`TurnPane::reasoning` **is** the thinking no row holds — the same trade the reply already makes one
field up — and both the count and the fold read that one string. The state that replaced the offset
is `reasoned_rows`: an index into `appended`, which is the only thing the handover needs to know.

* **A prefix walk, not an equality** (unlike the prose's `prose_taken_over`). One round can produce
  `[Reasoning, Assistant, Reasoning]` (`turn::items`' `only_the_last_thought_can_be_the_open_one`),
  and the pane accumulates a round's reasoning into one buffer, so the rows hold a *prefix* of it in
  order. The walk stops at the first row that is not a prefix of what is left.
* **The failure mode is a duplicate, never a hole** — that is why the comparison is exact bytes and
  a mismatch stops the walk instead of clearing. The pane's copy has been through
  `without_control_lines` and a row's has not, so a sanitised delta mismatches: the words stay on
  the screen twice rather than vanishing.
* **Both doors call one method** (`hand_over_reasoning`): `App::record_item` (a body landing) and
  `App::load` (a snapshot, where rows arrive holding their bodies and no event will ever say so).

## What the frozen prefix forced

`IncrementalMarkdown` settles blocks and never re-lexes them, so `raw()[n..]` is a **different
document** (a fence opened above the cut, a list numbered from the middle) — "draw the slice after
byte N" would need its own lexer and cache and would still be a second rendering of one text. So
the handover **re-seats the remainder**: `IncrementalMarkdown::new()` plus one `push`, plus a fresh
`BlockCache`, at a row's landing. Cost: one lex of what is left, at most once per reasoning row,
normally zero bytes because a round's thinking lands whole.

## The second disagreement the fold exposed: the width

`reasoning_display_lines` divided by the **frame's** width while the body is drawn
`activity_indent + REASONING_RAIL_WIDTH` narrower — 4 columns at ≥60 — so any thought that wrapped
read one line short of what opening it costs: MEASURED `[2 thinking lines]` over three `┃` rows at
86 columns. `reasoning_body_width` is now one definition used by `reasoning_cfg` (the wrap) and by
`reasoning_display_lines` (the number). The count is **screen** rows, so a thought written as
`para\n\npara` counts its blank separators too — which is right, and is why a test that compares the
number with the `┃` rows must use text with no blank lines in it.

## The rake

**A count and the thing it counts must read the same string, or the arithmetic will be honest and
the screen wrong.** When a `*_upto` mark exists, ask what DRAWING honours it — if the answer is
"none", the mark is a bug with a good excuse, and the fix is to make the buffer hold only the
unlanded part rather than to teach the renderer the offset. The `┃` rail is the cheapest way to
count what a fold actually drew (`.contains('┃')` per frame line), and the header's word tells the
two copies apart without an id: a landed row says `Thought`, the live pane says `Thinking…`.
