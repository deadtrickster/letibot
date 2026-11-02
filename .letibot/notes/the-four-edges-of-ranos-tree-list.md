<!-- abstract: Four edges of rano's `tree_list` as letibot ported onto it: the picker's cursor sentence must be composed by the head, a tree block in a bigger pane must clear its ids, `Row::detail` is not Enter-prose, and a number is the n-th SESSION. -->

# Four edges of rano's one tree list, found while porting letibot onto it

`SessionPicker::tree()` is private, so `TreeList::hint_line()` is unreachable from the head - the only
route out of a picker is `content(w) -> PaneLines`. A head that wants the cursor's sentence composes it
itself, which letibot does in `row_says` (`crates/tui/src/app/keys/bindings.rs:567`), and the two
spellings can drift until rano exposes it.

`TreeList::selected` is clamped to the stops, so the list cannot say *nothing is selected* - which a tree
block inside a bigger pane needs, since the cursor may be on a finding two panels down. `tree_lines`
(`crates/tui/src/ui/panes/plugin.rs:424`) hands it the rows with the `id`s cleared when the cursor is
outside the block.

rano's `Row::detail: Vec<String>` is the faint line UNDER a row; C3's `Row.detail` is markdown read
whole on Enter, so mapping one to the other draws every plugin row's whole prose beneath it. The port
maps `detail` to nothing and leaves Enter to read it.

A drawn number is the n-th SESSION: a group header takes no number, so `/switch N` means *the n-th
session drawn*, read by `App::session_at_number` (`crates/tui/src/app/session.rs:435`).
