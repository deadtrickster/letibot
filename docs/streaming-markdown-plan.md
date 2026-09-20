# Streaming tree-sitter in rano, markdown as the first consumer — plan

Written 2026-09-18, revised 2026-09-20 after rano's engine landed and its measurement
failed (§1). The conversation renderer in the TUI (`letibot_tui::markdown`,
`IncrementalMarkdown`) is a hand-written, block-level-only lexer: `**bold**`, `` `code` ``,
`*italic*` and `[links](url)` render as literal text. This plan replaces its engine with
tree-sitter markdown. The engine is **not markdown-specific**: rano has a generic
append-only incremental parse facility for any language it registers, and the TUI's
markdown renderer is its first consumer. The markdown-specific projection (block model,
inline styles, stability) stays in letibot, where the `Block` enum already lives.

**The one-paragraph version of the revision**: the engine works and is correct, but
tree-sitter's incremental reuse is *not* an asymptotic win for markdown — a push
re-lexes essentially the whole document, so a whole-document `Stream` is O(N²) and the
µs-per-push budget misses by ~100×. The fix is the architecture the hand-written lexer
already has: **never give the parser the settled text**. The window (§2) is what makes
this viable, and it is a letibot-side design, not a rano change.

**Status, 2026-09-20**: rano's half landed (`44a6a4f`) and letibot's half is **built and
green** — `rano::syntax::Stream` feeding a bounded window, the block model and inline styles
projected from the tree, the renderer drawing `Run`s, 209 tests in `letibot-tui` plus a
clean `cargo check --workspace --all-targets` and release build. §2 records what
implementation found that this plan had wrong; §6 records the three window bugs each of
those became a test for. What is left is the operator's screen.

## 0. What exists, measured

| thing | where | state |
|---|---|---|
| `IncrementalMarkdown` | `crates/tui/src/markdown.rs` | hand-written; block-level only (`Heading`, `Paragraph`, `Code`, `List`, `Quote`, `Table`, `Rule`); no inline. Incremental by freezing a stable prefix at a safe `\n\n` boundary (4 guards + `relax_at` past 4 KB). Instrumented: `bytes_lexed`, `lex_calls`. Property test: `lex(a)++lex(b) === lex(a++b)`. **Its freeze/splice structure is the load-bearing part of §2 — the lexer under it is what gets replaced.** |
| `BlockCache` / `render_block_with` | `crates/tui/src/render.rs` | consumes `md.stable()` / `md.tail()` / `stable_count()`; code blocks go to `letibot_ui::highlight::StreamingCode` (hand-written line-oriented lexer, already incremental). |
| `rano::syntax::Stream` | `~/Projects/rano/rano/src/syntax.rs:948-1224`, commit `44a6a4f` | **built.** `new(lang)`, `push(delta)`, `src()`, `root() -> Option<Node>`, `set_included_ranges(&[(usize,usize)])` (sorted+merged internally), `captures(query) -> Vec<Capture>`, `parse_calls()`. `Node { kind, start, end, has_error, is_missing, named, children }`, `Capture { name, start, end }`. Additive: `Highlighter`/`detect`/`classes`/`style_at` untouched. `Lang::MarkdownInline` registered. 50 tests green. |
| the brief | `~/Projects/rano/rano/plans/S8-streaming-engine.md` | tracked, numbered after S1–S7; a byte-identical duplicate still sits at `~/Projects/rano/streaming-engine-brief.md` (untracked — it is the copy that can drift). |
| rano `syntax` (editor path) | `syntax.rs` | `Highlighter` full-reparses per call (the rule at `syntax.rs:625-628`, deliberate for the *editor*, whose lines get shorter). `Lang::Markdown` there is block-only: *"its inline grammar is a separate tree meant for injections, which rano's engine does not run."* |
| versions | rano `Cargo.toml` | `tree-sitter = "0.27"`, `tree-sitter-md = "0.5.3"` — **identical to letibot's lock**. No new dependency. `tree-sitter-md`'s `parser` feature is *not* enabled (it would pull `tree-sitter 0.26`, a second C runtime). |
| rano as a dep | `crates/ui/Cargo.toml:44` | `rano = { path = "/home/dead/Projects/rano/rano" }` — already in the TUI's graph via `letibot-ui`. `~/Projects/rano/rano` **is a git repo** (`master`); the parent `~/Projects/rano/` is not. |

## 1. The measurement, and what it kills

rano's brief had one load-bearing number (§3.5): per-push cost flat as the document
grows. Measured on this machine, release build, 1,000 pushes of ~100 bytes
(`rano/TODO.md` §9, `Stream`'s doc comment):

| grammar | first pushes | last pushes | growth |
|---|---|---|---|
| Markdown block | 829 µs | 9.5 ms | **11×** |
| Rust | 42 µs | 305 µs | 7× |
| two-pass (block + inline) | 5.4 ms | 88.8 ms | **16×** |

The suite's not-quadratic bound is 5×; both real targets fail it. Per-push cost is
**linear in the document** (~106 ns per document byte for markdown, ~3.5 for Rust), so a
stream is O(N²) in total — the exact shape this workstream existed to avoid. At 200 KB
the µs budget misses by ~100×, and the two-pass pipeline at 88.8 ms per push is
unshippable for a head that pushes per token.

Diagnosis, from tree-sitter's own source and not our driver: `Tree::edit` is ~0.3 µs.
The cost is all in `parse`, because `ts_parser__can_reuse_first_leaf` (`parser.c`)
refuses to reuse a token when the current parse state admits *external* tokens
(`external_lex_state == 0`). Markdown's block grammar runs a 48-state external scanner
in nearly every block state, so a markdown push re-lexes essentially the whole document
— about what a full parse costs. Rust's states mostly do not, so it reuses ~97% and a
push costs ~3% of a full parse: better, but still linear.

**Therefore: a whole-document `Stream` is not the design.** Handing the parser a growing
document is the mistake, and it is the mistake the old brief's API shape invited.
Incremental reuse is a constant-factor win at best; the *bound* has to come from not
showing the parser the settled text at all. rano named this in its own TODO: *"parse only
the growing tail via `set_included_ranges` and splice trees, or settle closed blocks out
of the parser's input."* That is §2 — and it is the structure `IncrementalMarkdown`
already has.

Also from the brief as written vs what the grammars actually do (both now pinned by
rano's tests):

- **An unterminated fence does *not* produce an error node** in `tree-sitter-md` —
  CommonMark closes a fence at EOF, so the block is complete and error-free. The brief's
  "stability = no error node" rule therefore does not catch the case it was written for.
  Stability needs a block-boundary rule (§2), not an error rule.
- The inline construct kinds are **`code_span`** and **`inline_link`**, not `inline_code`
  and `link`.

## 2. The corrected design: window, not whole document

The engine stays as rano built it; what changes is how letibot feeds it.

**Invariant: the parser is only ever given the unsettled window.** `window_start` is a
byte offset into `raw`; the window is `raw[window_start..]`; everything before it is
settled and lives in `stable: Vec<Block>` where no parser can re-cost it.

Per push:

1. `stream.push(delta)` — the stream holds `raw[window_start..]`, so this parse is bounded
   by the window, not the document.
2. Read `root()`. Its top-level children are the window's blocks.
3. **Settle**: every top-level block except the last is complete (a later block started
   after it, so the next append cannot change it). Move them into `stable` and set
   `window_start` to the start of the last block. If `window_start` moved, **drop the
   stream and start a fresh one seeded with the new window text** — same for the inline
   stream. A fresh stream starts from one parse of a small window, and the settled text
   leaves the parser's input for good (which is also what keeps the window from being
   re-lexed at 106 ns/byte forever).
4. **Cap**: if the window still exceeds the budget (`DEFAULT_MAX_UNFROZEN`, 4 KB — the
   existing constant, and now it bounds parse cost rather than tail length), settle at the
   last safe `\n\n` boundary inside it. This is the existing `relax_at` escape hatch and
   it is what bounds a single unbroken paragraph, whose "last block" never completes.

So the per-push cost is bounded by the window, and the window is bounded by one trailing
block or 4 KB, whichever is smaller. At markdown's ~106 ns/byte that is ≤ ~425 µs worst
case and far less in the common case, flat as the conversation grows.

The freeze guards — the 4 conditions that make a boundary safe to cut at (balanced
fences, the next character starts content, the list is provably closed) — **stay**, and
implementation found they are load-bearing in a way this plan did not predict. The obvious
tree signal, *"a block settles when a later block starts after it"*, is **not sufficient**,
and the way it fails is worth recording:

```text
1. a⏎⏎2      parses as [list(1. a), paragraph(2)]  →  the list "settled"
1. a⏎⏎2. b    arrives                 →  it is one loose list, and the settled block was wrong
```

Whether a blank line ends a list depends on text that has not been written yet, which no
tree can answer. That is guard 4. So the **text** guards decide *where* to cut and the tree
supplies the block model. Settling also re-parses the prefix on its own
(`parse(win[..cut])`) rather than lifting leading blocks out of the window's tree, which
makes the property hold by construction for a block that straddles the cut; it costs one
extra bounded parse per settle.

### The two passes

- **Primary**: a parse of the window with the block grammar. Block model by walking the
  tree, same mapping as today's lexer read off a tree: `atx_heading`/`setext_heading`
  → `Heading`, `list` → `List` (written number from the marker), `block_quote` → `Quote`,
  `pipe_table` → `Table`, `thematic_break` → `Rule`, `paragraph` → `Paragraph`. The blocks
  are **not** `root`'s direct children: the grammar wraps the document in `document` and in
  a `section` per heading, so the walk descends through those containers.

  **Fenced code blocks are not in that parse at all.** The grammar's closing delimiter is
  not line-anchored — `"abc ```"` closes a block opened with ``` , with the delimiter node
  `" ```"` at bytes 7..11 — and the damage is not local: a block closed early leaves the
  parser in the wrong state, so the blocks after it are wrong too and there is nothing to
  repair one at a time. So `fences_in` finds the fences **in the text**, where CommonMark's
  rule is four lines long (a line of its own, only the same run, at least as long as the
  opening one, container prefix stripped: a fence in a quote is written `"> ```rust"`), and
  before the parse each fence's bytes are blanked to spaces with newlines kept. Length and
  every newline are preserved, so the tree's offsets still address the real text and the
  block structure *around* the fences is unchanged — a fence was a separator, and blank
  lines are the same separator. The fences are then merged back in by offset, and a
  container whose only content was a fence is dropped rather than rendering as an empty
  quote beside the code box that took its content.
- **Secondary**: `Stream::new(Lang::MarkdownInline)`, **one parse per range**, each over
  that range's text alone. A range holding no character that can start a construct
  (`` ` `` `*` `_` `~` `[` `<` `\` `!`) is skipped without parsing, which is most
  paragraphs — and that skip is also what keeps the renderer's cost near the old lexer's.

  Three things about the ranges this plan got wrong. Two were caught by the tests; the
  third was caught by the operator's screen, and it is the worst of the three.

  1. **The ranges are not one document.** This plan said to hand them all to a single parse
     with `set_included_ranges`. That is wrong: tree-sitter **concatenates** included ranges
     in the byte stream, so a delimiter in one range pairs with a delimiter in another. The
     operator's screen (2026-09-20, *"first rust block is perfect, second absolutely not"*)
     showed a ``` in the message's first paragraph opening a code span that closed **3,000
     bytes and ten blocks later** — every block in between rendered as code, with its `**`
     showing. The grammar's own reference (`tree-sitter-md`'s `MarkdownParser`) parses one
     inline node at a time for exactly this reason. What made it hard to see: the *streamed*
     path was correct throughout, because a settled prefix is parsed on its own, so only the
     one-shot path — a transcript replay after a restart — showed it.
  2. The ranges are cut around **`block_continuation`** children: a quote's second `> ` is
     block structure the block grammar leaves *inside* the paragraph's inline node, and the
     inline grammar has never heard of it, so it renders as a literal `> three`.
  3. The span walk must be **exhaustive, with an explicit "not text" marker**, rather than
     "text is whatever no span covers": the markers are named nodes, so leaving them
     uncovered hands `**` back to the renderer as literal text — the exact bug this
     workstream exists to remove. (The named-children split in the earlier draft was wrong
     in the other direction too: this grammar puts the content of `**bold**` in no node at
     all. Its children are four `emphasis_delimiter`s, and the word is the gap between the
     second and the third.)
- **`Block` text fields widen** `String` → `Vec<Run>` (`Code` keeps raw lines — it is
  highlighted separately by `StreamingCode`). `title()` takes the first run's text.
- **Inline caching**: settled blocks' runs are computed once, when they settle. Only the
  window's inline ranges are re-parsed.

`crates/tui/src/render.rs`: `render_block_with` draws `Run`s with the palette (letibot owns
the mapping, as with `classes()`): Bold → `sgr::BOLD`, BoldItalic → `sgr::BOLD_ITALIC`,
Italic → `sgr::ITALIC`, Code → `sgr::CYAN`, Strikethrough → `sgr::DIM` (no `9m` — an
attribute half the terminals in use do not carry). A link shows its text and not its
destination: the TUI has no pointer. `BlockCache` unchanged in shape: same keys
(`stable_count()`, width, palette, limit), same cached stable lines.

`crates/tui/src/app.rs`: **no changes**, as predicted — it holds `IncrementalMarkdown`
values and calls `push`/`blocks`.

The hand-written inline scanner `render::inline` — the `**bold**`/`` `code` `` text scan —
is **deleted**. There is nothing left for it to scan: the model arrives styled.

## 3. Phases

### Phase 0 — rano's engine — **DONE** (`44a6a4f`)

`rano::syntax::Stream`, `Node`, `Capture`, `Lang::MarkdownInline`; 12 new tests; 50 green.
The measurement was taken and it **failed the flat-cost criterion**, which is the finding
that reshaped §2. rano encodes the criterion as an `#[ignore]`d test that fails, an
always-on test that prints both languages' numbers and asserts the weaker true property
(an append is never worse than a full re-parse), and a test that pins the current linear
behaviour so a future tree-sitter or grammar change that fixes it turns red. That is the
right treatment — the number is recorded where it can be re-measured, not argued away.

**No further rano work is needed for §2**: the window is consumer-side (a fresh `Stream`
per settle + `set_included_ranges`), which the existing API already supports. If it turns
out a `truncate`/`reset` on `Stream` would save the one extra parse per settle, that is a
follow-up, argued with `per_push_cost_stays_flat` re-run — not a precondition.

### Phase 1 — window + projection in letibot — **DONE**

Built as below, with three things the plan had wrong (all recorded in §2 and §6):

1. `crates/tui/Cargo.toml`: `rano = { path = "/home/dead/Projects/rano/rano" }`, same
   deliberate absolute path as `crates/ui/Cargo.toml`, with the same reasoning. `Cargo.lock`
   gains one line — the dependency edge, no new crate.
2. `crates/tui/src/markdown.rs`: same name, same `Block` enum shape, same public API
   (`push`, `blocks`, `stable_count`, `stable`, `tail`, `raw`, `is_empty`, `bytes_lexed`,
   `lex_calls`), plus `window_len()` for the test. `lex()` is now the one-shot form of the
   same engine rather than a separate implementation, so the streamed and the whole-parse
   paths cannot drift. `split_cells`/`delimiter_row`/`is_rule` and the hand-written
   emphasis scan are gone; `list_item` survives only for guard 4.
3. **The test that matters most, and it is not a unit test**:
   `the_window_stays_bounded_as_the_document_grows` — 400 paragraphs streamed eight bytes
   at a time, asserting the window stays under `DEFAULT_MAX_UNFROZEN`, that most of the
   document settled, and that `bytes_lexed` is an order of magnitude under the
   full-re-parse-per-delta figure. Asserts on **size**, never on wall time: that is the
   flake rano's ignored test has to live with, and this one must not.
4. Tests: 22 in `markdown.rs` (streaming == one parse over four block shapes and six chunk
   sizes, the window and cap bounds, headings, emphasis and nesting, literal asterisks,
   links and autolinks, tables and alignment, loose lists, nested lists, quotes, fences,
   the guard unit tests) and 3 end-to-end in `render.rs` that render a whole answer and
   check every word survives.

### Phase 2 — switch the renderer — **DONE**

1. `render.rs` draws `Run`s per §2; `BlockCache` untouched in shape.
2. `render::inline` — the hand-written `**bold**`/`` `code` `` scanner — and its
   `utf8_len` helper are deleted, replaced by `paint_runs` (style → escape) and
   `joined_runs` (a block's lines as one run list).
3. `sgr::BOLD_ITALIC` added for `***both***`.

Exit: `cargo test -p letibot-tui` green at 209, `cargo check --workspace --all-targets`
with no new warnings, `cargo build --release` clean. The operator's screen showing
`**bold**` as bold on a live answer is the one thing a test cannot assert.

### Phase 3 — polish — **DONE, and it found a content-loss bug**

Tested by probing the shapes the plan listed: a fence in an unknown language (fine — the
renderer falls back to plain, as before), a table after a list (fine), an indented code
block (fine), and then two that were not.

**A block inside a flat container was being dropped or mangled.** `Block::Quote` holds run
lines and `List::items` holds run lists — the renderer has one indent level, so nesting is
flattened by design — but the projection filled them by asking for *inline* content, and a
`block_quote` or `list_item` can hold a `fenced_code_block`, a `pipe_table`, a nested list.
Neither of those has an `inline` node, so:

```text
> ```rust⏎> let a = 1;⏎> ```     →  Quote { lines: [[]] }        the code was gone entirely
- item⏎⏎  ```rust⏎…              →  item ```rust let a = 1; ```   the markers were item text
```

The first is the worse bug of the two and the harder to notice: an empty quote renders as
an empty quote, which looks like a model that said nothing. It is the same class as a card
naming the wrong file — the head misquoting the model.

Fixed by `subtree_lines`: where the model is flat, walk the subtree for everything a reader
would see (inline text with its styles, a fence's content with its continuation markers
cut, a nested table's rows as joined cells) instead of asking for inline content. Also
`code_fence_content` carries `block_continuation` children — the `> ` that opens a quoted
fence's closing line is a child of the *content*, not of the quote — so a fence's lines are
extracted with those cut out too.

The cost of flattening is that a quoted fence is quote prose rather than a coloured code
box. That is the model's limit, not this fix's: `Block::Quote` cannot say "this line is
code", and showing the ``` markers instead would be worse. Six tests in `markdown.rs`'s
`nesting` module assert the *words survive* rather than the shape, because the failure mode
is silent.

**Then the operator's next message was "code blocks still broken", and it was two more
bugs — both in the guards, both found by adding a fence to the streaming corpus.**

1. **A fence whose content contains a fence.** The guards count fences by lines that
   *start* with ```, and a quoted line *is* such a line, so the count inverted from there
   on. The `\n\n` after the (really open) fence was taken for a boundary, the prefix
   settled, and a prefix that ends really does end at an end of input — so the open fence
   became a *closed* one, empty, with everything after the cut re-parsed as prose. On the
   operator's screen that was an empty code box and my own text spilled out below it.
   Fixed by `fence_spans`: the tree says where every fence begins and ends, and a cut
   inside one is refused. An unclosed fence's range reaches the end of the window, because
   that is what an unclosed fence covers.
2. **The relax fallback cut inside a fence body.** Added for the endless-paragraph case,
   and it turned the half of a long code block without delimiters into a paragraph — the
   model's code shown as prose. Also fixed by `fence_spans`: a window that is one code
   block has no legal cut at all, so it stays whole. That is the *old* lexer's behaviour
   too (its fence guard was never relaxed), and it is the honest bound: the window is
   capped at 4 KB for prose and equals the largest code block otherwise, at markdown's
   ~106 ns/byte per push — about a millisecond for a 10 KB block, only while it streams.
3. **Guard 4 compared the written numbers, not the list kind.** `1. first\n\n2. second` is
   one loose list; comparing the numbers said `1.` and `2.` were different items of
   different lists and cut between them. `MARKDOWN`'s list is tight (no blank lines), which
   is why the fixture never caught it. Fixed by `list_kind`.

All three are pinned in a new `streaming_matches_one_parse` module: a corpus of nine
documents at six chunk sizes, plus the byte-at-a-time case. The property is the whole
safety argument for the window, and the corpus has to keep the shapes that break it —
fences in fences, loose lists, quotes holding fences, tables, rules.

**Then "code blocks still broken" turned out to be the inline pass, not the guards.** That
report (2026-09-20, *"first rust block is perfect, second absolutely not"*) was a code span
spanning ten blocks — item 1 of the secondary pass above. What it says about the method is
worth keeping:

- **The bug was in the path the operator was looking at and not the one I was testing.**
  Every test asserted `stream(...) == lex(...)` — but the *stream* was right and the
  *one-shot* was wrong, so only a document long enough to hold two delimiters in one parse
  could show it, and every fixture was a few lines.
- **The fix made the renderer cheaper.** One parse per range, with ranges holding no syntax
  characters skipped, replaced one parse of the whole window: `letibot-tui`'s suite went
  from 26 s to 5.6 s on the same tests.
- The message itself became the first fixture (`streamed-message.md`), because the shape
  that broke it is a shape and its length was part of it.

**Then "awful" was the fences, and the grammar is where it went wrong.** The last report
(2026-09-20, *"that first rust block was perfect but second was a code block with ```rust
<code> ``` inside"*, then *"awful"*) was not about the renderer at all:

- **`tree-sitter-md`'s closing fence is not line-anchored.** For `"abc ```"` the delimiter
  node is `" ```"` at bytes 7..11, so a fence whose content ends a line in backticks closes
  there. The damage is not local — the parser's *state* is wrong from that point — so a
  message that drew box art containing ``` lost two art lines to a paragraph and had its
  whole tail rendered inside one unclosed code box. That is exactly what the operator's
  screen showed, and it is the grammar's README taken literally: *"not recommended to use
  this parser where correctness is important"*.
- The fix is `fences_in` + `mask` (see the primary pass above): fences are found in the
  text and blanked out before the parse, with length and newlines preserved so every offset
  still addresses the real text. It costs one thing — a fence inside a quote or a list item
  is now a **code box of its own** rather than quote prose — which is better than what it
  replaced, and it is what the tests assert.
- The message became the second fixture, `box-art-message.md`.

Two things the whole sequence says about the method, both worth keeping:

- **Every one of these was invisible at the size I tested and obvious at the size the
  operator was using.** Six bugs across four rounds, and the fixtures that catch them are
  real messages from the store rather than documents written to be small.
- **`fence_spans` was the wrong idea, and it took two rounds to see it.** Reading fences off
  the tree looks right until a test asks whether the tree's idea of a fence is the text's.
  It is not, for the closing delimiter — and every other reader of the tree inherited that.
  The rule the code now follows is the text's, with the tree doing only what it is good at.

**Then the operator asked whether every bug had a test, and the answer was no.** Checked by
mutation — put each bug back and see whether anything goes red — and two mutations changed
no result. Chasing why found two more bugs and two pieces of dead code:

- **A fence on a list marker's line was never found.** `- ```rust` rendered as item text
  holding the markers plus a spurious empty code box, because the container prefix was read
  as whitespace and `>` only. A list marker is now part of that prefix, as **blanks rather
  than as itself** — a marker does not repeat on a continuation line, so `"- ```rust"` is
  continued by `"  let a = 1;"` and the only prefix they share is the content column.
  `closing_fence` strips each candidate line on its own for the same reason.
- **Three pieces of code were unreachable and had never run.** After `mask` the tree holds
  no `fenced_code_block` node at all, so the arms written to read one out of a container —
  and `only_holds_a_fence` with the drop it guarded — could not fire. Measured both ways:
  deleting them changed nothing, and `false` in place of the drop's condition changed
  nothing. Removed, with `masking_leaves_no_fence_in_the_tree` asserting the invariant so
  they cannot come back. The empty container they were written to drop renders as zero lines
  anyway, which is why nobody noticed.

The lesson generalises past this file: **a test that cannot fail is not evidence**, and the
only cheap way to find one is to break the code it is supposed to guard. Two of the six
regression tests in this file were like that until the mutation run.

Code blocks stay on `StreamingCode` for this workstream. Real grammar highlighting for
fenced code via rano is a separate, later decision — now a *generic* one (`Stream` +
`captures()` for any fence language), and it would want the same window discipline. It is
also what would let a quoted fence render as code.

## 4. What does not change

- `letibot_ui::highlight::StreamingCode` — code fences keep their current highlighter.
- `rano::syntax::Highlighter` — the editor's `refresh`/`classes`/`style_at` path is
  untouched; `Lang::Markdown` stays block-only there.
- The `BlockCache` invalidation logic in `render.rs` — same keys, same cache.
- The freeze-point guards in `markdown.rs` — kept, and now load-bearing for correctness,
  not only for speed.
- Everything outside `crates/tui/src/{markdown,render}.rs` and `crates/tui/Cargo.toml`.

## 5. Decisions, with the recommendation

1. **Window, not whole document.** §1. The settled prefix never reaches a parser. This is
   the design; everything else is detail.
2. **The engine is generic and lives in `rano::syntax`.** Markdown is the first consumer,
   not the shape of the API.
3. **The projection stays in letibot.** Block model, inline styles, the settle rule, range
   collection: one consumer's reading of a tree, next to the `Block` enum that already
   lives there. rano hands back `Node`s and byte ranges; it does not know what a heading
   is.
4. **No `tree-sitter` types leak into letibot.** `Node`/`Capture` are rano's own structs,
   so letibot gains no `tree-sitter` dependency and there is no second integration —
   `crates/ui/Cargo.toml:27-34` stays true.
5. **Delete the hand-written lexer at the switch, no fallback.** The streaming==full-parse
   property test plus the corpus test are the confidence; a dead fallback path is the kind
   of thing that rots and then gets trusted. Note this deletes the *lexer*, not the
   freeze/splice scaffolding, which §2 keeps.
6. **rano IS version-controlled, and the brief lives in it.** `~/Projects/rano/rano` is a
   repo on `master`; the parent `~/Projects/rano/` is not, and an earlier version of this
   plan (and an offer to `git init`) came from reading the parent. The brief is
   `plans/S8-streaming-engine.md`, tracked.

## 6. Risks

- **The measurement is the risk, and it already fired.** §1. §2 is the answer to it, and
  the risk is now that §2's window is wrong somewhere — a missed settle, or a guard that
  cuts where the tail does not parse standalone. Phase 1 step 3 is the test aimed at it,
  and it asserts on the window's size rather than on time so it cannot flake.
- **tree-sitter-md's own caveat** (README): *"it is not recommended to use this parser
  where correctness is important. The main goal … is to provide syntactical information for
  syntax highlighting."* For a TUI that is the right contract — a misparsed emphasis is a
  word in the wrong colour, the same cost class `StreamingCode` documents for its
  heuristics. A misparsed *block* (a fence seen as prose) is the worse failure; the corpus
  and guard tests are aimed at that.
- **The window adds a failure mode the old lexer did not have.** Cutting at byte offset F
  and parsing `raw[F..]` standalone is correct only under the guards; if one is wrong, the
  tail renders as structure the document does not have, and the settled prefix hides it.
  This fired during implementation, three times, and each one is now a test:
  - the tree's own "a later block started" rule is not a sufficient cut condition (§2):
    a paragraph becomes the next list item. Guard 4 was kept because of it.
  - the inline tree walk that was not exhaustive let `**` back onto the screen as literal
    text. "Text is whatever no span covers" is the wrong rule.
  - a block quote's `>` continuation lives inside the paragraph's inline node, so the
    inline grammar rendered it as a literal character.
  What catches the next one is the test that compares the **streamed** model against a
  **single parse** of the same document, over four documents and six chunk sizes. It is
  the only assertion that can fail when a cut is unsafe, and it is why §1's "error node"
  rule was replaced rather than patched.
- **Two blocks shape the cost, one of them improved.** A long *loose* list is one block
  however many items it has, so the cap has to cut it and `stable_count()` says nothing
  about how much settled (the test counts items). In exchange the tree is better than the
  lexer it replaced on exactly the case that motivated this work: a loose list used to
  arrive as one block per item, which is what made every point of a six-point answer
  render `1.`. It is now one list, numbered from `start + i`.
- **Numbering, and what the tree gives that text did not.** A `*` in `2 * 3` is no longer
  italicised (the grammar says it is not emphasis), an ordered list's start comes from its
  own marker node, and an escape (`\|` in a table cell) is undone where the grammar
  recognised it. Each is a test, and each is a case the text scan got wrong.
- **rano drift.** rano is an editor under active development; a change to `syntax` or its
  tree-sitter pin could break the letibot build silently (path dep). Mitigation is the
  existing one: `cargo check --workspace` after any rano edit, and rano's pins matching
  letibot's lock (0.27 / 0.5.3 today).
