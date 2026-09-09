# `letibot-ui` — rendering primitives, and how to wire them into a head

This crate was written by reading two other agent harnesses and taking the parts
that solve problems letibot has. It does not integrate itself: `crates/tui` was
being worked on concurrently and is deliberately untouched. This file is the
handover.

Attribution for everything ported is in the top-level `NOTICE` and, per file, in
each module's `# Provenance` header. Apache-2.0 requires *stating changes*, so
those headers say what was altered and why, not just where it came from.

---

## 1. The seam

**This crate produces lines. A head decides where they go.**

Nothing here opens a file descriptor, reads a clock, reads the environment, or
knows what a session is. Every entry point is a pure function or a small struct
with a `Vec<String>` output. There is no dependency on any other letibot crate,
so a rendering primitive can be tested without the transcript, and a head that
wants to draw something the transcript does not model does not have to fork it.

The mapping from engine types to display types is one `From` impl in the head,
not a dependency edge here. That is the whole reason `card::Outcome` exists
beside `letibot_transcript::ToolOutcome` rather than reusing it.

| module | lines | what it owns |
|---|---:|---|
| `width` | ~600 | columns, grapheme clusters, escape-aware wrap/truncate, byte-range wrap |
| `highlight` | ~600 | streaming-safe syntax colouring for fenced code |
| `diff` | ~700 | Myers line diff, intra-line word diff, unified rendering |
| `card` | ~600 | tool calls and reasoning: fold states, budgets, tense, disclosure |
| `progress` | ~400 | the three-segment prefill bar and the decode line |
| `editor` | ~800 | multi-line input, history, paste, undo, interrupt |
| `style` | ~150 | roles, and the one place a colour is chosen |

74 tests. `cargo clippy -p letibot-ui --all-targets` is clean.

---

## 2. What to integrate, in order of value

Ordered by *how much a person notices*, not by how interesting the code is.

### 2.1 Replace `render::visible_width`, `render::wrap` and `render::trim_to`

**Highest value, smallest change.** `letibot-tui::render` says of its own width
function: *"Counts a char as one column, which is wrong for CJK and emoji and is
the accepted cost of not vendoring a width table."* The cost is not actually
accepted anywhere a person can see it — a status line that is one column too
long wraps, which scrolls the screen, which is one of the things being called
flicker.

```rust
// crates/tui/src/render.rs
pub use letibot_ui::width::{width as visible_width, wrap, truncate as trim_to};
```

Three behavioural changes fall out, all of them fixes:

- CJK, Hangul, fullwidth forms and emoji measure 2.
- A truncation never splits a grapheme cluster or an escape sequence.
- Wrapping breaks between wide clusters (so CJK prose wraps at all — it has no
  spaces, and the current wrapper returns one 400-column line) and hard-breaks a
  run longer than the width (so a 200-character URL no longer overflows).

`wrap` also carries SGR state across a break: each returned line reopens what
the previous one closed. That matters for `term::draw`, which emits `\x1b[K` per
row — erase-to-end-of-line uses the *current* attributes, so a line that leaves
bold open paints the rest of the row bold.

`width::trim_to` keeps the existing `…`-in-the-last-column behaviour.

### 2.2 `progress::prefill_line` in the status line

`app::status_line` currently renders

```
prefill 3921/41233 (38100 cached)
```

Replace with:

```rust
let p = letibot_ui::progress::Prefill {
    total: pp.total, cache: pp.cache, processed: pp.processed, time_ms: pp.time_ms,
};
letibot_ui::progress::prefill_line(&p, w, palette)
```

which gives

```
prefill 95% ▐████████████▓▓▍░▌ 39.9k/41.2k tok · 38.1k cached (92%) · 2.0k tok/s · ~1.6s left
```

and degrades field-by-field as the terminal narrows, never wrapping.

Read the module header before changing anything in it: `processed` **includes**
`cache`, so the fraction is `processed / total` and the work done is
`processed - cache`. Reading it the other way shows a 90 %-cached prompt as 10 %
done and then jumping to 100 %, which is the classic progress-bar lie. The
throughput figure divides by `computed()` for the same reason — dividing the
cache hit by the wall clock produces a number in the hundreds of thousands that
is not a speed.

This display is the one place letibot has strictly more to show than either
surveyed project, because both talk to a metered API and have no prefill number
to show at all.

### 2.3 `card::Card` for tool calls

`app::call_line` produces one line: `● edit(call_7) — ok · 214 B`. Replace with
`Card`, which keeps the disclosure and adds the three things a person is
actually looking for: **what was acted on**, **how long it has been running**,
and **what came out**.

```rust
let card = Card::new(&name, &call_id)
    .target(target)                 // see §4.1 — the engine does not supply this live
    .phase(match state {
        CallState::Proposed => Phase::Proposed,
        CallState::Running  => Phase::Running { elapsed_ms: now - started_ts, note },
        CallState::Finished { outcome, .. } => Phase::Finished {
            outcome: outcome.into(), elapsed_ms: Some(finished_ts - started_ts),
        },
    })
    .body(body_lines);
out.extend(card.render(&CardConfig { width: w, palette, mode, budget, show_id: false }));
```

Notes for the wiring:

- **Elapsed time is derivable today.** `Envelope::ts` is on every event, so
  `ToolStarted.ts` and `ToolFinished.ts` give the duration with no engine change.
  The head has to keep them; `TurnPane` currently does not.
- `ToolProgress { note }` finally has somewhere to go: `Phase::Running { note }`.
  The comment in `call_line` — *"No partial output. There is nowhere to put it,
  by design"* — was true of a one-line renderer and is the reason to stop having
  one.
- `Phase::Replayed` exists for `--replay` and for a late head reading a
  snapshot. It renders `Thought` / `● Read foo.rs` with **no duration**, because
  a replay has no honest elapsed time and `0.0s` is a measurement that was never
  taken rendered as one that was.
- `DisplayMode::next(running)` never expands a running block. A body that grows
  a line at a time pushes the composer down a row per frame, which reads as
  flicker even though nothing is being repainted wrongly.
- Budgets are `Budget::READ` (5/3), `Budget::SHELL` (2/3), `Budget::GENERIC`
  (10/3), from grok-build's shipped calibration. `Budget::for_verb` picks one.

### 2.4 `card::reasoning` for the reasoning pane

Replaces the bare `dim(&cfg, "reasoning")` header. Three independent signals —
the word (`Thinking…` → `Thought for 4.2s`), the `┃` rail, and the dim-italic
attribute — because each one is lost somewhere: colour under a terminal-native
palette, the rail in a copy-paste, the attribute in a `--replay` diff.

**Wrap the body two columns narrower** (`card::REASONING_RAIL_WIDTH`). Getting
this wrong makes the block one row taller than the space reserved for it, which
moves everything below it by a line every frame.

Reasoning duration is derivable from the first and last `Delta { target:
Reasoning }` envelope timestamps.

### 2.5 `highlight::StreamingCode` inside a code block

`render_block`'s `Block::Code` arm paints every line the same green. Replace the
body loop with a `StreamingCode` held per open fence:

```rust
// once, when the fence opens
let mut code = StreamingCode::new(&lang, palette);
// per delta that lands inside the fence
code.push(delta);
// per frame
for l in code.lines() { out.push(format!("│ {l}")); }
```

The important property, and the reason not to just call a one-shot highlighter
per frame: a **complete line is highlighted exactly once, ever**. The lexer
state is `Copy` and is carried across the newline, so only the incomplete tail
is repainted, and that is bounded by the terminal width rather than by the
message. `StreamingCode::bytes_highlighted()` is the instrumentation that says
so — the same shape as `IncrementalMarkdown::bytes_lexed`, and for the same
reason.

**This needs one change in `crates/tui/src/markdown.rs`**, which is why it is
not done here: `Block::Code` accumulates `lines: Vec<String>` by re-lexing the
tail, so a code block's body is currently rebuilt each time the tail is lexed.
Either (a) give `IncrementalMarkdown` a side-channel that hands raw code deltas
to a `StreamingCode`, or (b) keep a `StreamingCode` keyed by block index and
feed it only the *new* lines each frame (`frozen_lines()` says how many it has).
(b) is a smaller change and is enough.

### 2.6 `diff::render` for edit results

Nothing in the head renders a diff today. Once §4.2 below is settled and a
head can see an edit's before/after, this is a drop-in:

```rust
let lines = diff::render(&old_lines, &new_lines, &DiffConfig {
    width: w, palette, context: 3, line_numbers: true,
    intra_line: true, max_rows: 40,
});
card.body(lines)
```

Unified only, deliberately: side-by-side needs 160 columns to show two
72-column files and truncates below that — and the truncated part is where the
change is. `Diff::degraded` is set when Myers hit its `max_d` cap, and `render`
prints a line saying so, because a head is single-threaded over the same loop
that services the socket and an unbounded diff is a dropped frame.

### 2.7 `editor::Editor` for the composer

The largest behavioural change and the one with a real bug behind it.
`term::keys()` reads into a **64-byte buffer**; a paste larger than that arrives
as several reads, and any read that ends mid-UTF-8 fails `from_utf8` and is
**silently dropped**. Pasting a stack trace into the composer loses bytes.

`Editor` is the model half. Wiring it needs three things in `term.rs`, which is
not this crate's to change:

1. **A growable read.** Loop `read` until it would block, rather than one 64-byte
   read, and carry a partial UTF-8 tail between reads instead of discarding it.
2. **Bracketed paste.** Emit `\x1b[?2004h` on enter and `\x1b[?2004l` on
   restore; decode `\x1b[200~ … \x1b[201~` into a single `Key::Paste(String)`.
   This is what lets a paste be recognised *as* a paste rather than as 3,000
   keystrokes — which is what makes the placeholder in §2.7.1 possible and what
   stops a pasted newline from submitting the prompt.
3. **More keys.** `Left`, `Right`, `Home`, `End`, `Delete`, `WordLeft`,
   `WordRight`, `SoftEnter` (Alt+Enter, `\x1b\r`), `KillToEnd` (0x0b),
   `KillToStart` (0x15), `KillWordBack` (0x17), `Yank` (0x19), `Undo` (0x1f or
   0x1a), `Eof` (0x04). `editor::Key` is the target enum.

Then the loop becomes:

```rust
match editor.key(k, now_ms) {
    Reaction::Submit(text) => actions.push(Action::Prompt(text)),
    Reaction::Interrupt    => actions.push(Action::Interrupt("operator".into())),
    Reaction::Quit         => actions.push(Action::Quit),
    Reaction::Changed      => redraw = true,
    Reaction::Idle         => {}
}
```

and the composer is drawn with `editor.render(w, palette)` (which returns the
cursor position) at `editor.height(w, h)` rows.

**2.7.1 The three behaviours worth having, specifically:**

- **A large paste collapses to `[Pasted #1 ~41 lines]`** (≥5 lines or >800
  bytes) and expands on submit. Without it a pasted file fills the composer and
  scrolls the conversation away.
- **Esc twice within 5 s interrupts; Ctrl+C twice within 1 s quits, and only
  from an empty composer.** Ctrl+C on a non-empty composer clears it. Today
  `CtrlC` maps straight to `Action::Interrupt`, so there is no way to abandon a
  half-typed prompt and no protection against a stray Ctrl+C killing the head.
  `editor.hint()` changes text after the first press, which is the entire
  mechanism by which anyone discovers a double-tap exists.
- **History that refuses to move once a recalled entry has been edited**, so
  the next arrow press cannot silently destroy an edit. Persistence is the
  head's: `Editor::with_history(Vec<String>)` in, `Editor::history()` out.

---

## 3. What was deliberately not ported

**grok-build's inline viewport** (`xai-ratatui-inline`, ~1,600 lines, zero
internal dependencies — the most liftable thing in that tree). It runs a TUI
*without* the alternate screen: finished blocks are emitted into the terminal's
own scrollback with `emit_to_scrollback`, so they get native scroll, native
selection and survive the process exiting, while a small pinned viewport below
them stays interactive. Its `split_into_line_segments` is an ANSI-aware VTE pass
that computes how many *physical* rows styled content will occupy without
re-encoding it, and its resize strategy is to refuse to predict terminal reflow
and reprint the entire history instead.

It is not ported for two reasons. It is a fork of ratatui's `Terminal` — letibot
has no ratatui and adding it to get one file would invert the dependency
argument this crate is built on. And it is an **architectural** change to how a
head paints, which belongs to whoever owns `crates/tui`, not to a library
handover. It is recorded here because it is the strongest answer to "our
transcript is trapped inside an alternate screen", and because letibot's blocks
are already frozen-prefix-shaped, which is exactly the precondition
`emit_to_scrollback` needs.

Also not ported, with reasons:

- **DEC 2026 synchronised output** (`\x1b[?2026h` / `\x1b[?2026l` around a
  frame). One line in `term::draw`, and the single most effective anti-flicker
  measure available. Left alone only because `crates/tui` is owned by another
  agent this session and this is a two-line change in their file.
- **grok-build's OSC 8 per-cell hyperlink layer.** Needs a cell buffer; letibot
  paints strings.
- **grok-build's `EditHighlightPhase`** (highlight each hunk in isolation first,
  then upgrade to file-scoped styles from a worker thread). Needs a thread to be
  worth anything. The equivalent here is cheaper: seed a `highlight::State` by
  running the lexer over the lines above the hunk.
- **Side-by-side diff.** See §2.6.
- **Markdown tables.** Neither `letibot_tui::markdown::Block` nor this crate has
  them, and a model emits them constantly. opencode's fix is instructive as a
  *policy* — freeze the column widths so the table does not shimmer as rows
  stream in — and instructive as an *anti-pattern*: their `formatMarkdownTables`
  re-runs over the whole accumulated message on every delta, which is exactly
  the O(n²) §13.3 exists to forbid. Doing it right means a `Block::Table` whose
  column widths are sticky once set. That is a change to `markdown.rs`.
- **opencode's compaction, provider abstraction and permission code**, which
  `docs/` already records as deliberately different designs.

---

## 4. What the UI wants that the engine does not expose

Written down rather than invented, per the brief.

### 4.1 A live tool call has no display target

`ToolCallProposed { turn_id, call_id, name, args_digest }` and
`ToolStarted { turn_id, call_id, name, access }` carry no arguments. The
arguments exist in `TranscriptItem::Assistant { tool_calls: Vec<ToolCall> }`,
where `ToolCall::arguments` is the raw JSON — but that arrives with
`TranscriptAppended`, i.e. *after* the call, which is precisely when the head no
longer needs it. So while a call is running, the head can render `Running bash`
and not `Running cargo test --workspace`.

`args_digest` is the right thing to send to *every* head (§4.5's fan-out
argument), and is useless for display.

**Proposed, smallest form:** add `target: String` to `ToolCallProposed` — a
short, tool-supplied, already-truncated display string (a path, a pattern, a
command line), capped at something like 120 bytes. The tool knows which of its
arguments is the one a person reads; the head cannot guess it from JSON without
a per-tool table, which is a schema the head should not own.

This is one field and it is what `Card::target` is waiting for.

### 4.2 A head cannot render a tool result body live

`ToolFinished` carries `payload_digest`, `inline_bytes`, `full_bytes` and
`spill` — no payload. The payload reaches the head only through
`TranscriptItem::ToolResult { payload }` in a snapshot or a
`TranscriptAppended` reconciliation, and `SnapshotItem::item` is `Option`, so
even then it may be absent.

That is a defensible design (an event fans out to every head; a 480 KB payload
should not), and it means §2.3's `Card::body` and §2.6's diff can only be filled
for a **settled** call whose transcript item has been reconciled. The head
should render a running call's body as empty and fill it when the item lands,
which is what `Phase` already distinguishes.

If a live preview is wanted later, the shape that fits the existing design is a
bounded one: `ToolProgress { note }` already exists and is already
interactive-only (scrubbed for late heads). A `preview: Option<String>` on it,
capped at a few hundred bytes, would cost nothing structurally.

### 4.3 There is no rendered diff anywhere in the pipeline

For §2.6 to draw anything, a head needs the file's before and after. Today the
`edit` tool's result payload is whatever the tool wrote; nothing promises it
contains both sides. Either the tool emits a unified patch in its payload (and
the head parses it), or it emits `{old, new}` and the head diffs. The second is
better for this crate — `diff::render` takes two slices — and worse for the
model, which does not need either.

Recommendation: the tool's payload keeps whatever the model needs, and the
*display* target from §4.1 is extended for edit-shaped tools to carry a patch.
Not decided here; it needs whoever owns `letibot-tools`.

### 4.4 Reasoning has no server-supplied end time

`card::reasoning` takes `elapsed_ms: Option<u64>` and renders `Thought` with no
duration when it is `None`. Today the head can compute it from delta timestamps,
which is right for a live turn and wrong for a replay — hence `Phase::Replayed`.
opencode's version keys on a server-set `time.end` on the reasoning part so the
block finalises independently of the parent message; letibot has no equivalent
and does not obviously need one, since `DeltaTarget` switching from `Reasoning`
to `Text` is the same signal.

---

## 5. Property tests worth keeping when this is integrated

Each of these caught something while it was being written, and each is the kind
of thing that regresses silently:

- `width::wrapping_never_exceeds_the_width_for_any_input` — over mixed CJK,
  emoji, escapes and unbreakable runs, at five widths.
- `width::wrap_ranges_tile_the_input_and_agree_with_wrap` — the byte-range and
  string wrappers share one breakpoint finder, and this asserts it rather than
  trusting the comment.
- `highlight::a_complete_line_is_highlighted_exactly_once` — asserts the *ratio*
  against a hypothetical full repaint, and separately that doubling the block
  does not quadruple the work. §13.3, one layer down.
- `highlight::painting_never_changes_the_visible_text` — strip the escapes and
  you must get the source back. The invariant that makes a highlighter safe to
  put underneath a wrapper.
- `diff::the_edit_script_reconstructs_the_new_file` — and, in the same test,
  that dropping the insertions reconstructs the old one.
- `progress::a_mostly_cached_prompt_reads_as_nearly_done_not_nearly_undone`.
- `card::nothing_a_card_renders_ever_exceeds_the_width`.
- `editor::history_stops_navigating_once_a_recalled_entry_is_edited`.
