<!-- abstract: MEASURED 2026-10-27: a new `Target` variant costs three arms in `app/keys/mouse.rs` (the only click dispatcher — pointer.rs has none), and hit records are made BEFORE the gutter pad, so a target's columns must add `gutter` themselves — the git field's record does not, and sits 2 columns left of what it drew. -->

# A click target's columns, and where the click is actually answered — MEASURED 2026-10-27 (branch `queue-standings-click`, making the merge queue's standings on the composer's bottom edge clickable)

## The click answer is not in `pointer.rs`, whatever the brief says

`crates/tui/src/app/pointer.rs` answers **hover** (`clickable_at`, `relight`) and turns a
release into `Key::Click`. The click itself is dispatched in **`crates/tui/src/app/keys/mouse.rs`**:
`key_click` → `still_drawn` → `click_on`, plus `wheel_at` for a notch. So a brief that says
"answer it in `pointer.rs`" cannot be obeyed, and a brief that makes `app/keys/**` off limits
cannot be obeyed either:

**A new `Target` variant costs exactly three arms in `mouse.rs`**, because all three matches are
exhaustive over the enum with no `_`:
- `still_drawn` — will not compile without it;
- `wheel_at` — will not compile without it;
- `click_on` — compiles without it (there is a `(_, None)` catch-all) and silently swallows the
  click, which is the dangerous one: the target records, the hover lights, and the click does
  nothing.

Nothing else in the tree matches `Target` exhaustively (the plugin panes have their own
`Target` type; `hits.rs` only constructs).

## Hit records are made before the gutter pad, so a target's `x` must add `gutter`

`compose_screen` pushes every `Hit` while `out`'s rows still have no gutter — the `" ".repeat(gutter)`
pad is inserted into each row at the very end, in the same pass as the trim. A click's `x` is in
**terminal cells**, so `rect.x` must be `col + gutter` (`record_main_hits` takes `gutter` for
this reason; `box_top_hits` adds it at the record site).

**The git field forgets it, and is measurably two columns left of what it drew.** Measured on a
140-column frame with the field drawn at column 61:

```
MEASURED rect.x=59 drawn_col=61 w=6
```

(`(main)` is 6 wide, so the click in `app/tests/repos.rs` — aimed at `drawn + 1` — lands inside
anyway and the test stays green.) Left alone: it is not this package's file, and the fix is one
`+ gutter`.

## The one that reads like arithmetic and must not be

The standings' rectangle is read off the **finished row** (`screen::drawn_span`: strip escapes
with `app::keyhints::cells`, find the words, take `visible_width` of the prefix) and not computed
from the words' length, because `rano::agent::composer::box_edge` truncates the right-hand line to
`inner - (left_cols + 3)` — the standings are the last span, so a narrow terminal cuts them first.
The safe failure is *no target*; arithmetic gives a target for words the frame dropped. Same rule
as `box_top_label` for the count labels, and worth a test in both directions (a 30-column frame
records nothing; a frame that draws them records their exact columns).
