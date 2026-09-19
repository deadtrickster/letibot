# Streaming tree-sitter in rano, markdown as the first consumer — plan

Written 2026-09-18. The conversation renderer in the TUI (`letibot_tui::markdown`,
`IncrementalMarkdown`) is a hand-written, block-level-only lexer: `**bold**`, `` `code` ``,
`*italic*` and `[links](url)` render as literal text. This plan replaces its engine with
tree-sitter markdown. The engine is **not markdown-specific**: rano gets a generic
append-only incremental parse facility for any language it already registers, and the
TUI's markdown renderer becomes its first consumer. The markdown-specific projection
(block model, inline styles, stability) stays in letibot, where the `Block` enum already
lives.

## 0. What exists, measured

| thing | where | state |
|---|---|---|
| `IncrementalMarkdown` | `crates/tui/src/markdown.rs` | hand-written; block-level only (`Heading`, `Paragraph`, `Code`, `List`, `Quote`, `Table`, `Rule`); no inline. Incremental by freezing a stable prefix at a safe `\n\n` boundary (4 guards + `relax_at` past 4 KB). Instrumented: `bytes_lexed`, `lex_calls`. Property test: `lex(a)++lex(b) === lex(a++b)`. |
| `BlockCache` / `render_block_with` | `crates/tui/src/render.rs` | consumes `md.stable()` / `md.tail()` / `stable_count()`; code blocks go to `letibot_ui::highlight::StreamingCode` (hand-written line-oriented lexer, already incremental). |
| rano `syntax` | `~/Projects/rano/rano/src/syntax.rs` | `Lang` enum (27 languages incl. `Markdown`), `detect()`, `Highlighter` with `refresh(&Buffer)` (full re-parse, per-char `Style` grid, rano's palette) and `classes(&str, Lang) -> Vec<Vec<Option<String>>>` (engine without palette: per-char capture-name grid, **full re-parse per call**). |
| rano's Markdown | `syntax.rs:73,251-296` | **block grammar only** — `tree_sitter_md::LANGUAGE`, rano's own `MARKDOWN_HIGHLIGHTS_QUERY`. The comment says it plainly: *"its inline grammar is a separate tree meant for injections, which rano's engine does not run. Block structure only; inline text stays plain."* |
| `tree-sitter-md` 0.5.3 | in `Cargo.lock` via rano; rano `Cargo.toml` pins it | the tree-sitter org's CommonMark+GFM grammar (repo `tree-sitter-grammars/tree-sitter-markdown`, MIT). Two grammars: `LANGUAGE` (block) + `INLINE_LANGUAGE` (inline), both `LanguageFn`. GFM on by default: pipe tables, task lists, strikethrough. The crate ships a `MarkdownParser` doing the two passes — the reference implementation for Phase 0. |
| versions | rano `Cargo.toml` | `tree-sitter = "0.27"`, `tree-sitter-md = "0.5.3"` — **identical to letibot's lock** (single `tree-sitter` 0.27.0). No new dependency, no version conflict. |
| rano as a dep | `crates/ui/Cargo.toml:44` | `rano = { path = "/home/dead/Projects/rano/rano" }` — already in the TUI's graph via `letibot-ui`. An operator-local directory, and **a git repo** (`master`, with history) — this table said otherwise until 2026-09-19, which would have left the rano agent thinking it had no undo and no way to satisfy its own "verify by diff" exit criterion. The parent `~/Projects/rano/` is not a repo, which is what the wrong reading came from. |

The two-pass mechanism the grammar documents (README, "Standalone usage"): parse with the
block grammar, then a second parse with the inline grammar using
`ts_parser_set_included_ranges` over the `inline` nodes. rano does not run this today;
nothing in the tree does.

## 1. What rano gets: a generic incremental engine

No markdown module. `rano::syntax` gains one type that knows nothing about markdown —
it works for any `Lang`, the same way `Highlighter` already does:

```rust
// rano/src/syntax.rs (extended)
/// An append-only document, incrementally parsed. The editor's `Highlighter`
/// re-parses whole buffers because editor lines get shorter; this type only
/// ever grows, which is the case tree-sitter's incremental re-parse is safe for.
pub struct Stream {
    /* parser, last tree, source, cached query, instrumentation */
}

impl Stream {
    pub fn new(lang: Lang) -> Self;
    /// Append `delta` to the end and re-parse incrementally: the old tree is
    /// edited for the pure-append range and passed to `parse`, so the unchanged
    /// prefix is reused.
    pub fn push(&mut self, delta: &str);
    pub fn src(&self) -> &str;
    /// The current tree as rano's own type — no `tree_sitter::Node` leaks out,
    /// the same rule as `classes()` returning plain strings.
    pub fn root(&self) -> Option<Node>;
    /// Restrict the next parse to these byte ranges (tree-sitter's
    /// `set_included_ranges`). This is what a second grammar over selected
    /// parts of the document is — the generic form of markdown's inline pass,
    /// and of HTML-embeds-JS, C-embeds-asm, etc.
    pub fn set_included_ranges(&mut self, ranges: &[(usize, usize)]);
    /// Run a query over the current tree: capture name + byte range. The
    /// existing `build_classes` walk, exposed without the grid. (The markdown
    /// consumer walks trees directly and does not need this; it is the
    /// facility future consumers — and a reworked `classes()` — use.)
    pub fn captures(&mut self, query: &str) -> Vec<Capture>;
    // NO `bytes_reparsed`. It was in this sketch as "the bytes_lexed
    // equivalent" and it cannot exist: tree-sitter does not expose how much of
    // the prefix it reused, so any counter here is a proxy — and every available
    // proxy (`src.len()` per call, the edited range) is quadratic or constant by
    // construction, answering a question about the instrument rather than about
    // the engine. The not-quadratic question is answered by TIME; see Phase 0.4.
    pub fn parse_calls(&self) -> u64;
}

pub struct Node {
    pub kind: String,      // "atx_heading", "fenced_code_block", "strong_emphasis", …
    pub start: usize,      // byte offsets into `src`
    pub end: usize,
    pub has_error: bool,
    pub is_missing: bool,
    pub children: Vec<Node>,
}

pub struct Capture { pub name: String, pub start: usize, pub end: usize }
```

Plus one grammar registration, which is a grammar and not markdown logic:
`Lang::MarkdownInline` → `tree_sitter_md::INLINE_LANGUAGE`, next to the existing
`Lang::Markdown` → `tree_sitter_md::LANGUAGE`.

What is deliberately **not** in rano: the block model, the inline style vocabulary, the
stability rule, the "collect the `inline` node ranges" step. All of that is one consumer's
projection of the tree, and it stays in letibot.

## 2. What letibot does with it

`crates/tui/src/markdown.rs` keeps its name, its `Block` enum and its public API
(`push`, `blocks`, `stable_count`, `stable`, `tail`, `raw`, `is_empty`,
`bytes_lexed`, `lex_calls`); the hand-written `lex` / `stable_boundary` / `split_cells` /
`list_item` are deleted and replaced by a walk of two `Stream`s:

- **Primary**: `Stream::new(Lang::Markdown)`. `push(delta)` forwards the delta. The block
  model is built by walking `root()`: `atx_heading`/`setext_heading` → `Heading`,
  `fenced_code_block` → `Code` (language from the `info_string` child, `closed` from
  whether the closing delimiter is present), `list` → `List` (items from `list_item`
  children, written number from the marker), `block_quote` → `Quote`,
  `pipe_table` → `Table` (alignment from the delimiter row), `thematic_break` → `Rule`,
  `paragraph` → `Paragraph`. This is the same mapping the hand-written lexer encodes today,
  read off a tree instead of re-derived from text.
- **Secondary**: `Stream::new(Lang::MarkdownInline)`. Per push: walk the primary tree for
  the `inline` node byte ranges, `set_included_ranges`, forward the same delta, walk the
  result → `Run`s per line. Node kinds map to styles in letibot: `strong_emphasis` →
  Bold, `emphasis` → Italic, both nested → BoldItalic, `inline_code` → Code, `link` →
  Link (url from the `hyperlink` destination child), `strikethrough` → Strikethrough,
  everything else → Plain.
- **`Block` text fields widen** from `String` to `Vec<Run>` (`Code` keeps raw lines — it is
  highlighted separately by `StreamingCode`, not inlined). `title()` takes the first run's
  text.
- **Stability** (replaces the 4 freeze guards + `relax_at`): a block is stable iff it is
  not the last top-level block and has no `ERROR`/missing node — read straight off the
  `Node` flags. A non-last block is complete: a later block started after it, so the next
  append cannot change it, which is exactly the prefix tree-sitter reuses.
- **Inline caching**: frozen blocks' runs are computed once; only the tail's inline ranges
  re-parse per push.
- **Incomplete tail**: an unterminated fence or a half-written `**bold` shows as
  `ERROR`/missing nodes in the last block; the block still renders (as today's
  `Code { closed: false }` does) and is excluded from `stable_count`.

`crates/tui/src/render.rs`: `render_block_with` draws `Run`s with the palette (letibot
owns the mapping, as with `classes()`): Bold → emphasis role, Italic → dim, Code →
inline-code role, Link → underlined property colour (URL not shown inline — the TUI has no
pointer), Strikethrough → crossed dim. `BlockCache` is unchanged in shape: same keys
(`stable_count()`, width, palette, limit), same cached stable lines.

`crates/tui/src/app.rs`: no changes expected — it holds `IncrementalMarkdown` values and
calls `push`/`blocks`; the surface is kept.

## 3. Phases

### Phase 0 — PoC in rano (half a day)

Prove the generic mechanism before any letibot code changes. All in `~/Projects/rano/rano`.
The rano-side work (Phases 0–1) is specified for the rano agent in
`~/Projects/rano/rano/plans/S8-streaming-engine.md` — tracked beside S1–S7, moved there
from the unversioned parent directory on 2026-09-19 — self-contained, with the verified
tree-sitter 0.27 API details (`InputEdit`, `Range`, `set_included_ranges`), the
two-pass reference to read (not link), and the exit criteria. This plan's §1–§2 are the
same design from the letibot side.

1. `syntax::Stream` skeleton: `push` + `root()` for any `Lang`; `set_included_ranges`;
   `Lang::MarkdownInline`.
2. **Not-markdown-specific test**: the streaming==full-parse property (tree equality after
   the same total input, appended in 7 random chunks vs at once) for **rust and markdown**
   — two grammars, one engine.
3. Inline-pass test: primary markdown `Stream` + secondary inline `Stream` over the
   `inline` node ranges, following the crate's own `MarkdownParser` as reference; assert
   `strong_emphasis` / `inline_code` / `link` come back with the right byte ranges.
4. **Measurement** (the number the plan lives or dies on): on a 100 KB doc grown by 1,000
   appends, per-push wall time, for markdown **and** rust. Two separate questions, and
   both are asserted because one can pass while the other fails:
   - **Not quadratic**: `p95(last 100 pushes) < 5 × median(first 100)` — per-push cost
     flat as the document grows. An engine that is uniformly slow passes this.
   - **Inside the budget**: `median < 100 µs`. The TUI pushes per token delta, tens per
     second. An engine that is fast at 1 KB and quadratic fails this only at the tail,
     which is why the first assertion exists too.

   Also measure the secondary stream while its included ranges grow — the question is
   whether range changes keep the incremental reuse honest. If either assertion fails,
   report the number; that is the result.

Exit: `cargo test -p rano` green; the measurement printed.

### Phase 1 — the rano API (1 day)

1. Finalise `Node`/`Capture` (children, error/missing flags, byte ranges); `captures()`
   reusing the existing `build_classes` walk; query compilation cached per
   (language, query) as today.
2. Tests: append-only invariant across languages; included-ranges change mid-stream
   (ranges grow at the tail, old ranges unchanged — the streaming case); error recovery
   (a bad tail produces `ERROR` nodes, the next push repairs them, the prefix is
   untouched); `parse_calls` accounting.
3. Module doc on `Stream`: the append-only contract, and why this does not hit the
   full-re-parse rule documented at `syntax.rs:625-628` (that rule is for buffers whose
   lines get shorter; a pure-append edit never does).

Exit: `cargo test -p rano` green; the type is usable from a foreign crate (letibot is the
test).

### Phase 2 — switch the TUI (1 day)

1. `crates/tui/Cargo.toml`: add `rano = { path = "/home/dead/Projects/rano/rano" }` (same
   deliberate absolute path as `crates/ui/Cargo.toml:44`, with the same comment).
2. `crates/tui/src/markdown.rs`: per §2 — two `Stream`s, tree walk to `Block`, `Run`
   widening, stability from the `Node` flags, inline caching. The hand-written lexer is
   deleted, not kept as a fallback (decision §5).
3. `crates/tui/src/render.rs`: draw `Run`s per §2; `BlockCache` untouched in shape.
4. Tests: the existing tui markdown tests (streaming property, the `MARKDOWN` fixture)
   re-point at the new engine; render tests gain inline cases (a bold word, an inline code
   span, a link) asserting the painted roles.

Exit: `cargo test -p letibot-tui` green (172+), `cargo check --workspace` clean, and the
operator's screen shows `**bold**` as bold.

### Phase 3 — polish and the quadratic check (half a day)

1. Port the §13.3 instrument, asking its question the way the engine can answer it: a tui
   test that streams a long doc (the `MARKDOWN` fixture repeated to ~200 KB) in small
   deltas and asserts per-push wall time stays flat — `p95(last) < 5 × median(first)`, as
   Phase 0.4. `bytes_lexed` answered this for the hand-written lexer because that lexer
   knew what it re-read; tree-sitter does not tell us, so the instrument changes shape
   while the question does not.
2. Edge cases found on screen, fixed where they belong: a wrong node-kind mapping is a
   letibot fix; a grammar misparse is upstream (`tree-sitter-md`) and reported there, not
   papered over in either repo. Expected candidates: tables inside lists, a fence with a
   language `StreamingCode` does not know (falls back to plain, as today), very long
   single-line paragraphs (wrap is unchanged — it happens on the rendered lines).
3. Code blocks stay on `StreamingCode` for this workstream. Real grammar highlighting for
   fenced code via rano is a separate, later decision — and it is now a *generic* one
   (`Stream` + `captures()` for any fence language), not a markdown one.

Exit: release build, operator restarts, a real streaming answer renders with inline
markdown, and the instrument says not-quadratic.

## 4. What does not change

- `letibot_ui::highlight::StreamingCode` — code fences keep their current highlighter.
- `rano::syntax::Highlighter` — the editor's `refresh`/`classes`/`style_at` path is
  untouched; `Lang::Markdown` stays block-only there (the editor's per-char grid is a
  different product). `Stream` is additive.
- The `BlockCache` invalidation logic in `render.rs` — same keys, same cache.
- Everything outside `crates/tui/src/{markdown,render}.rs`, `crates/tui/Cargo.toml` and
  `~/Projects/rano/rano/src/syntax.rs` (+ `lib.rs` if the new types are re-exported).

## 5. Decisions, with the recommendation

1. **The engine is generic and lives in `rano::syntax`.** No `rano::markdown` module:
   rano's contribution is "incremental parse for any `Lang`", which is what a
   tree-sitter hub should have and which the editor itself could later use for large
   files. Markdown is the first consumer, not the shape of the API.
2. **The projection stays in letibot.** Block model, inline styles, stability, range
   collection: one consumer's reading of a tree, next to the `Block` enum that already
   lives in `crates/tui`. rano hands back `Node`s and byte ranges; it does not know what a
   heading is.
3. **No `tree-sitter` types leak into letibot.** `Node`/`Capture` are rano's own structs
   (the same rule as `classes()` returning plain strings), so letibot gains no
   `tree-sitter` dependency and there is no second integration — `crates/ui/Cargo.toml:27-34`
   stays true.
4. **`Lang::MarkdownInline` is a grammar registration**, next to `Lang::Markdown` — the
   inline grammar is a grammar, not markdown logic in the engine.
5. **Delete the hand-written lexer at the switch, no fallback.** The streaming==full-parse
   property test plus the corpus test are the confidence; a dead fallback path is the kind
   of thing that rots and then gets trusted.
6. **rano IS version-controlled, and the brief lives in it.** This decision read "rano is
   not version-controlled … `git init` in `~/Projects/rano` is a one-liner" — wrong, and
   wrong twice over, because §0's table said the same thing and correcting one did not
   find the other. `~/Projects/rano/rano` is a repo on `master` with history; the PARENT
   `~/Projects/rano/` is not, and that is where both readings came from.
   So there is a commit to point at, and `Stream` is a branch like any other work. The
   brief was the one thing outside the repo — every other brief for the crate is in
   `plans/` and tracked — and it is `plans/S8-streaming-engine.md` now. That also makes
   the brief's own exit criterion ("verify by diff — you did not touch it") something an
   agent can actually satisfy.

## 6. Risks

- **tree-sitter-md's own caveat** (README): *"it is not recommended to use this parser where
  correctness is important. The main goal … is to provide syntactical information for syntax
  highlighting."* For a TUI that is the right contract — a misparsed emphasis is a word in
  the wrong colour, the same cost class `StreamingCode` documents for its heuristics. A
  misparsed *block* (a code fence seen as prose) is the worse failure; the error-recovery
  and incomplete-tail tests in Phases 0–1 are aimed at exactly that.
- **Incremental re-parse cost on pathological input.** A 200 KB doc whose last line keeps
  growing could force a large re-parse per push; a secondary stream whose included ranges
  change every push may reuse less than a fixed-range one. Phase 0 step 4 measures both;
  Phase 3 step 1 measures the accumulated case. If either is in the ms range at push
  frequency, the fallback is renderer-side throttling (parse at most every N ms, render the
  stale tail) — no engine change.
- **rano drift.** rano is an editor under active development by the operator; a change to
  its `syntax` module or its tree-sitter pin could break the letibot build silently (path
  dep, no lock entry of its own beyond the workspace lock). Mitigation is the existing one:
  `cargo check --workspace` after any rano edit, and the version pin in rano's `Cargo.toml`
  matching letibot's lock (0.27 / 0.5.3 today).
