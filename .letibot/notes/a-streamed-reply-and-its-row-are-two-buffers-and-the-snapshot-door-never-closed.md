<!-- abstract: MEASURED 2026-10-30: the duplicate reply was two buffers (items row + TurnPane::text), the copy WITH the counts is the LIVE one, and the live door (record_item) already closed — the open door is App::load, whose snapshot carries TurnView.text beside rows that already hold it, plus settled_calls/reasoned_upto left at zero. -->

# A streamed reply and its stored row are two buffers, and only one door closed

MEASURED 2026-10-30, branch `agent/a-streamed-reply-is-drawn-twice` (`crates/tui`), from the
operator's photograph of a reply drawn twice with their question between the copies.

## The two copies, and which is which

`items` holds the stored `Assistant` row; `TurnPane::text` holds the prose the deltas streamed
into; the frame draws every row and then the pane (`ui::transcript::window`, `push_turn_pane`
last). So **nothing in the item list is duplicated** — a rule about the list cannot see this
defect, and a count of rows is right while the screen is wrong.

**The copy carrying the counts is the LIVE one, not the completed row.** The pane draws its
marker *after* the reply (`ui::transcript::live` — the operator's own ruling, *"all of it after
the reply"*), so `… §20.8 …: [1 tool call, 36 thinking lines]` at the bottom is the pane's. The
copy above the question is the row. The photographed shape reads as the opposite, which is what
sent the first reading of it the wrong way.

## The live door was already closed; the snapshot door was not

`App::record_item` clears `TurnPane::text` when an `Assistant` body lands for a row in
`t.appended` — identity, not text. That covers a head that *watched* the round. A **snapshot**
(`ServerFrame::Hello`, `ServerFrame::Resync` — i.e. re-attach, resync, `leticode --session`,
switch back) is a replacement that carries `TurnView.text` **beside rows that already hold their
bodies**, and the daemon's view never trims `TurnView.text` when a row lands
(`sessionlog/src/view.rs`: the `TranscriptAppended` arm only pushes `appended`). So the head is
handed one paragraph twice and no later event will take the pane's copy away: the body it waits
for already happened, and the next `TurnStarted` is a whole round away. MEASURED in the tui's own
harness: `left: 2, right: 1` for the paragraph before the fix.

## The same door left two more marks at zero

`settled_calls` and `reasoned_upto` are how the pane knows the transcript already holds its
working, and the live door advances both at a body. A snapshot carries neither, so a re-attach
mid-round counted (and on a rung that shows the working, drew) a call and a block of thinking
that were rows above it: MEASURED `[2 tool calls, 2 thinking lines]` for one call and one
thinking line. The fix seeds all three in `App::load`; the tests are
`a_reattach_mid_turn_does_not_draw_the_reply_twice` and
`a_reattach_mid_round_does_not_count_the_working_it_has_rows_for` in `app/tests/scroll.rs`, and
both fail on the tree before it.

## The rake

**A buffer the transcript "takes over" is only taken over at the door you closed it at.** Every
`*_upto`/`settled_*` mark on `TurnPane` is advanced by an EVENT; a snapshot has no events behind
it, so each mark needs asking again in `App::load`. Ask it of the *text* (`a_row_holds`, equality
after trim, only within `appended`) rather than of an id — a snapshot's rows and the pane's
buffers have no shared id to pair on. And do not pair by prefix: clearing on a prefix hides words
no row holds, which is worse than the duplicate.
