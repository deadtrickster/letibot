# Streaming tree-sitter in rano, markdown as the first consumer

The TUI's markdown renderer is tree-sitter, not a hand-written lexer, and this document is why
the engine is fed a window rather than the whole document. The engine is **not
markdown-specific**: rano has a generic append-only incremental parse facility for any language
it registers, and the TUI's markdown is its first consumer.

The short version: the engine works, but tree-sitter's incremental reuse is **not** an asymptotic
win for markdown. A push re-lexes essentially the whole document, so a whole-document stream is
O(N²) and the µs-per-push budget misses by ~100×. The fix is the architecture the lexer already
had: **never give the parser the settled text**.

## 1. The measurement that decided the design

rano's brief had one load-bearing number: per-push cost flat as the document grows. Measured in
release, 1,000 pushes of ~100 bytes:

| grammar | first pushes | last pushes | growth |
|---|---|---|---|
| Markdown block | 829 µs | 9.5 ms | **11×** |
| Rust | 42 µs | 305 µs | 7× |
| two-pass (block + inline) | 5.4 ms | 88.8 ms | **16×** |

The suite's not-quadratic bound is 5×; both real targets fail it. Per-push cost is **linear in the
document** (~106 ns per document byte for markdown), so a stream is O(N²) in total - the exact
shape this workstream existed to avoid. At 200 KB the budget misses by ~100×, and the two-pass
pipeline at 88.8 ms per push is unshippable for a head that pushes per token.

The diagnosis is tree-sitter's own: `ts_parser__can_reuse_first_leaf` refuses to reuse a token
when the current parse state admits *external* tokens. Markdown's block grammar runs a 48-state
external scanner in nearly every block state, so a markdown push re-lexes essentially the whole
document. Rust's states mostly do not, so it reuses ~97% and a push costs ~3% of a full parse -
better, but still linear.

**Therefore a whole-document stream is not the design.** Incremental reuse is a constant-factor
win at best; the *bound* has to come from not showing the parser the settled text at all.

Two grammar facts, both now pinned by tests:

- **An unterminated fence does not produce an error node** in `tree-sitter-md` - CommonMark
  closes a fence at EOF, so the block is complete and error-free. "Stability = no error node"
  therefore does not catch the case it was written for; stability needs a block-boundary rule.
- The inline construct kinds are **`code_span`** and **`inline_link`**, not `inline_code` and
  `link`.

## 2. The design: window, not whole document

**Invariant: the parser is only ever given the unsettled window.** `window_start` is a byte offset
into `raw`; the window is `raw[window_start..]`; everything before it is settled and lives in
`stable: Vec<Block>`, where no parser can re-cost it.

Per push:

1. `stream.push(delta)` - the stream holds `raw[window_start..]`, so this parse is bounded by the
   window, not the document.
2. Read `root()`. Its top-level children are the window's blocks.
3. **Settle**: every top-level block except the last is complete (a later block started after it,
   so the next append cannot change it). Move them into `stable` and set `window_start` to the
   start of the last block. If `window_start` moved, **drop the stream and start a fresh one
   seeded with the new window text** - same for the inline stream. The settled text leaves the
   parser's input for good.
4. **Cap**: if the window still exceeds the budget (4 KB), settle at the last safe `\n\n`
   boundary inside it. This is the escape hatch that bounds a single unbroken paragraph, whose
   "last block" never completes.

So the per-push cost is bounded by the window, and the window by one trailing block or 4 KB,
whichever is smaller: at ~106 ns/byte, under half a millisecond worst case, flat as the
conversation grows.

**The text guards stay, and they are load-bearing.** The obvious tree signal, *"a block settles
when a later block starts after it"*, is not sufficient:

```text
1. a⏎⏎2      parses as [list(1. a), paragraph(2)]  →  the list "settled"
1. a⏎⏎2. b    arrives                 →  it is one loose list, and the settled block was wrong
```

Whether a blank line ends a list depends on text that has not been written yet, which no tree can
answer. So the **text** guards decide *where* to cut and the tree supplies the block model.
Settling also re-parses the prefix on its own rather than lifting leading blocks out of the
window's tree, which makes the property hold by construction for a block that straddles the cut.

### The two passes

**Primary**: a parse of the window with the block grammar. The block model comes from walking the
tree: `atx_heading`/`setext_heading` → `Heading`, `list` → `List` (with the written number from
the marker), `block_quote` → `Quote`, `pipe_table` → `Table`, `thematic_break` → `Rule`,
`paragraph` → `Paragraph`. The blocks are not `root`'s direct children - the grammar wraps the
document in `document` and in a `section` per heading, so the walk descends through those
containers.

**Fenced code blocks are not in that parse at all.** The grammar's closing delimiter is not
line-anchored - `"abc ```"` closes a block opened with ``` , with the delimiter node `" ```"` at
bytes 7..11 - and the damage is not local: a block closed early leaves the parser in the wrong
state, so the blocks after it are wrong too and there is nothing to repair one at a time. So
fences are found **in the text**, where CommonMark's rule is four lines long (a line of its own,
only the same run, at least as long as the opening one, container prefix stripped: a fence in a
quote is written `"> ```rust"`), and before the parse each fence's bytes are blanked to spaces
with newlines kept. Length and every newline are preserved, so the tree's offsets still address
the real text and the block structure *around* the fences is unchanged - a fence was a separator,
and blank lines are the same separator. Fences are merged back in by offset, and a container
whose only content was a fence is dropped rather than rendering as an empty quote beside the code
box that took its content.

**Secondary**: one inline parse **per range**, each over that range's text alone. A range holding
no character that can start a construct (`` ` `` `*` `_` `~` `[` `<` `\` `!`) is skipped without
parsing, which is most paragraphs - and that skip is what keeps the cost near the old lexer's.

Three things about the ranges, each found by a real message:

- **The ranges are not one document.** Handing them all to a single parse with
  `set_included_ranges` is wrong: tree-sitter **concatenates** included ranges, so a delimiter in
  one range pairs with a delimiter in another. A ``` in a message's first paragraph opened a code
  span that closed 3,000 bytes and ten blocks later, rendering every block in between as code.
  The grammar's own reference parser parses one inline node at a time for exactly this reason.
- The ranges are cut around **`block_continuation`** children: a quote's second `> ` is block
  structure the block grammar leaves *inside* the paragraph's inline node, and the inline grammar
  has never heard of it, so it renders as a literal `> three`.
- The span walk must be **exhaustive, with an explicit "not text" marker**, rather than "text is
  whatever no span covers": the markers are named nodes, so leaving them uncovered hands `**`
  back to the renderer as literal text - the exact bug this workstream exists to remove. This
  grammar also puts the content of `**bold**` in no node at all: its children are four
  `emphasis_delimiter`s, and the word is the gap between the second and the third.

**`Block` text fields widened** `String` → `Vec<Run>` (`Code` keeps raw lines). Inline runs are
computed once, when a block settles; only the window's ranges are re-parsed.

## 3. Where the code is

The engine and the projection live in **`rano::markdown`** (`Block`, `IncrementalMarkdown`,
`MarkdownView`, `lex`, `render_block`), pinned by rev in `crates/tui/Cargo.toml` and
`crates/ui/Cargo.toml`. letibot keeps the glue the head owns in `crates/tui/src/ui/render.rs`:
`RenderConfig` and its palette, the reasoning pane's `Decor`, the picture placement, and
`BlockCache`, which is rano's `MarkdownView` with its settled lines turned into row strings once,
as they settle, so a frame costs the tail and the window and not the document. `App` holds
`IncrementalMarkdown` values and calls `push`/`lines`; the hand-written lexer and its inline
scanner are gone.

Fenced code keeps a line-oriented incremental highlighter, which is now rano's, driven by
letibot's palette; `BlockCache::parses()` is the instrument that says a frame drawing a settled
fence added no parse.

The two messages that broke the first cut are kept as documents in the tree, because the shapes
that break a parser are shapes and their length was part of it:
`crates/tui/tests/fixtures/streamed-message.md` and `box-art-message.md`.

## 4. What does not change

- The `BlockCache` contract in `render.rs`: same keys (width, palette, bound), same cached
  settled rows.
- The palette mapping is letibot's: Bold → `sgr::BOLD`, BoldItalic → `sgr::BOLD_ITALIC`, Italic →
  `sgr::ITALIC`, Code → `sgr::CYAN`, Strikethrough → `sgr::DIM` (no `9m` - an attribute half the
  terminals in use do not carry). A link shows its text, not its destination.
- Everything outside `crates/tui`'s rendering path and the rano pin.

## 5. Decisions, with the reason

1. **Window, not whole document.** §1. The settled prefix never reaches a parser. This is the
   design; everything else is detail.
2. **The engine is generic and lives in rano.** Markdown is the first consumer, not the shape of
   the API.
3. **No `tree-sitter` types leak into letibot.** rano hands back its own `Node`s and byte ranges,
   so letibot gains no second tree-sitter integration.
4. **Delete the hand-written lexer, no fallback.** The streaming==full-parse property test plus
   the corpus are the confidence; a dead fallback path rots and then gets trusted. The *lexer* is
   what goes - the freeze/splice scaffolding is the design.
5. **The text decides where to cut; the tree supplies the block model.** §2's guards, and the
   grammar's non-line-anchored closing fence, both say the same thing: a tree is good at
   structure and wrong about text.
6. **A test that cannot fail is not evidence.** Two of the six regression tests here were like
   that until each bug was put back to see whether anything went red.

## 6. Risks

- **`tree-sitter-md`'s own caveat** (its README): *"not recommended to use this parser where
  correctness is important."* For a TUI that is the right contract - a misparsed emphasis is a
  word in the wrong colour. A misparsed *block* (a fence seen as prose) is the worse failure, and
  the guards and the corpus are aimed at that.
- **The window adds a failure mode the old lexer did not have.** Cutting at byte offset F and
  parsing `raw[F..]` standalone is correct only under the guards; if one is wrong, the tail
  renders as structure the document does not have and the settled prefix hides it. What catches
  the next one is the test that compares the **streamed** model against a **single parse** of the
  same document, over a corpus of documents at six chunk sizes plus byte-at-a-time. It is the
  only assertion that can fail when a cut is unsafe.
- **A long loose list is one block**, so the cap has to cut it and a block count says nothing
  about how much settled. In exchange the tree is better than the lexer it replaced on the case
  that motivated this work: a loose list used to arrive as one block per item, which made every
  point of a six-point answer render `1.`.
- **rano drift.** A change to `markdown` or its tree-sitter pin could break the letibot build
  silently through the git dependency. `cargo check --workspace` after any rano edit, and rano's
  pins matching letibot's lock.
