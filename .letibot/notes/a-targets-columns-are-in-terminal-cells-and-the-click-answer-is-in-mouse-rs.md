<!-- abstract: A new click `Target` costs three arms in `app/keys/mouse.rs` and only two of them fail to compile, and hit records are made before the gutter pad so a target's `x` must add it. -->

# A click target's columns, and where the click is actually answered — MEASURED 2026-10-27 (branch `queue-standings-click`, making the merge queue's standings on the composer's bottom edge clickable)

`crates/tui/src/app/pointer.rs` answers hover (`clickable_at`, `relight`) and turns a release into
`Key::Click`; the click itself is dispatched in `crates/tui/src/app/keys/mouse.rs`.

A new `Target` variant costs exactly three arms there, and only two of them fail to compile:
`still_drawn` and `wheel_at` are exhaustive over the enum with no `_`, while `click_on` has a
`(_, None)` catch-all that compiles without the new arm and silently swallows the click - the target
records, the hover lights, and the click does nothing.

Hit records are made before the gutter pad: `compose_screen` pushes every `Hit` while the rows still
have no gutter, and the pad is inserted at the end. A click's `x` is in terminal cells, so a
target's `rect.x` must be `col + gutter` (`record_main_hits` in `crates/tui/src/app/hits.rs` takes
`gutter` for this reason). The header's git field does not, and sits two columns left of what it
drew.

A rectangle read off words must be read off the finished row (`screen::drawn_span`), not computed
from their length: `box_edge` truncates the right-hand line, so arithmetic gives a target for words
the frame dropped. The safe failure is no target.
