<!-- abstract: "Collapsed notifications" are R89's one-event-one-row pairings in `App::absorbed`, and three doors dropped the pairing - a retired note, any snapshot, and a second head's starter-todo seed. -->

# A collapsed notice is a pairing, and three things dropped it

"Collapsed" means R89's one-event-one-row pairing, `crates/tui/src/app/one_event.rs`: a session row
saying the same event as a hub warning is absorbed into the warning's one-line card, held in
`App::absorbed`. The code has no collapse or fold word for it, which is why grepping the head finds
nothing.

Three doors dropped it. The reader's `ctrl-n`: `is_absorbed` asked `!dismissed.contains(key)`, and a
retired note gives its rows back, so a key whose whole promise is hide-this put the row back where the
card had been. Any snapshot (`/resync`, a reattach, a switch back): `App::load` cleared the map, and the
pairing is only re-made from a note at a `Placed::Seam`, while a snapshot's warnings are
`Placed::Before`. And a second head's starter-todo seed: `mark_seeded` saves a whole `HeadPrefs`
carrying THIS head's `dismissed`, so a head that retired nothing wrote an empty `retired` over another
head's.

The rule that survived: a row is collapsed while its note's card is on the screen (`Placed::Seam`) or
the reader retired that note. A note held but neither drawn nor retired gives its rows back, because
then the row is the only thing saying the event happened.
