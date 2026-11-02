<!-- abstract: A streamed reply and its stored row are two buffers, and only the live door closed them - a snapshot carries `TurnView.text` beside rows that already hold it and leaves `settled_calls` and `reasoned_upto` at zero. -->

# A streamed reply and its stored row are two buffers, and only one door closed

`items` holds the stored `Assistant` row; `TurnPane::text` holds the prose the deltas streamed into;
the frame draws every row and then the pane. Nothing in the item list is duplicated, so a rule about
the list and a count of rows both see a screen that is wrong. The copy carrying the counts is the LIVE
one - the pane draws its marker after the reply - and the copy above the question is the row.

`App::record_item` clears `TurnPane::text` when an `Assistant` body lands for a row in `t.appended`,
by identity, which covers a head that watched the round. A snapshot (`ServerFrame::Hello`,
`ServerFrame::Resync`: a re-attach, a resync, `leticode --session`, a switch back) carries
`TurnView.text` beside rows that already hold their bodies, and the daemon never trims that field when
a row lands (`crates/sessionlog/src/view.rs`), so nothing later takes the pane's copy away.

`settled_calls` and `reasoned_upto` are the same hole at zero: a re-attach mid-round counts, and on a
rung that shows the working draws, calls and thinking that already have rows above them. `App::load`
(`crates/tui/src/app/events/mod.rs`) seeds all three.

A buffer the transcript takes over is only taken over at the door you closed it at: ask it of the text
(`TurnPane::a_row_holds`, equality after trim, only within `appended`), never by prefix.
