# Ratatui survey - what this head took, and what it refused

The question was never *is ratatui good*. It was what this head would gain, and what it would lose, from
giving its drawing to a library. The answer, and the one the tree follows: **take ratatui's models, keep the
frame.** `Text`/`Line`/`Span`/`Style` instead of escapes in a `String`; no `ratatui::run()`, no crossterm
backend, no `Terminal` of ratatui's. The frame is still this tree's - a `Vec<String>` composed by
`App::screen` and painted by a row diff.

That split is not a compromise; it is where the defects were. The evidence below is what decided it.

## 1. The comparison

### 1.1 Two renderers, and the difference that matters

Ratatui is immediate-mode: every frame renders all its widgets into a `Buffer` of cells, and `Terminal::flush`
hands the backend only the cells that differ from the previous frame. The diff is careful about the cases a
naive cell diff gets wrong - wide characters' trailing column, VS16 emoji - and about modifiers that are
visible on a space (`REVERSED`, `UNDERLINED`, the blinks).

This head diffs **per row**: a frame is `Vec<String>`, and each row that differs from what is on the glass is
written absolutely after one `ESC[K`. rano's `Terminal::draw` is that painter, and its `WriteStats` counts
`frames`, `silent` (frames that wrote zero bytes), `bytes`, `rows` (row rewrites, counted by the `ESC[K` that
begins each) and `clears`.

Neither diff is obviously better. A cell diff writes far fewer bytes when one character changes; a row diff is
one escape and one write per row, cheaper when a whole row changes - and on a streaming transcript a whole row
changes constantly - and far cheaper to *count*.

### 1.2 The four properties ratatui does not have

These are the ones that decided it, and each is checkable against ratatui's source.

**An idle frame writes zero bytes.** This head's painter has that property: a frame equal to the last one
writes nothing, so an idle head is silent on its output, not merely cheap. Ratatui does not: `try_draw` calls
`flush` unconditionally and the crossterm backend queues three reset sequences even with an empty update
iterator. Losing this is losing the instrument that found the 10 Hz full-repaint defect.

**A row counter.** "How much of the glass did that redraw" is the question a lost mouse selection asks, and a
`tmux capture-pane` cannot answer it - a capture is the rendered pane, not the byte stream. `rows` answers it,
and a per-cell path has no row to count.

**`Palette::None`, which is not a theme.** Under it the head emits no sequences at all: the output is plain
text, which is what `--replay`, a pipe to a file and a CI log need, and it is byte-identical on every machine.
`Style::default()` is not the same statement.

**One event loop.** `term` exists as fifty lines of `termios` for the same reason `letibot-turn` writes its own
HTTP: what it needs is byte-level control of one well-understood interface, and a framework brings an event
loop that would be the second one in the process. `ratatui::run()` plus crossterm's `event::poll`/`read` would
be a second reader of stdin next to this head's own.

### 1.3 What ratatui had that was worth taking

**`Span` carries a `Style`.** The head had three separate workarounds for one problem - a colour was a `String`
containing an escape, so nothing composed safely: `Palette::open` returned a `&'static str` and painting was
concatenation; `RenderConfig::base` existed so a span inside a themed block closed back into its block rather
than to the terminal default (*"a reset is not a restore"*); and `box_edge` had to reopen its border after an
inlaid legend's reset. A `Span` carries its style, so there is no reset and nothing to reopen. That retires a
class of bug rather than three instances of it, and it is the one thing the library gave that this head could
not build for itself more cheaply.

**A `Buffer` boundary.** Rendering into a rectangle of cells and materialising it back into rows is the seam
where both properties can hold at once: the widgets speak cells, the painter speaks rows, and the row diff and
its counters survive. rano's `render` module is that boundary, and this head's frame still leaves as
`Vec<String>`.

## 2. What was refused, and why

**`Layout` and `Constraint`.** A constraint solver says *how much room each thing gets*. This head's problem is
*what is given up first*: the fit ladder deletes the most expendable row until the frame fits with a line of
transcript left over, and that order is the design. A solver has no priority list of that kind, so the ladder
survives as caller code either way - it is `rano::agent::fit` now, ported verbatim from this tree's own.

**`List`, `Table`, `Tabs`, `Clear`.** Real replacements for hand-rolled painting, and additive rather than
defects: none of them answers a report the operator made. The one that is not a refactor at all is `Clear` plus
a centred `Rect` for the cards. Every card - the decision card, the quit card, the key ask, the secret card -
is composed inline into the chrome stack above the composer with the transcript still visible, and that is
deliberate. A `Clear`-based modal makes them centred and opaque: a visible behaviour change the operator would
have to want, and not implied by "adopt ratatui".

**`ratatui::run()` and the crossterm backend.** §1.2, four times over. The library is worth taking; the
framework's loop is not.

**`tui-term` plus `vt100` for the terminal pane.** `crates/vt` is a working emulator whose safety guarantee is
*structural*: a cell holds a `char` and a `Role`, no sequence is ever passed through, and no byte a program
writes can reach the frame - not `ESC[?1002h`, not an OSC title, not a C1 control. `tui-term` is a painter over
`vt100`'s model, so adopting the painter means adopting the model, and whether `vt100` can be restricted to the
sixteen roles the way `letibot_ui::ansi` restricts a line is unverified. The comparison that would settle it is
a byte-stream corpus fed to both emulators - the sequences in `crates/vt`'s table plus the hostile ones that
must be dropped - and it has not been run. The in-tree emulator stays until it is.

## 3. The constraints that still bind a drawing or a key

Each of these is a property the tree has paid for, and each survives any future change to how a frame reaches
the glass.

| constraint | why |
|---|---|
| A frame equal to the last writes zero bytes | an idle head is silent, not merely cheap - and it is how a repaint storm is noticed |
| `rows` counts row rewrites by the `ESC[K` that begins each | the only instrument that answers "how much of the glass did that redraw"; a capture cannot |
| `Palette::None` emits nothing and is byte-identical on every machine | `--replay`, a pipe, a CI log |
| Roles, never colours; the ANSI 16, never the 256-cube | cube indices are absolute RGB and paint *beside* the reader's theme; two shades of one hue are indistinguishable under half the themes, so a distinction that matters is made with an attribute |
| `Role::UserBlock` is reverse video | the reader's own pair swapped is legible under any theme; a fixed dark-on-light is a black bar on a light terminal |
| The head's own escapes are a positive list, and content cannot add one | `head_vocabulary()` is what the frame may emit, `only_the_heads_own_escapes` asserts nothing else is there, and `no_escape_from_content_this_head_did_not_author_reaches_the_terminal` holds the other side |
| `crates/vt` never passes a sequence through | a program in the pane must not be able to turn the operator's mouse off or write an OSC title |
| `--replay` and `--no-tty` produce the same bytes for the same input | the whole testing technique in `docs/tui-testing.md` rests on it |
| The fit ladder's order of sacrifices | `hint`, then the notice, then composer rows, then the stuck disclosure, then the composer's box, then card content - while `stopping` and `link` are counted in full and never given up |
| The turn's status row is reserved, not conditional | the operator: *"keep the line reserved for `Responding...` always free, or we have these ugly jumps"*. A row that appears when a turn starts shoves the transcript the reader is reading |
| The completion slot is counted by the slot, not by the text in it | a row the fit loop may delete is a row that appears and disappears again - the same jump, from the other side |
| A card's content is windowed, never shrunk whole | trimming a card from its end takes the ladder, the deadline and the hint first, and leaves the wall |
| A click target's rectangle is recorded by the frame that drew it | the head takes the mouse, so nothing else knows where a row was; `app/hits.rs` is the one record and `Target` is the one vocabulary |

## 4. If more is ever taken: the staged path

The stages are ordered so each failure is cheap and each gate is a measurement rather than an opinion.

**Stage 0, the plumbing, with no new widgets.** Implement `Backend` over this head's painter, then render the
existing frame unchanged as one `Text` in the frame's area, so the two renderers are identical by construction
and the only variable is the plumbing. The backend's `draw` must not write cells: it applies the incoming cell
updates to a shadow buffer, renders that buffer to rows, and hands the whole frame to the row-diffing painter.
That keeps the diff, the `rows` counter and the silent idle frame.

*Gate.* `WriteStats` identical before and after on a `--replay` of three real sessions (one short, one
turn-heavy, one truncated mid-turn); `tmux capture-pane -p -e` byte-identical at 100x28, 60x20 and 40x12;
`--no-tty` byte-identical. **If this fails, stop** - the failure means the library decides something about the
cursor, the clear or the write order that this head cannot express, and no widget port fixes it.

**Stage 1, the transcript as a custom widget.** The right first widget because it is the largest and most
bespoke, and because its *model* does not move: folding, the rungs, the budget, the sanitiser, ANSI-to-role
painting, provenance marks and the markdown block model are not about drawing. Only the painting half moves.

*Gate.* Frame-for-frame equality at three widths over the same three sessions, with every difference either
eliminated or written down as intended; the transcript's own tests pass unchanged (if one needs changing that
is a finding, not a chore); the escape-vocabulary test passes at the new boundary.

**Stage 2, the panes.** One pane per commit, each with its own frame comparison. The open question is the one
this head already answers: a pane records the screen row each stop was drawn on so a click maps back to a row.
Either the widget reproduces that arithmetic exactly or the pane keeps its record and the widget paints only -
and that is decided by the existing click tests, not by argument.

**Stage 3, the terminal pane.** The byte-stream corpus of §2. Three outcomes are acceptable: the library
matches and is adopted; it differs and the differences are enumerated and judged; or it cannot be restricted to
the roles and the in-tree emulator stays. **The one unacceptable outcome is adopting it without the corpus.**

**What must be true before the old painter can be deleted.** Every `screen()` assertion passes or has been
rewritten with the old expectation recorded as wrong; the row counter still answers its question, either
because the head kept its own backend or because a replacement was calibrated against the old one on the same
replay; `--replay`, `--no-tty` and `Palette::None` produce identical bytes on a second `TERM`; and a day-long
session's byte count is not worse by more than a stated factor.

## 5. What this survey did not verify

It did not compile, build or time anything, so no claim in it about *cost* is a measurement of work. It did not
read the source of any ecosystem crate - only ratatui's own - so every statement about those is from published
metadata. It did not check whether the crossterm backend preserves DEC 2026, bracketed paste, SGR mouse or the
steady block cursor, and it did not read ratatui's `inline` viewport example, the one feature that could
interact with this head's ownership of the alternate screen. Its claim that the fit ladder survives as caller
code is an argument from the API surface, not a measurement.
