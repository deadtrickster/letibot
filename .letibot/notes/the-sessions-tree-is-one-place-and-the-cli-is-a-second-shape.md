<!-- abstract: `App::session_rows()` returns rows and a node's line is not a session, so every reader goes through `SessionRow::idx`; where a row goes is one function and what it says is two copies held by one equality test. -->

# The sessions tree: one place for where a row goes, two copies of what it says — and a second listing in the CLI

`App::session_rows()` draws a project's line, a `system` line, a group's line and a session's
row into one `Vec<SessionRow>`. `SessionRow::idx: Option<usize>` is the index into
`App::sessions`; a row's POSITION is not, and indexing by it panics one past the end, which
reads as a fixture with too few sessions. Every reader goes through `idx`.

`App::placements()` decides `project` (the ROOT of the parent chain - a review clone under a
temp dir is not a project) and `system` (`App::system_kind`), and `sessions_feed()` ships both
to the built-in layer, so the guest never recomputes them. A guest cannot walk a store.

What a row SAYS exists twice, because the layer is the copy a project overrides. One equality
test holds them together
(`sessions_list.rs::the_heads_list_and_the_plugins_list_draw_the_same_rows`) plus two committed
pictures in `app/tests/data/sessions-list-{closed,open}.txt`, re-blessed with `LETIBOT_BLESS=1`:
a change to the tree's shape is a change to those files.

`letibot --sessions` is a second shape - `harnessd::cli::grouped` prints the five
`SessionGroup`s - so one session can sit differently in the two pictures.
