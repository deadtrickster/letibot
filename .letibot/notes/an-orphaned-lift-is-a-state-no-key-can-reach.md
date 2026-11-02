<!-- abstract: The thinking's openness had four carriers, so a door that emptied one made a state no key could reach; it is one field now, and `thinking=folded` at a hiding rung is not a way to close it. -->

# An orphaned lift is a state no key can reach — and four more things the review of it found

"The thinking is open" had four carriers - `App::reasoning`, `visibility.thinking`, and a pair
`run_show` + `live_open` - so a door that emptied one made the state unreachable: `set_verbosity`
cleared `payload_sel`, the panel's only exit, and `ctrl-r` turned a click into an orphan that stayed
drawn and no key could undo.

It is one field now (`App::lift`, read against `payload_sel`) with two writers
(`set_lift`/`drop_lift`, which take the panel with the switch), one reader (`thinking_open`), one
writer (`set_thinking`) and `flip_thinking` as the verb every door calls - all in
`crates/tui/src/app/runclick.rs`.

`thinking=folded` at a bottom rung is `normal`, because `conversation` and `read-edits` hide the
reasoning rows: writing the fold there moves the RUNG and the whole working appears. So the branch
that picks a spelling asks `visibility.hides_the_working()`, not `!shows(Thinking)`.

An open run's marker must stay drawn - the hit record a click is tested against comes from the drawn
frame - and a row is never the live tail, so the per-row walk answers `false` for `LIVE_RUN` while
`App::run_is_open` answers the sentinel itself.
