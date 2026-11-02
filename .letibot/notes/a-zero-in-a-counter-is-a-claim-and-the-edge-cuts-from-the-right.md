<!-- abstract: A zero in a counter is a claim of absence, and the composer edge truncates from its RIGHT end - so severity leads the standings line, and the words must stay one contiguous span for the click target and the tests. -->

# A zero in a counter is a claim, and the edge cuts from its RIGHT end

`Standings::words()` (`crates/tui/src/ui/panes/queue.rs`) is the one place the composer edge's queue
line is made - `bottom_markers` (`crates/tui/src/ui/screen.rs`) draws it, and nothing else draws a
queue badge. It read `{} in review · {} being merged · {} stuck · {} landed` unconditionally, so a
repository whose rows were all terminal said `0 in review · 0 being merged · 0 stuck · 0 landed`
while its `main` was red. Every number was true and the sentence was idle.

A zero is not the absence of a claim, it is a claim of absence, and it is the one a reader believes
because it is printed where they asked what there is. A counter drawn when it is zero spends their
attention on nothing: a word needs a count.

`box_edge` truncates the right markers from their RIGHT end (`rano`'s `render::text::truncate`,
`room = inner - (left_cols + 3)`), so on a narrow terminal the LAST word is the first one lost. That
is why `red` leads the line and `landed` may go: put severity at the end of a right-truncated edge
and a narrow window eats the alarm first.

The standings' click target is read off the drawn row (`screen::drawn_span`, searching the plain
words), so the words must stay ONE contiguous span. Splitting them into a span per word, to give
`1 red` its own colour, breaks every `screen.contains("... · ...")` assertion in
`app/tests/queue.rs`, because the ANSI bytes then sit between the words. The line keeps one
register: `Standings::role()`, the max of its parts.
