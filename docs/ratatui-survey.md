# Ratatui survey — what this head would get, and what it would cost

Written 2026-10-06, against **`letibot` at `83898d1`** ("Merge branch 'agent/term-pane' — a
persistent shell session, and the VT screen it draws into"), on the worktree
`agent/ratatui-survey`. Every letibot citation below resolves at that commit and only there.

The question is not *is ratatui good*. It is **what would we actually get, and what would it
cost, given this tree's head** — whose rendering is hand-rolled in `crates/tui/src/term.rs`
(raw mode, the alternate screen, key decoding, and a row-level diff) and
`crates/tui/src/app.rs` (a 49,959-line head that composes a frame as a `Vec<String>`).

---

## 0. What was read, and how — three classes of evidence, kept apart

This document is written for somebody deciding, so every claim carries which of three kinds
of evidence it rests on. They are not equivalent and are marked inline.

| mark | means | how it was obtained |
|---|---|---|
| **[page]** | a page or README I fetched and read | `web_fetch` / `curl`; the URL is given |
| **[source]** | a crate's **source**, read | from the local cargo registry, `/home/dead/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`; file and line given |
| **[tree]** | this head's own code | `crates/…:LINE` at `83898d1` |
| **[recalled]** | from my own knowledge, not verified here | no citation; treated as a lead, not a fact |

**[source]** is the strongest of the four and it is the one that matters most, because the
three claims that decide this survey — how ratatui diffs, what its `Backend` trait costs to
implement, and what its diff does *not* know — are all answered by reading ratatui's code,
not its documentation. The registry holds `ratatui-0.30.2`, `ratatui-core-0.1.2`,
`ratatui-widgets-0.3.2`, `ratatui-crossterm-0.1.2`, `ratatui-macros-0.7.2` and
`crossterm-0.29.0` unpacked, so this was possible without fetching a tarball.

**Crate versions are as published on 2026-10-06**, read from the crates.io API. Star counts
and last-push dates are from the GitHub API on the same date and they move.

**I did not compile, build, or run anything.** No `cargo add`, no timing, no binary size. That
is the largest hole in this survey and it is listed again in §8.

---

## 1. What ratatui is, and how it works

### 1.1 Immediate mode over a `Buffer` of cells

Ratatui is an **immediate-mode** renderer: for each frame the application renders *all* the
widgets that should be part of the UI, and ratatui diffs the result against the previous
frame. **[page]** <https://docs.rs/ratatui/latest/ratatui/>:

> Ratatui is based on the principle of immediate rendering with intermediate buffers. This
> means that for each frame, your app must render all `widgets` that are supposed to be part
> of the UI. This is in contrast to the retained mode style of rendering where widgets are
> updated and then automatically redrawn on the next frame.

The intermediate buffer is a `Buffer`: a rectangle of `Cell`s, each carrying a symbol (a
grapheme cluster, which may be two columns wide) plus a foreground, a background and
modifiers. **[source]** `ratatui-core-0.1.2/src/buffer/buffer.rs:17`.

### 1.2 The diff on flush is **per cell**, and that is the load-bearing difference

`Terminal::flush` compares the current buffer against the previous one and hands the backend
only the changed cells. **[source]** `ratatui-core-0.1.2/src/terminal/buffers.rs:97`:

```rust
let previous_buffer = &self.buffers[1 - self.current];
let current_buffer  = &self.buffers[self.current];
let mut last_pos = None;
let updates = previous_buffer
    .diff_iter(current_buffer)
    .inspect(|(col, row, _)| { last_pos = Some(Position { x: *col, y: *row }); });
self.backend.draw(updates)?;
```

The iterator yields `(x, y, &Cell)` **for each cell that differs**, and it is careful about
the cases a naive cell-by-cell diff gets wrong: wide characters whose trailing column must be
refreshed, and VS16 emoji whose trailing column visually differs without the symbol changing.
**[source]** `ratatui-core-0.1.2/src/buffer/diff.rs:5-46` — including a
`VISIBLE_ON_BLANK` set of modifiers (`REVERSED`, `UNDERLINED`, the two blinks, `CROSSED_OUT`)
that are visible on a space and so must be emitted even when the symbol has not changed.

What the backend then does with those cells is its own business. The crossterm backend
**moves the cursor to each changed cell and prints that cell**, carrying a running diff of
modifiers and colours so consecutive cells on a row cost no move and no SGR. **[source]**
`ratatui-crossterm-0.1.2/src/lib.rs:232-292`.

**This is the point where ratatui meets this head's hand-rolled frame, and the two are
different shapes.** `crates/tui/src/term.rs` diffs **per row**:

- `paint_full` **[tree]** `term.rs:531` walks the frame's lines, and for each line that
  differs from what is on the glass emits `\x1b[{row};1H\x1b[0m\x1b[K` followed by the whole
  row's text. Exactly one `ESC[K` per repainted row.
- `paint` **[tree]** `term.rs:497` is the same thing against `shown`.
- `Terminal::draw_with_cursor` **[tree]** `term.rs:304` / `paint_to` `term.rs:349` are the
  write path, and `paint_to` treats a failed write as *the glass is unknown*: it clears the
  memory and forces a full repaint next frame.
- `WriteStats` **[tree]** `term.rs:129` counts `frames`, `silent` (frames that wrote zero
  bytes), `bytes`, `rows` (row rewrites), `repeats` and `clears`. Its docstring is explicit
  that `rows` is a *fact* rather than a proxy, because `paint_full` is the only emitter of
  `ESC[K` in the file and emits exactly one per repainted row.

So: **ratatui diffs cells, this head diffs rows.** Neither is obviously better — a cell diff
writes far fewer bytes when one character changes, and a row diff is one escape and one write
per row, which is cheaper when a whole row changes and much cheaper to *count*. The
consequence for the migration is in §5(a) and it is the single largest cost in this survey.

### 1.3 The diff is against ratatui's own memory, and ratatui says so

This head's comment on `paint_full` **[tree]** `term.rs:528` is:

> `full` is for the two cases where the glass really is unknown: a resize, and Ctrl-L — *"the
> diff is against a memory of the screen, and anything that writes behind the head's back
> makes that memory wrong"*.

Ratatui has the identical hazard and names it in the same terms. **[source]**
`ratatui-core-0.1.2/src/terminal/buffers.rs:85`:

> `Terminal::flush` only reasons about Ratatui's internal buffers. It does not know whether the
> backend's display surface changed since the last render pass. For example, if you leave the
> alternate screen and then call `Terminal::flush`, Ratatui may replay a diff that was computed
> for the alternate screen onto the main screen.

**Ratatui does not solve this; it documents it.** Anybody expecting the library to retire the
`full` flag, the resize path, or the "a failed write invalidates the glass" policy should
know that now. That policy is this head's and it survives adoption either way.

### 1.4 `Terminal`, `Backend`, `Frame`, and `Widget`

- **`Terminal`** owns the two buffers, the viewport, and the cursor. `Terminal::draw(|frame| …)`
  runs the closure, diffs, and flushes. **[page]** docs.rs front page.
- **`Backend`** is the escape-writing half. **[source]** `ratatui-core-0.1.2/src/backend.rs:160`.
  The required surface is small — `type Error`, `draw`, `hide_cursor`, `show_cursor`,
  `get_cursor_position`, `set_cursor_position`, `clear`, `clear_region`, `size`,
  `window_size`, `flush` — with `append_lines` defaulted and the scroll-region methods behind
  the `scrolling-regions` feature. `TestBackend` **[source]**
  `ratatui-core-0.1.2/src/backend/test.rs:32` is an in-memory backend whose `Buffer` can be
  asserted on directly.
- **`Frame`** is what the draw closure is handed: `frame.area() -> Rect` and
  `render_widget(widget, area)` / `render_stateful_widget(widget, area, state)`.
  **[source]** `ratatui-core-0.1.2/src/terminal/frame.rs:22,106,147`.
- **`Widget`** is `fn render(self, area: Rect, buf: &mut Buffer)`; `StatefulWidget` adds
  `type State` and a `&mut State`; `WidgetRef`/`StatefulWidgetRef` do the same by reference
  and are **unstable**, behind the `unstable-widget-ref` feature. **[page]** docs.rs widgets
  index; **[source]** `ratatui-core-0.1.2/src/widgets/widget.rs`. The recommended pattern
  since 0.26 is `impl Widget for &MyWidget`.

### 1.5 `Layout` and `Constraint`

`Layout::vertical([…])` / `Layout::horizontal([…])` split a `Rect`; `.areas::<N>(area)` gives
an array, `.split(area)` a `Vec`. **[page]** docs.rs front page, which shows the canonical
three-band example:

```rust
let vertical = Layout::vertical([Length(1), Min(0), Length(1)]);
let [title_area, main_area, status_area] = vertical.areas(frame.area());
```

`Constraint` is `Min(u16) | Max(u16) | Length(u16) | Percentage(u16) | Ratio(u32,u32) |
Fill(u16)`. **[source]** `ratatui-core-0.1.2/src/layout/constraint.rs:72-218`. Excess
distribution is governed by `Flex`. **[source]** `ratatui-core-0.1.2/src/layout/flex.rs:26`.
The solver is **`kasuari`**, a linear-constraint (cassowary-family) solver, not a greedy
allocator. **[source]** `ratatui-core-0.1.2/src/layout/layout.rs:10,136`.

**Where this meets the head:** `compose_screen` **[tree]** `app.rs:12066` builds the frame as
a chrome `Vec<String>` appended above a composer, and it does not use a constraint solver — it
uses a **fit ladder** **[tree]** `app.rs:12236-12288` that deletes the most expendable row
first (`hint`, then `notice`, then composer rows, then the stuck disclosure, then the turn's
status row, then the box, then card content) and stops as soon as `n < h`. `Layout` expresses
*how much room each thing gets*; the ladder expresses *what is given up first*. Those are
different questions and §5(a) says what that means.

---

## 2. The control inventory we would inherit

Ratatui's built-in widgets, from the widgets index **[page]**
<https://docs.rs/ratatui/latest/ratatui/widgets/index.html> (names verified there; behaviour
verified in `ratatui-widgets-0.3.2/src/`):

`Block`, `BarChart`, `calendar::Monthly`, `Canvas`, `Chart`, `Clear`, `Fill`, `Gauge`,
`LineGauge`, `List`, `Paragraph`, `Scrollbar`, `Sparkline`, `Table`, `Tabs`, `RatatuiLogo`,
`RatatuiMascot`; plus `String`, `&str`, `Line`, `Span`, `Text` as widgets in their own right.
Since 0.30 the project is a workspace: `ratatui-core` (traits and text types),
`ratatui-widgets` (the widgets), `ratatui` (the re-exporting app crate), and a backend crate
per backend. **[page]** docs.rs.

Below, each control against **what it would replace in this head, if anything**. "Replace" is
meant strictly: *the thing whose code would be deleted*.

| control | what it is | what it would replace here | verdict |
|---|---|---|---|
| **`Block`** | borders + titles, with `BorderType` sets and `inner()` | `App::box_edge` **[tree]** `app.rs:12647` — the composer's two edges, the left legend, the right legend that yields by truncation. `Block::inner` **[source]** `ratatui-widgets-0.3.2/src/block.rs:762` is exactly the `w - 2` arithmetic at `app.rs:12649`. | **Real replacement.** The head's edge has one thing `Block` does not: two legends (left + right) that negotiate for room, and the reopen-after-reset workaround at `app.rs:12652`. `Block` has `title_top`/`title_bottom` (`block.rs:397,426`), so top-right titles are expressible — but the *yielding* rule would stay caller code. |
| **`Paragraph`** | styled, wrapped text with an optional `Block` | `letibot_ui::width::wrap` + `render::render_block` **[tree]** `render.rs:342`; the decision card `app.rs:16945`, help `app.rs:12470`, status pane `app.rs:17754`, the transcript body | **Partial.** `Paragraph` replaces the *painting*; it cannot replace the model. The transcript's IR is `markdown.rs`'s `Block`/`Run`/`InlineStyle` (3,304 lines) with folding, budgets and syntax colouring, and it is richer than ratatui's `Text`/`Line`/`Span`. See §5(e). |
| **`List`** | rows with selection, `highlight_symbol`, `highlight_style`, `ListState` for scroll-into-view | `subagents_lines` **[tree]** `app.rs:15837`, `jobs_lines` `16312`, `todos_lines` `15634`, `picker_lines` `16433`, `setting_picker_lines` `16759`, `config_lines` `15520`, `help_lines` | **Real replacement, with one snag.** `ListItem` takes multiple `Line`s, so the head's two-row item (a row + a dim subtitle) fits. `ListState` does the scroll-the-selection-into-view job the head hand-rolls at `app.rs:15835`/`16310`. **The snag:** the head *records the screen row each stop was drawn on* (`todos_stop_rows`, `subagents_stop_rows`, `jobs_stop_rows`) so that a **mouse click** maps back to a row (`todo_stop_at_row` `app.rs:11030`). `List` does not report where it drew a row, so that arithmetic has to be re-derived from `ListState::offset` or kept alongside. |
| **`Table`** | rows × columns with per-column `Constraint`s | `render::table_lines` **[tree]** `render.rs:507` and its `fit_columns` natural-width shrinker `render.rs:605` — the markdown table renderer | **Real replacement.** `fit_columns` is a hand-rolled version of what a column `Constraint` does. |
| **`Tabs`** | a tab bar with selection | **nothing** — the head's "screens" are boolean flags (`help`, `stats`, `picker`, `todos_pane`, `config_pane`, `subagents_pane`, `jobs_pane`) dispatched by an if/else chain **[tree]** `app.rs:12449-12509` | **New capability, not a replacement.** Worth having only if the operator wants the screens named on screen. |
| **`Gauge` / `LineGauge`** | a percentage bar | `letibot_ui::progress::prefill_line` (546 lines) and the `Responding · 4.2s · 12.4k tok` row `App::turn_status` **[tree]** `app.rs:17461` | **Not a drop-in.** The prefill bar carries a cache split and a measured-vs-unmeasured distinction (`app.rs:14925-14935`) that a percentage cannot express. |
| **`Sparkline` / `Chart` / `BarChart`** | a data series drawn | **nothing** — the token and spend numbers in the header **[tree]** `app.rs:14827` and the status pane are text | **New capability.** |
| **`Scrollbar`** | a scrollbar in a `Rect` | **nothing** — the head draws `scroll_state()` → `"holding"` on the composer's bottom edge **[tree]** `app.rs:13455`, and `pane_window` **[tree]** `app.rs:15586` tracks `pane_len`/`pane_room` | **New capability.** |
| **`Clear`** | resets every cell in its area | see below | **Different presentation model — a design decision, not a refactor.** |

### The pop-up / modal idiom

Idiomatically a modal is `Clear` followed by a widget in the same `Rect`, with the `Rect`
computed as a centred sub-rectangle of `frame.area()`. **[source]**
`ratatui-widgets-0.3.2/src/clear.rs` — the widget is four lines of nested loops resetting
cells, and its own doc comment is the recipe:

```rust
fn draw_on_clear(f: &mut Frame, area: Rect) {
    let block = Block::bordered().title("Block");
    f.render_widget(Clear, area); // <- this will clear/reset the area first
    f.render_widget(block, area); // now render the block widget
}
```

There is a worked example at `examples/apps/popup/` **[page]** <https://ratatui.rs/examples/apps/popup/>
and a recipe at <https://ratatui.rs/recipes/render/overwrite-regions/> ("Popups (overwrite
regions)"). **[page]** <https://ratatui.rs/recipes/>

**What this head does instead, and why it matters.** The head has **no modal**. Every card —
the decision card (`app.rs:16945`), the quit card (`16708`), the key ask (`16848`), the secret
card (`16875`), the new-todo card (`8679`), the setting picker (`16759`) — is composed
**inline into the chrome stack immediately above the composer**, with the transcript still
visible above it. That is a deliberate choice, stated at **[tree]** `app.rs:12112`:

> The mode card rides in the ask card's slot: a compact card at the bottom of the screen with
> the transcript still visible above it, which is where everything else that wants a choice sits.

So `Clear` + a `Rect` is available, and adopting it would move every card from the bottom edge
to the middle of the screen and make it opaque. **That is a visible behaviour change the
operator would have to want**, and it is not implied by "adopt ratatui".

The head's *screens* (help, stats, config, pickers, todos, subagents, jobs) are a different
case: they **replace** the transcript rather than overlaying it (`compose_screen`'s if/else
chain), so under ratatui they are naturally a `Rect` over the body area — no `Clear` needed.

### The per-element mapping the operator asked for

- **agents pane** → `List` (two-row items, `highlight_symbol` for `▸`, `highlight_style` for
  `REVERSE`), plus the fold row `[+] finished (N)` which is a `ListItem` with a title, and the
  row-record problem above.
- **todo pane** → `List` with a nested indent. The items are drawn under their heading and the
  heading carries the counts (`app.rs:19831-19837`); `List` gives no nesting, so the tree
  structure stays the caller's.
- **jobs pane** → `List`, same shape as the agents pane (`app.rs:16305-16312`).
- **header's git field** → **`Line`/`Span`/`Style`, and this is the clearest small win.**
  `gitfield.rs` (637 lines) produces per-segment colours (`GitState`, `gitfield.rs:68`;
  `GIT_FORMAT_DEFAULT`, `gitfield.rs:58`) and today must compose SGR by hand into a `String`.
  The head's own comment at **[tree]** `app.rs:12652` records the resulting bug class:
  `Palette::paint` closes with a plain reset, which restores the *terminal default* rather
  than the grey of the border it was inlaid into, so the border has to reopen itself. A
  `Span` carries a `Style`; there is no reset to get wrong.
- **pickers** → `List` + `ListState` for the setting picker and the mode picker.
- **completion row** → **nothing in ratatui.** It is a custom row, and its interesting
  property is not its content but its *reservation* (see §5(c)).

---

## 3. The controls ratatui deliberately does NOT give you

This is where the "the lib" half of the operator's question actually lives.

### 3.1 What is missing, stated plainly

**[page]** docs.rs front page, in the crate's own words:

> Ratatui does not include any input handling. Instead event handling can be implemented by
> calling backend library methods directly.

And from the widget index **[page]**: there is **no text box**, **no editor**, **no terminal
or pty widget**, and **no window manager**. The built-in list is the whole built-in list.

- **Input** is the backend's: `crossterm::event` for the crossterm backend, `termion` for
  termion. **[page]** docs.rs; `crossterm 0.29.0` **[source]** in the registry.
- **Text entry, editors, terminal panes, tiling, and pickers** are all third-party.

**This matters more here than in a greenfield app**, because this head already has the input
half: `term::enter` sets raw mode, the alternate screen, bracketed paste (`?2004h`), SGR mouse
(`?1002h`/`?1006h`) and a steady block cursor (`ESC[2 q`) **[tree]** `term.rs:15-30`, and
`term::decode_prefix` **[tree]** `term.rs:667` is a hand-written escape decoder that carries a
partial UTF-8 sequence, a half-arrived escape and an unterminated paste across reads. None of
that is given up by adopting ratatui — ratatui never asked for it. Adopting crossterm would be
adopting *a second reader of stdin* next to this one.

### 3.2 The ecosystem that fills each gap

Metadata below is from the **crates.io API** and the **GitHub API** on 2026-10-06. **I did not
read the source of any of these crates** — every statement is from published metadata or from
a README I fetched, and is marked accordingly.

#### Text input and editors

| crate | version | last release | stars | notes / cost to depend on |
|---|---|---|---|---|
| **`tui-textarea`** | 0.7.0 | **2024-10-22** | 515 ([page] <https://github.com/rhysd/tui-textarea>) | The well-known one, and the most-depended-on: 2.74 M downloads. **Its last release predates ratatui 0.30's workspace split by ~20 months.** Whether 0.7.0 compiles against `ratatui` 0.30 is **not verified here** and is the first thing to check. |
| **`tui-textarea-2`** | 0.13.2 | 2026-08-23 | — | A maintained fork; its own crates.io description says "Compatibility updates for current ratatui releases plus Rust 2024 / rust-version = 1.85.0". 164 k downloads, 100 k recent. **This is the one to look at first.** |
| **`tui-input`** | 0.15.5 | 2026-09-26 | 205 | A **headless** single-line input (state machine, no rendering). Cheap to depend on and does not fight the head's own composer. |
| **`edtui`** | 0.11.7 | 2026-08-16 | 159 | Vim-inspired editor widget. Larger surface; a different interaction model from this head's composer. |
| **`ratatui-code-editor`** | 0.0.6 | 2026-07-07 | — | Tree-sitter syntax highlighting. **Note: `tui-code-editor` does not exist on crates.io**; this is the nearest name. Pre-1.0 and 5 k downloads. Also: this tree already has a tree-sitter integration (`rano`, `crates/tui/Cargo.toml`) and a deliberate rule against a second one. |

**What the head has now:** `letibot_ui::editor` (1,264 lines) — multi-line input, history,
paste, kill ring — plus the composer's layout in `App::composer_rows` **[tree]**
`app.rs:12596`. The head's composer is *not* a gap.

#### Terminal panes

| crate | version | last release | stars | notes |
|---|---|---|---|---|
| **`tui-term`** | 0.3.4 | 2026-04-07 | 232 | A pseudoterminal **widget**: it renders a `vt100::Screen`. Deps **[source via API]**: `ratatui-core ^0.1.0`, `ratatui-widgets ^0.3.0`, optional `vt100 ^0.16.2`, optional `portable-pty ^0.9.0`. Its README **[page]** <https://github.com/a-kenji/tui-term> says of itself: *"This project is currently in active development and should be considered a work in progress."* |
| **`vt100`** | 0.16.2 | 2025-07-12 | 122 | A VT emulator **model** — the screen, cursor and attributes. 13 M downloads. |
| **`vte`** | 0.15.0 | 2025-02-02 | 326 | Alacritty's ANSI parser. 80 M downloads. A *parser*, not a screen: it is the layer under `vt100`, and under this tree's own `vt.rs`. |

**What the head has now:** `letibot_ui::vt::Screen` **[tree]** `crates/ui/src/vt.rs` (1,598
lines) — cursor addressing, erase in display and line, the scroll region, scroll up/down,
insert/delete line, delete/erase char, SGR folded into the palette's roles, the alternate
buffer, save/restore cursor; OSC consumed whole and **dropped**; everything not in its table
dropped. **[tree]** `vt.rs:41-61`. And `letibot_tools::exec::shell` already drives it: *"the
pane's renderer — consumes OSC sequences whole"*, `crates/tools/src/exec/shell.rs:103`, with
`Screen::pane_rows(cols, room, palette)` returning **exactly `room` rows** (`vt.rs:376`).

So for the terminal pane the choice is **not** "add a library" — it is "replace a working
1,598-line emulator, and a working pty, with `tui-term` + `vt100` + `portable-pty`". §7 says
why I would not do that yet and what would change my mind.

#### Tabs and tiling

| crate | version | last release | stars | notes |
|---|---|---|---|---|
| **`ratatui-hypertile`** | 0.4.2 | 2026-10-01 | 313 | A tiling layout engine for ratatui. The healthiest of the three by a wide margin (11 k downloads, MIT, pushed this week). |
| **`ratatui-tabs`** | 0.2.0 | 2026-04-30 | **0** | **45 downloads, 0 stars, and LGPL-3.0.** The licence alone is a question for an Apache-2.0 tree (see `NOTICE`, and `DECISIONS.md` D4/D8 on licence policy). |
| **`tui-tabs`** | 0.1.1 | 2026-03-23 | — | A tab navigation widget with bordered boxes; **MIT OR Apache-2.0**, 9 k downloads. The better-licensed alternative if tabs are wanted. |
| **`ratatui-comfy-tabs`** | 0.5.12 | 2026-07-04 | — | A third option, "highly customizable". |
| **`tuiwindow`** | 0.1.1 | **2024-03-02** | **2** | A minimal window and focus manager. Two stars, no push in two and a half years. **[recalled]** nothing here looks maintained; treat as abandoned. |

**What the head has now:** no tabs and no tiling. Its screens are a single if/else chain
**[tree]** `app.rs:12449`. There is no pane splitting anywhere in the head.

#### Lists, pickers, and scroll helpers

| crate | version | last release | stars | notes |
|---|---|---|---|---|
| **`tui-widget-list`** | 0.15.3 | 2026-07-18 | — | A versatile list, 358 k downloads. Overlaps ratatui's own `List`. |
| **`tui-scrollview`** | 0.6.8 | 2026-09-24 | — | A scrolling view over a larger area. **Part of `ratatui/tui-widgets`** — the org's own merged collection. |
| **`tui-prompts`** | 0.6.8 | 2026-09-24 | — | Interactive prompts. Same org collection. |
| **`tui-popup`** | 0.7.7 | 2026-09-24 | — | A popup component. Same org collection; 852 k downloads. **The nearest thing to a maintained modal.** |
| **`ratatui/tui-widgets`** | 0.7.12 | 2026-09-24 | 238 | The umbrella: *"a crate that combines multiple previously standalone crates into one in order simplify maintenance"*. **Apache-2.0.** |
| **`ratatree`** | 0.4.0 | 2026-09-12 | — | A file/directory picker widget. **Note: `tui-file-picker` does not exist on crates.io.** `ratatui-file-picker` 0.0.0 exists but its description is literally *"Reserved for Ratatui file picker dialogs and file selection controls"* — a squatted name with no code. |
| **`rat-widget`** | — | — | — | **[page]** listed in awesome-ratatui: text-input, date/number input, text-area, checkbox, choice, radiobutton, slider, calendar, view/split/tabbed/multi-page, a table for large data sets, **a file-dialog**, menubar, status-bar, and built-in crossterm event and focus handling. **One crate that fills several gaps at once** — and that is also its cost: it brings its own event handling. |

**What the head has now:** `picker_lines` (`app.rs:16433`), `setting_picker_lines`
(`16759`), `pane_window` (`15586`) with its own scroll clamp. The head has a working picker;
`tui-widget-list`/`tui-scrollview` are alternatives to ratatui's own `List`, not to the head's
gaps.

#### The index

**`awesome-ratatui`** — <https://github.com/ratatui/awesome-ratatui>, **2,037 stars**,
pushed 2026-10-05. **[page]** Its "Libraries" section is organised as Frameworks / Widgets /
Utilities / Bindings, and it is where the 40-odd widget crates are actually enumerated. It is
the right first stop and it is not exhaustive: **`ratatui-tabs`, `ratatui-hypertile` and
`tuiwindow` are all absent from it** as of this reading, so it is an index and not a census.

The ratatui site also maintains a **third-party widgets showcase** **[page]**
<https://ratatui.rs/showcase/third-party-widgets/>.

---

## 4. Recipes and documentation

All of the following were fetched and returned HTTP 200 on 2026-10-06.

**The library's own docs**
- API docs: <https://docs.rs/ratatui/latest/ratatui/> — the crate front page is unusually
  good; it is where the crate organisation, the immediate-mode statement, and the "ratatui
  does not include any input handling" sentence live.
- The website: <https://ratatui.rs/>
- Concepts: [Rendering](https://ratatui.rs/concepts/rendering/) (immediate mode and the
  buffer/diff model), [Under the hood](https://ratatui.rs/concepts/rendering/under-the-hood/),
  [Widgets](https://ratatui.rs/concepts/widgets/), [Layout](https://ratatui.rs/concepts/layout/),
  [Event Handling](https://ratatui.rs/concepts/event-handling/),
  [Backends](https://ratatui.rs/concepts/backends/) and
  [Backend comparison](https://ratatui.rs/concepts/backends/comparison/),
  [Application Patterns](https://ratatui.rs/concepts/application-patterns/) (Elm / component /
  Flux), [Builder Lite Pattern](https://ratatui.rs/concepts/builder-lite-pattern/).
- **Recipes** — <https://ratatui.rs/recipes/>. The ones that bear on this head:
  [UI Layout](https://ratatui.rs/recipes/layout/), [Dynamic Layouts](https://ratatui.rs/recipes/layout/dynamic/),
  [Center a Widget](https://ratatui.rs/recipes/layout/center-a-widget/),
  [Collapse Borders](https://ratatui.rs/recipes/layout/collapse-borders/),
  [Render UIs](https://ratatui.rs/recipes/render/),
  [**Popups (overwrite regions)**](https://ratatui.rs/recipes/render/overwrite-regions/) — the
  `Clear` + `Rect` recipe,
  [Styling Text](https://ratatui.rs/recipes/render/style-text/),
  [**Create custom widgets**](https://ratatui.rs/recipes/widgets/custom/) — the recipe the
  transcript would be written against,
  [Terminal and Event Handler](https://ratatui.rs/recipes/apps/terminal-and-event-handler/),
  [Setup Panic Hooks](https://ratatui.rs/recipes/apps/panic-hooks/) — directly comparable to
  `Terminal::enter`'s hook **[tree]** `term.rs:205-211`,
  [Spawn External Editor (Vim)](https://ratatui.rs/recipes/apps/spawn-vim/) — the "give the
  screen to a subprocess and take it back" problem, which is the same one the terminal pane has.
- **Testing recipes** — [Testing Apps](https://ratatui.rs/recipes/testing/),
  [**Testing with insta snapshots**](https://ratatui.rs/recipes/testing/snapshots/),
  [Debugging Widget State](https://ratatui.rs/recipes/testing/debug-widget-state/). The
  snapshot recipe is the closest thing to what §6 proposes, and it is worth reading before
  writing a harness by hand.
- **Tutorials** — [Hello Ratatui](https://ratatui.rs/tutorials/hello-ratatui/),
  [Counter App](https://ratatui.rs/tutorials/counter-app/) (including a multi-file
  `app/event/main/tui/ui/update` layout that is the standard skeleton), and a **JSON Editor**
  tutorial with an explicit *editing popup* and *exit popup* screen — the most relevant
  walkthrough for this head's card problem.
- **Examples** — <https://ratatui.rs/examples/> (pinned to 0.30.2) and the source tree at
  <https://github.com/ratatui/ratatui/tree/main/examples>. **[page]** the app examples are:
  `demo`, `demo2`, `async-github`, `calendar-explorer`, `canvas`, `chart`, `color-explorer`,
  `colors-rgb`, `constraint-explorer`, `constraints`, `custom-widget`, `hyperlink`, `flex`,
  `hello_world`, `gauge`, `inline`, `input-form`, `modifiers`, `mouse-drawing`, `minimal`,
  `panic`, `popup`, `scrollbar`, `table`, `todo-list`, `tracing`, `user-input`, `weather`,
  `widget-ref-container`, `advanced-widget-impl`. Widget examples live separately under
  `ratatui-widgets/examples/`.
  The three worth reading for this tree: **`custom-widget`** and **`advanced-widget-impl`**
  (the transcript), **`popup`** (the cards), **`inline`** (see below), **`user-input`**
  (composer comparison), **`panic`**.
- **`inline` viewport** deserves a note: `Viewport::Inline` renders into a fixed region of the
  normal screen rather than the alternate screen. This head owns the alternate screen and
  gives it back **[tree]** `term.rs:19`. **[recalled]** I have not read the inline example; it
  is named here because it is the one ratatui feature that could interact with the head's
  screen ownership, and it should be read before any adoption.
- **Templates** — <https://ratatui.rs/templates/> (cargo-generate).
- **FAQ** — <https://ratatui.rs/faq/>.
- **Highlights** — <https://ratatui.rs/highlights/v030/> for the 0.30 workspace split.
- **Changelog / breaking changes** — `CHANGELOG.md` and `BREAKING-CHANGES.md` in the repo.

**Community walkthroughs worth reading**
- **[page]** *"I build a real-time chat application with Tokio and Ratatui"* —
  <https://www.reddit.com/r/rust/comments/1fpokg3/i_build_a_realtime_chat_application_with_tokio/>.
  A chat UI with rooms; the shape (streaming text into a scrolling pane, a composer, an
  async reader) is this head's shape.
- **[page]** The ratatui GitHub topic page — <https://github.com/topics/ratatui> — is where
  the *applications* are, and the applications are more useful than the tutorials for a
  question like this one. `gitui`, `atuin`-adjacent tools, and a long tail of multiplexers
  (`p2pmux`, `termgrid`, `degen-terminal`) and agent-session dashboards (`bosun`, `crmux`,
  `thurbox`, `ilmari`) are listed in awesome-ratatui's Apps section **[page]**. **There is no
  first-party ratatui multiplexer or terminal-emulator app in that list**; the terminal-pane
  problem is solved in the ecosystem at the *widget* level (`tui-term`), not demonstrated in
  a shipped app I could point at.

**This tree's own testing document is the more important one for §6.**
`docs/tui-testing.md` **[tree]** already specifies the technique: two tmux sessions, the same
binary, byte-identical input, `tmux capture-pane -p` (and `-p -e` for SGR), `-x`/`-y` for
narrow terminals, and `letibot-tui --replay FILE.jsonl` so the input is *data you choose*.
It also states the limit that decides §6's gate:

> **A capture is the *rendered* pane, not the byte stream.** It cannot show you frames that
> were written and immediately overwritten, so it is the wrong instrument for repaint volume.
> That question is answered by counting bytes written to the terminal — which is how the 10 Hz
> full-repaint bug was found.

---

## 5. The migration analysis, honestly

### (a) What would be gained

**1. The diffing engine — with a caveat that is the whole story.**

The head's row diff was hand-rolled and it was wrong once, in the way that mattered: the old
`draw` repainted everything unconditionally. **[tree]** `term.rs:277-293` records the
measurement — *"one 28-second session: 269 frames, **221 of them byte-identical to the frame
before**"* — so the whole screen was erased and repainted ten times a second for a screen that
was not changing. That is flicker, a lost selection and a pinned core, and the fix is
`paint_full`.

Ratatui's engine is better than that one *and* better than the current one, in one respect:
it diffs **per cell** rather than per row, and it handles the two cases a cell diff gets
wrong (wide-character trailing columns, VS16 emoji) explicitly. **[source]**
`ratatui-core-0.1.2/src/buffer/diff.rs:5-46`.

**But ratatui does not give the head anything it does not already have on the failure that
actually happened**, because the failure was *repainting when nothing changed*, and the head's
`draw` already answers that with a stronger property than ratatui's: **[tree]** `term.rs:291`

> A frame equal to the last one writes zero bytes. An idle head is silent on its output, not
> merely cheap.

Ratatui does **not** have that property. `Terminal::try_draw` calls `self.flush()`
unconditionally — not gated on there being any updates — then hides or shows the cursor and
positions it, then swaps buffers and flushes the backend **[source]**
`ratatui-core-0.1.2/src/terminal/render.rs:294-308`. With an empty update iterator the
crossterm backend still queues three reset sequences (`SetForegroundColor(Reset)`,
`SetBackgroundColor(Reset)`, `SetAttribute(Reset)`) **[source]**
`ratatui-crossterm-0.1.2/src/lib.rs:277-291`. So an idle ratatui frame writes bytes.
**"An idle head is silent" is this head's property and would be lost unless the tree keeps its
own `Backend`.** §6 stage 0 is built around exactly this.

**2. `Layout`/`Constraint` — a real engine for a problem the head solves by hand, but not for
the head's actual problem.**

The head computes the frame's vertical budget with a fit ladder **[tree]**
`app.rs:12236-12288` whose order of sacrifices is the design (`hint`, then `notice`, then
composer rows, then `stuck`, then `status`, then the box, then card content). `Constraint` can
say *how much* each thing gets; it cannot say *what is given up first*. A ladder is a
priority, and a constraint solver has no priority list of that kind. **So the ladder survives
as caller code either way** — this is an argument from the API surface, not a measurement,
and it is flagged in §8.

What `Layout` genuinely does replace: `render::fit_columns` **[tree]** `render.rs:605` (the
markdown table's natural-width shrinker), and the ad-hoc width arithmetic in `App::gutter`
(`app.rs:12580`) and `composer_rows` (`12596`).

**3. The controls of §2** — `List`, `Table`, `Tabs`, `Scrollbar`, `Gauge`, `Sparkline`,
`Chart`, `Clear`. Roughly half are replacements and half are new capability, and the table in
§2 says which is which.

**4. `Text`/`Line`/`Span` with `Style` instead of escapes in a `String`. This is the best
value in the whole survey and it is easy to miss.**

The head has three separate workarounds for one underlying problem — *a colour is a `String`
containing an escape, so nothing can be composed safely*:

1. `Palette::open` returns a `&'static str` **[tree]** `crates/ui/src/style.rs:148`, so
   painting is string concatenation.
2. `RenderConfig::base` exists solely so that a span inside a themed block closes back into
   its block rather than to the terminal default — the operator's report, quoted at **[tree]**
   `render.rs:107-113`: *"tries to be grey, then goes green and becomes white for several rows
   and then grey again"*. The comment says it plainly: **"A reset is not a restore."**
3. `App::box_edge` **[tree]** `app.rs:12652` has to reopen the border after an inlaid legend's
   reset for the same reason.

A `Span` carries a `Style`; there is no reset and nothing to reopen. That single change would
delete a class of bug rather than three instances of it.

**5. `TestBackend`** **[source]** `ratatui-core-0.1.2/src/backend/test.rs:32` and the
`insta` snapshot recipe. The gain here is smaller than it looks, because the head already has
the same shape: `App::screen(term_w, h) -> Vec<String>` **[tree]** `app.rs:12034` is a pure
function over the app state with **508 call sites in `app.rs`**. The head's harness is already
a frame-level one.

**6. The terminal pane.** `tui-term` + `vt100` is a real alternative to `crates/ui/src/vt.rs`.
See (g).

### (b) What would be lost — the part that matters

**1. `WriteStats`, and with it the only instrument that answers "how much of the glass did
that redraw".**

**[tree]** `term.rs:117-127`: `rows` counts `ESC[K`, and *"`paint_full` is the only thing in
this file that emits that sequence and it emits exactly one per row it repaints, so the count
is the fact rather than a proxy for it"*. `frames` and `silent` answer *"is it drawing when
nothing changed"*; `bytes` and `repeats` answer *"how much"* and *"is it repeating"*.

`docs/tui-testing.md` **[tree]** says a `tmux capture-pane` **cannot** answer repaint volume,
and that this counter is what can. The counter is read through
`LETIBOT_TUI_WRITE_STATS` **[tree]** `term.rs:38-43`, which is also how the 10 Hz bug was
found.

Ratatui's per-cell path has no equivalent: the crossterm backend writes cells and resets, and
there is no row to count. **Verdict: genuinely lost in the "crossterm backend" variant.
Survives only in the "own `Backend`" variant**, where the tree's `draw` materialises the
changed cells back into a row buffer and hands the whole frame to `paint_full`, which
recomputes the row diff and keeps the counters. §6 stage 0 is that variant and its gate is
that this instrument still reads the same.

**2. The frame's arithmetic, as *tested*.**

`app.rs` has **448 `#[test]`s** and **508 call sites of `App::screen(...)`**, 501 of them with
literal sizes. Many of them assert
positions, not just content. The suggestion-slot test is the model **[tree]** `app.rs:47581`:

```rust
let composer_at = |f: &[String]| f.iter().position(|l| l.contains('╭')).expect("…");
assert!(candidates[top - 2].contains("! cargo test 199"),
        "the slot above the status row is where the suggestion is drawn:\n{}", …);
```

Under ratatui the frame is a `Buffer` written by widgets, and *which row the composer's top
edge is on* becomes a fact about a layout the head no longer computes. **Verdict: needs
re-establishing, row by row.** It is re-establishable — `TestBackend` gives a `Buffer` and the
rows can be extracted to `Vec<String>` — but each assertion has to be re-pointed, and the ones
that encode a *design* decision (not an accident) are the ones worth keeping.

**3. The reserved suggestion slot.**

`completion_slot()` **[tree]** `app.rs:9792` is a predicate on the composer's text; the row is
counted in the ladder **by the slot and not by the text** (`app.rs:12249`), drawn **empty**
when the prefix matches nothing (`12325-12332`), and deliberately **not** a rung of the ladder
(`12328`), because a row the fit loop may delete is a row that appears and disappears again.
This is the operator's own reported defect — *"the conversation jumps one line up and then
down"* (`app.rs:47563`) — and it is a hand-built invariant.

**Verdict: survives, and cheaply.** `Constraint::Length(1)` on an area *is* a reserved slot —
"always one row" is exactly what it means. So the slot is expressible, the invariant moves
from "the ladder counts the slot" to "the `Constraint` list has a `Length(1)` for it", and the
test at `47581` re-points at that. The same applies to the turn's status row, which is
reserved the same way (`show_status = true`, `app.rs:12228`).

**4. The transcript's bespoke rules — and this is the most important line in the survey.**

`item_lines` **[tree]** `app.rs:21709` and `body_window` (`13559`) own all of:

- **folding** — `Fold`, `fold_cells` (`20840`), `folded_notice` (`20708`), the `Budget`
  (`render.rs:79`), and `BlockCache` (`render.rs:734`) which renders the frozen prefix once
  per width;
- **the rungs** — `Visibility`, five rungs, `rung_state` (`13478`), and the rule that a rung
  hides whole rows while `folded`/`tools`/`reasoning` hide bodies;
- **the one-line header's sanitising** — `without_control_lines`
  (`crates/transcript/src/sanitize.rs:219`), and the loss `crates/ui/src/ansi.rs:40-45` states
  openly: a one-line `! ls` is drawn plain where a four-line `! ls -la` is drawn coloured,
  because the two readers disagree;
- **ANSI→role painting** — `letibot_ui::ansi::painted`, whose whole safety argument is at
  `crates/ui/src/ansi.rs:23`: *"No sequence is ever passed through."*;
- **provenance marks** — the `▌` accent bar, and the test at `app.rs:31849` that asserts the
  provenance is readable **with and without colour** (i.e. under `Palette::None` too);
- **`RowClass`** **[tree]** `app.rs:21470` and the packing rules at `13050-13092` that decide
  when two adjacent `Activity` rows are one block.

**None of this is a widget. It is a model.** ratatui's `Text`/`Line`/`Span` cannot express a
fold, a rung, a budget, or a provenance mark — and it does not need to, because none of it is
about drawing. **Verdict: survives as a custom `Widget`, and only the painting half moves.**
`markdown.rs` (3,304 lines), `render.rs`'s block model, the fold logic, the rung logic, the
budget, the sanitiser and the `ansi` fold are **untouched by adopting ratatui**. That is the
single most reassuring fact in this survey, and it is the reason a staged adoption is
possible at all: the expensive, defect-hardened half of this head is not the half ratatui
replaces.

**5. `Palette` and `head_vocabulary`.**

`head_vocabulary()` **[tree]** `app.rs:31325` is the positive list of every escape this head
may emit — the `sgr` constants plus every non-empty `Palette::open`. `only_the_heads_own_escapes`
(`31377`) strips them and asserts no `ESC` remains. Its companion is
`no_escape_from_content_this_head_did_not_author_reaches_the_terminal` (`33885`).

Under ratatui the paint is a `Style`, and the guarantee becomes *"every `Style` is built from
`Palette`'s roles"* — which is **stronger**, because a `Style` cannot carry `ESC[?1002h` at
all; the type makes the hostile case unrepresentable rather than testing for it.
**Verdict: the guarantee is strengthened; the test has to be rewritten**, and it has to move
to the `Buffer`→`Vec<String>` boundary, because that is where escapes exist again.

Two things about `Palette` that must not be lost:

- `Palette::None` **[tree]** `crates/ui/src/style.rs:143` emits **no sequences at all** — "not
  a monochrome theme: the output is plain text, which is what a replay diff and a CI log
  need". **Ratatui has no equivalent.** `Style::default()` is not the same statement. This
  must be kept and applied at the boundary.
- The palette is deliberately **the ANSI 16, never the 256-cube and never truecolour**
  **[tree]** `style.rs:14-46`, because cube indices are absolute RGB and paint *beside* the
  reader's theme. ratatui's `Color` has `Reset`/`Black`..`White`/`Indexed`/`Rgb`, so the 16
  are expressible — but the discipline is the head's, not the library's.

The 22 `Role`s **[tree]** `style.rs:51` map cleanly: `Role::Success` = `\x1b[32m` =
`Color::Green`; `Role::UserBlock` = `\x1b[7m` = `Modifier::REVERSED`; `Role::Heading` =
`\x1b[1;36m` = `Color::Cyan | Modifier::BOLD`.

**6. The event loop and the terminal modes.**

`term::enter` **[tree]** `term.rs:195-202` sets `?1049h ?25l ?2004h ?1002h ?1006h ESC[2 q`,
and `term.rs:25-30` wraps every frame in DEC 2026 (`?2026h`/`?2026l`) because *"a torn frame
at 10 Hz is exactly what 'flicker' describes"*. `VMIN=0 VTIME=1` (`189-190`) is a 100 ms read
timeout so the render loop can service the network **without a second thread**.

The module's own reason for existing **[tree]** `term.rs:3-6`:

> Fifty lines of `termios` instead of a TUI framework, for the same reason `letibot-turn`
> writes its own HTTP: what this needs is byte-level control of one well-understood interface,
> and a framework brings an event loop that would then be the second one in the process.

**Verdict: genuinely lost under `ratatui::run()` + crossterm, and the reason is unchanged.**
`ratatui::run()` **[page]** docs.rs, and crossterm's `event::poll`/`read` would be a second
reader of stdin next to `Terminal::keys` **[tree]** `term.rs:253`. I did not verify whether
the crossterm backend preserves DEC 2026, bracketed paste, SGR mouse or the steady block
cursor — see §8 — but the head would be trading a mode set it chose for a mode set it does
not control.

**7. `--replay` / `--no-tty` / byte-identical output.**

`head.rs` **[tree]** `head.rs:26-33` documents `--replay FILE.jsonl`, `--demo` and `--no-tty`,
and `style.rs:9-12` says `Palette::None` exists so the output is "byte-identical on every
machine". `docs/tui-testing.md` **[tree]** builds its whole testing technique on `--replay`.

**Verdict: a hard constraint on any adoption.** Whatever renders, `--replay` must still
produce the same bytes for the same input. This is also the reason the `Buffer`→`Vec<String>`
boundary is the right seam: it is the only place where both properties can hold at once.

**8. The terminal pane's structural safety guarantee.**

`vt.rs`'s guarantee is **structural**, not a sanitiser **[tree]** `vt.rs:56-61`:

> **Everything not in that table is dropped** … a cell holds a `char` and a [`Role`], and a
> row is composed from cells. **No byte a program writes can reach the frame** — not
> `ESC[?1002h`, which turns the operator's mouse reporting off, not `ESC[?2026h`, not an OSC
> title, not a C1 control, not DEL.

`vt100` keeps a `Cell` with a foreground and a background. Whether it can be restricted to
this palette's sixteen roles the way `crate::ansi::apply` restricts a line is **not verified**
(§8). **Verdict: needs re-establishing, and it is the one place where the in-tree version has
a property the library's may not.**

### (c) Summary table

| what | survives? |
|---|---|
| `WriteStats` / `LETIBOT_TUI_WRITE_STATS` | **Only with an own `Backend`.** Lost under crossterm. |
| "An idle head writes zero bytes" | **Only with an own `Backend`.** |
| The fit ladder's *order of sacrifices* | Survives as caller code (no `Constraint` expresses it). |
| The reserved suggestion slot | Survives — `Constraint::Length(1)` is exactly a slot. |
| The reserved turn-status row | Survives, same way. |
| The exact-row test assertions (508 `screen()` sites, 448 tests) | **Needs re-establishing, row by row.** |
| The transcript's folds, rungs, budgets | **Survive untouched** — they are a model, not a painter. |
| The one-line header's sanitising | **Survives untouched** (`transcript::sanitize`). |
| ANSI→role painting | **Survives untouched** (`ui::ansi`). |
| Provenance marks | **Survive untouched.** |
| `Palette` (roles, the 16, no-cube discipline) | **Survives, and must.** |
| `Palette::None` (byte-identical replay) | **Survives, and must** — no ratatui equivalent. |
| `head_vocabulary` as a *test* | Rewritten; the guarantee is **stronger** under `Style`. |
| The event loop, `VMIN/VTIME`, DEC 2026, bracketed paste, SGR mouse, block cursor | **Lost under `ratatui::run()` + crossterm.** |
| `vt::Screen`'s "no byte reaches the frame" | **Needs re-establishing** if `vt100` replaces it. |
| `markdown.rs`, `render.rs`'s block model, `gitfield.rs`'s git-state reading, `editor.rs` | **Untouched.** |

---

## 6. A staged path, with the acceptance test for each stage

The stages are ordered so that each one's failure is cheap and each one's gate is a
measurement rather than an opinion. **Stage 0 is the whole survey's load-bearing claim**; if
it fails, stages 1–4 are not worth discussing.

### Stage 0 — the plumbing, with no new widgets

Implement `ratatui_core::backend::Backend` over this head's `Terminal`
**[tree]** `term.rs:60`. The required surface is ten methods **[source]**
`ratatui-core-0.1.2/src/backend.rs:160-331` — `draw`, `hide_cursor`, `show_cursor`,
`get_cursor_position`, `set_cursor_position`, `clear`, `clear_region`, `size`, `window_size`,
`flush`. `size` comes from `Terminal::size()` (`term.rs:234`, `TIOCGWINSZ`).

Then render the head's **existing** frame, unchanged, as a single `Text` in `Frame::area()`,
so the two renderers are identical **by construction** and the only variable is the plumbing.

The design decision that makes this stage worth doing: the backend's `draw` does **not** write
cells. It applies the incoming `(x, y, &Cell)` updates to a shadow `Buffer`, renders that
buffer to `Vec<String>`, and hands the whole frame to **`paint_full`** **[tree]**
`term.rs:531`. That keeps the row diff, keeps `WriteStats::rows` meaningful, and keeps "an
idle head writes zero bytes" — the three things §5(b) says are otherwise lost.

**Acceptance test.** Over `--replay` of **three real sessions** out of the store (the
technique is `docs/tui-testing.md` **[tree]**: one short, one 14-round tool-heavy turn, one
truncated mid-turn with `head -n K`):

1. `LETIBOT_TUI_WRITE_STATS` output is **identical** before and after, on the same replay.
   *This is the instrument that must survive, and it is the gate.*
2. `tmux capture-pane -p -e` is byte-identical at 100×28, 60×20 and 40×12.
3. `--no-tty` output is byte-identical.

**If this fails, stop.** The failure means ratatui's `Terminal` decides something about the
cursor, the clear, or the ordering that this head cannot express, and no widget port fixes it.

### Stage 1 — the transcript as a custom `Widget`

`impl Widget for &Transcript<'_>`, whose `render` walks the same `SnapshotItem`s through the
same `item_lines` **[tree]** `app.rs:21709` and produces `Vec<Line>` instead of
`Vec<String>`. The recipe is [Create custom widgets](https://ratatui.rs/recipes/widgets/custom/);
the examples are `custom-widget` and `advanced-widget-impl`.

This is the right first widget because it is the largest and the most bespoke, and because
§5(b)(4) says its *model* does not move — so the stage is a painter swap and nothing else.

**Acceptance test.**
1. Frame-for-frame equality against the old renderer over the same three sessions at three
   widths. Every difference is either eliminated or **written down as intended**, with the
   reason.
2. The transcript's own tests — folding, rungs, budgets, the `BlockCache` ratio, provenance,
   `RowClass` packing — pass **unchanged**, because they test `item_lines` and not the painter.
   If one of them needs changing, that is a finding, not a chore.
3. `no_escape_from_content_this_head_did_not_author_reaches_the_terminal` **[tree]**
   `app.rs:33885` passes at the new boundary.

### Stage 2 — the panes

`List` for the agents, jobs, todos and picker panes; `Table` for markdown tables; `Block` for
the composer's edges. Port **one pane per commit**, each with its own frame comparison.

The one open question this stage must answer: the head records the screen row each stop was
drawn on (`todos_stop_rows`, `subagents_stop_rows`, `jobs_stop_rows`) so a **mouse click**
maps back to a row (`todo_stop_at_row`, `app.rs:11030`). Either `ListState::offset` reproduces
that arithmetic exactly, or the pane keeps its record and `List` is used for painting only.
**Decide it by test, not by argument** — the click path has a test already, and it is the
acceptance test.

**Acceptance test.** Per pane: frame equality at the three widths, and the pane's own
keyboard/click tests green without modification.

### Stage 3 — the modal, and only if the operator wants it

`Clear` + a centred `Rect` for the decision card, quit card, key ask and secret card.

**This is not a refactor and its gate is not a test.** §2 says why: the head deliberately puts
these cards at the bottom with the transcript visible above them (`app.rs:12112`), and a
`Clear`-based modal makes them centred and opaque. **Acceptance: the operator looks at a
`capture-pane` of both and picks.** If the answer is "keep the bottom cards", the head writes
its own `Widget` that draws a card in a `Rect` without clearing — which is entirely fine and
is a smaller change than `Clear`.

### Stage 4 — the terminal pane

Compare `tui-term` + `vt100` against the in-tree `letibot_ui::vt::Screen`.

**Acceptance test.** A **byte-stream corpus**, not a screenshot: feed the same bytes to both
emulators and compare the rows. The inputs already exist — `vt.rs`'s own tests, and the shell
session's sentinel-framed output (`crates/tools/src/exec/shell.rs`, whose module header at
`:103` and `:234` already states that `Screen` consumes those bytes and returns exactly `room`
rows). The corpus should include the sequences in `vt.rs`'s table (`vt.rs:41-54`) plus the
hostile ones (`?1002h`, an OSC title, a C1 control, DEL) that must be dropped.

Three outcomes, all acceptable: `vt100` matches and is adopted; `vt100` differs and the
differences are enumerated and judged; or `vt100` cannot be restricted to `Palette`'s roles and
the in-tree emulator stays. **The one unacceptable outcome is adopting it without the corpus.**

### What must be true before the old renderer can be deleted

Stated as facts, each of which is checkable:

1. Every test in `app.rs` that asserts on `screen()` passes against the ratatui path, or has
   been rewritten **with the old expectation recorded as wrong** and the reason in the commit
   message.
2. `WriteStats` still answers *"how many rows did that frame rewrite"* — either because the
   head kept its own `Backend` (stage 0's design), or because a replacement instrument was
   built **and calibrated against the old one on the same replay**. An instrument with no
   calibration is not an instrument.
3. `--replay`, `--no-tty` and `Palette::None` produce byte-identical output for the same input,
   and that has been checked on a second machine or at least a second `TERM`.
4. `no_escape_from_content_this_head_did_not_author_reaches_the_terminal` passes, and
   `only_the_heads_own_escapes` has an equivalent at whatever boundary escapes now exist.
5. The three sessions' frames match at three widths and the operator has looked at a
   `capture-pane` of each.
6. **A day-long session with `LETIBOT_TUI_WRITE_STATS` on, and the byte count is not worse
   than the old renderer's by more than a stated factor.** This is the honest one: a per-cell
   diff should write fewer bytes for a small change and *more* for a large one, and nobody
   knows which way this head's traffic goes until it is measured. A row diff that rewrites one
   row costs one `ESC[K` plus the row; a cell diff that changes every cell of that row costs a
   `MoveTo` per cell plus the SGR diff. On a streaming transcript, whole rows change constantly.

---

## 7. The recommendation

**Adopt the models, keep the frame: depend on `ratatui` with `default-features = false` and
no backend crate, implement `Backend` over this head's own `Terminal`, and port the transcript
first. Do not adopt `ratatui::run()` or crossterm. Do not replace the terminal pane yet.**

Reasoning, in the order the decision actually turns on:

**1. The expensive half of this head is not the half ratatui replaces.** Folding, rungs,
budgets, the sanitiser, ANSI→role painting, provenance, the markdown block model and the
composer all stay exactly as they are (§5(b)(4)). What moves is how a `Vec<String>` or a
`Vec<Line>` reaches the glass. That asymmetry is what makes a staged adoption possible and it
is why the risk is lower than the size of `app.rs` suggests.

**2. The things that would break are the things this head paid for in defects** — the byte/row
instrument (§5(b)(1)), the reserved slots (§5(b)(3)), the exact-row assertions (§5(b)(2)), and
the event loop's single-reader property (§5(b)(6)). Ratatui does not solve the first: its
`Terminal::flush` diffs against its own previous buffer and its own docs say it *"does not know
whether the backend's display surface changed since the last render pass"*
**[source]** `buffers.rs:85` — which is this head's comment at `term.rs:528`, in the library's
own words. An own `Backend` over `paint_full` keeps all four; the crossterm backend keeps none.

**3. The cheapest real win is `Style` instead of escapes in a `String`.** It retires three
separate workarounds (`Palette::open`'s string concatenation, `RenderConfig::base`'s "a reset
is not a restore", and `box_edge`'s reopen) and makes the §3.1 guarantee a type property
rather than a test. That win arrives with stage 1 and it does not depend on any of the riskier
stages.

**4. The controls are worth having, but they are additive.** `List` and `Table` replace
hand-rolled code with a row-record cost (§2); `Tabs`, `Scrollbar`, `Chart`, `Sparkline` are
new capability, not replacements, and none of them is a defect the operator has reported. They
are a reason to be on ratatui, not a reason to move in one step.

**5. The terminal pane — the case the operator raises most — is the one where the library is
least obviously the answer.** `tui-term` is a *painter* over `vt100`'s model, and this head
already has both halves: a working pty (`crates/tools/src/exec/pty.rs`, `shell.rs`) and a
1,598-line emulator whose safety guarantee is structural — *"a cell holds a `char` and a
`Role`"*, `vt.rs:58`. Adopting the painter means adopting the model. The right next move is
not a decision but the corpus comparison in §6 stage 4.

**6. What I would refuse outright is `ratatui::run()` plus crossterm.** It puts a second event
loop in the process, which is the reason `term.rs` exists at all (`term.rs:3-6`), and it hands
the frame's bytes to a backend with no `rows` counter and no `Palette::None`. The library is
worth taking; the framework's loop is not.

### The next piece of evidence that would change my mind

**Primary: the stage 0 measurement.** If wrapping an own `Backend` over `paint_full` comes out
**byte-identical and with `WriteStats` unchanged**, then the risk collapses to "write the
widgets" and I would move to adopting the widgets wholesale (still keeping the loop and the
palette). If it *cannot* be made byte-identical — because ratatui's `Terminal` decides when to
`clear()`, where the cursor goes, or in what order the writes land, in a way the head cannot
express through the `Backend` trait — then the honest answer is a much smaller one: **models
only at the text level** (`Text`/`Line`/`Span`/`Style` for the palette and the composer's
edges, no `Terminal`, no `Buffer`). That is a smaller change than this survey is about, and it
might be the right one anyway.

**Secondary, and it is independent of everything above: one measured
`LETIBOT_TUI_WRITE_STATS` comparison on a real session.** If a per-cell diff writes materially
fewer bytes than the row diff on this head's actual traffic — streaming markdown, a growing
transcript, a mostly-idle screen — then the diff is worth replacing first and on its own, and
the argument for the rest of the library gets easier. If it writes *more*, that is the strongest
possible argument for keeping the frame and taking only the models.

**Tertiary:** if the operator actually wants centred, opaque cards, stage 3 becomes a
requirement rather than an option and the calculus shifts toward a fuller adoption, because
the head's bottom-card arrangement is then the thing being replaced rather than preserved.

---

## 8. What I could not verify

Stated plainly, because each of these is a place where a reader could reasonably expect more
than this document gives.

1. **I did not compile, build, or run anything.** No `cargo add`, no `cargo build`, no timing,
   no binary size, no dependency-closure check. Every claim about *cost* in this survey is
   about API surface and code shape, not about measured work.
2. **I did not read the source of any ecosystem crate.** Every statement about `tui-textarea`,
   `tui-textarea-2`, `tui-input`, `edtui`, `ratatui-code-editor`, `tui-term`, `vt100`, `vte`,
   `ratatui-tabs`, `ratatui-hypertile`, `tuiwindow`, `tui-widget-list`, `tui-scrollview`,
   `tui-prompts`, `tui-popup`, `ratatree` and `rat-widget` is from published metadata or a
   README I fetched — not from their code. The only crate sources I read are ratatui's own.
3. **I did not check whether `tui-textarea` 0.7.0 compiles against ratatui 0.30.** Its last
   release was 2024-10-22, well before the 0.30 workspace split. The maintained fork appears to
   be `tui-textarea-2` (0.13.2, 2026-08-23), whose own description cites "compatibility updates
   for current ratatui releases". This is the first thing to check if text input is wanted.
4. **`tui-code-editor` and `tui-file-picker` do not exist on crates.io under those names.** The
   nearest are `ratatui-code-editor` (0.0.6, pre-1.0) and `ratatui-file-picker` (0.0.0 — a
   *reserved* name whose description says so, with no code) or `ratatree` (0.4.0). I did not
   evaluate `ratatui-code-editor`'s quality, and I note that this tree already has a
   tree-sitter integration (`rano`) with a stated rule against a second one.
5. **Licences are not fully checked.** `ratatui-tabs` is **LGPL-3.0**, which is a question for
   an Apache-2.0 tree (`NOTICE`; `DECISIONS.md` D4 and D8 record that this tree treats licence
   compatibility as a decision rather than a default). The others I saw are MIT or
   Apache-2.0, but I did not enumerate every transitive dependency of every candidate.
6. **I did not test ratatui against this head's terminal modes.** Whether the crossterm backend
   preserves DEC 2026 (`?2026h`/`?2026l`), bracketed paste (`?2004h`), SGR mouse
   (`?1002h`/`?1006h`) or the steady block cursor (`ESC[2 q`) that `term::enter` sets is
   **unverified**. This is a real gap, and it is the kind that only shows up on a real terminal.
7. **My claim that the fit ladder survives as caller code is an argument from the API surface,
   not a measurement.** I read `Constraint`'s variants and `Flex`'s priority list; I did not
   build a `Layout` for this head's actual budget problem and see whether a constraint
   formulation reproduces the sacrifice order. A counter-example would be worth having.
8. **I did not verify `vt100`'s colour model** — specifically whether it can be restricted to
   the ANSI 16 roles the way `letibot_ui::ansi::apply` restricts a line. If it cannot, §5(b)(8)
   is decisive on its own.
9. **I did not read ratatui's `inline` viewport example.** It is named in §4 because it is the
   one ratatui feature that could interact with this head's ownership of the alternate screen,
   and it should be read before any adoption.
10. **Star counts, download counts and last-push dates are as of 2026-10-06 and they move.**
    The "looks maintained" judgements in §3 are inferences from `pushed_at` and release dates,
    not from reading the crates.
11. **I did not check whether `awesome-ratatui` is complete.** It is missing at least
    `ratatui-tabs`, `ratatui-hypertile` and `tuiwindow`, all of which are named in the
    operator's question, so it is an index and not a census — and the absence of a crate from
    it is not evidence about that crate.
