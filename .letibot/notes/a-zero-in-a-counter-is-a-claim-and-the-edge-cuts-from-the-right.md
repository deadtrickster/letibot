<!-- abstract: MEASURED 2026-10-31: the composer edge's standings printed four zeros under the one fact that mattered (a red target); a word now needs a count, and the edge's truncation direction is what decides which word leads. -->

# A zero in a counter is a claim, and the edge cuts from its RIGHT end

MEASURED 2026-10-31, branch `agent/a-red-target-that-says-nothing` (`crates/tui`), from the
operator's *"main is red, tests are going — nothing shows as in review or merged or stuck"*.

## The shape

`Standings::words()` (`crates/tui/src/ui/panes/queue.rs`) is the ONE place the composer edge's
queue line is made — `bottom_markers` (`crates/tui/src/ui/screen.rs`) draws it, and nothing else
draws a queue badge, so "the line" and "the badge" are the same span. It read
`{} in review · {} being merged · {} stuck · {} landed` unconditionally, with the target alarms
prepended. A repository whose rows are all terminal therefore said `0 in review · 0 being merged ·
0 stuck · 0 landed` four times while its `main` was red. Every number was true and the sentence
was *idle*.

**A zero is not the absence of a claim, it is a claim of absence** — and it is the one a reader
believes, because it is printed where they asked what there is. A counter that is drawn when it
is zero spends the reader's attention on nothing.

## What the fix rests on, and would be wrong without

**`box_edge` truncates the right markers from their RIGHT end** (`rano`'s
`render::text::truncate`, `room = inner - (left_cols + 3)`), so on a narrow terminal the LAST
word is the first one lost. That is why `red` leads the line: the emergency is the word that
must survive a 60-column terminal, and `landed` is the one that may go. Put severity at the end
of a right-truncated edge and a narrow window eats the alarm first.

**The longest line is 70 cells and the edge is 76 at 80 columns** (`term_w - 2 * App::GUTTER`,
minus the two corners) — it fits with one cell of fill, so 80 columns is the exact width at
which the whole six-word line is still drawn and still clickable. Two-digit counts overflow it.

## Rake for the next change to this line

The standings' click target is read off the drawn row (`screen::drawn_span`, searching the plain
words), so the words must stay ONE contiguous span. Splitting them into a span per word (to give
`1 red` its own colour) breaks every `screen.contains("… · …")` assertion in
`app/tests/queue.rs`, because the ANSI bytes then sit between the words. The line keeps one
register — `Standings::role()`, the max of its parts.
