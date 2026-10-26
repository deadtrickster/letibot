<!-- abstract: MEASURED 2026-10-26 (P1): a plugin pane row a click cannot act on never reaches `hovered`, because `clickable_at` only answers for hit targets; and the example dashboard's legend documents denominators and role words, not line labels, so the new hover answers on nothing in the one real plugin. -->

# The legend on hover: why it reads `pointer_at`, and what it cannot explain yet

MEASURED 2026-10-26, branch `plugins-followups`, wiring C3's *"the same text answers on hover"*
into `crates/tui` (`app/plugin.rs`, `app/hits.rs`, `ui/hint_bar.rs`, `ui/panes/plugin.rs`).

## A row a click would not act on has no `hovered` entry, and that is the row the reader points at

`App::relight` → `clickable_at` answers *what a click here would act on*, and for a pane it is
`stop_block`, which walks **the stop rows** a frame recorded. A `Line` with no `id` is not a stop,
so a row above the first stop — the pane's title, the findings summary line, the first line of a
panel whose rows carry no ids — is inside no block and gets `hovered = None`. So the legend text is
read from `App::pointer_at` (the pointer's position, always kept) against
`App::plugin_legend_at`, and **not** from `hovered`. The alternative — adding a `Hit` per legend row
— would have given every one of them a hand pointer, i.e. *clicking this does something*, and a
legend row is documentation.

`plugin_legend_at` is the same record in screen rows that `plugin_panel_rows` is: the draw writes
pane rows (`Content::legend`), `App::record_main_hits` translates them with `top` and
`pane_scroll`, and `hint_bar` prepends the words to its tail.

## One frame of lag, accepted, and it is the click map's own

`compose_screen` builds the chrome — so `hint_bar` — **before** `main_area`. A hover text read from
a draw-time record is therefore the previous frame's record with this frame's pointer. That is the
same lag `relight` already has (its `hits` are last frame's), and it only shows when the layout
moved in the same frame the pointer did (a scroll under a still pointer).

## The example plugin's legend does not document its line labels

`plugins/examples/plan-dashboard/src/lib.rs`'s `legend()` keys are **`done/total`** (a value
column), **role words** (`attention`, `success`, …) and **each panel's denominator**. None of them
is a `Line::label` or a `Row::cells[0]`, which is what the hover matches on (exact, after
`clean_line`, because that is the text the drawing measured). So on the one real dashboard, C3's
"same text answers on hover" still answers nothing — the seam works and the example does not feed
it. Either the example documents its two tier labels, or the hover also answers for a panel's
denominator on its title row; both are contract-level decisions, and neither was this package's.
