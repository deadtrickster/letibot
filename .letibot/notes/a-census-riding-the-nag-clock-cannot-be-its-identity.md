<!-- abstract: A scheduled notice whose text carries a growing number needs a deadline of its own, and the queue census is a `Door::Census` turn rather than a `note_to_model` row, which a quiet session never reads. -->

# A notice that carries a growing number gets its own deadline

`TodoNagClock::said` decides repeat vs new by comparing the notice's text with `nagged`. A sentence
naming the age of the oldest stuck row changes on every delivery, so in `nagged` every delivery is
new, the ladder resets and an alarm becomes a metronome. So the clock keeps a deadline per subject
(`due`/`census_due`, `nagged_at`/`census_said_at`, `window`/`repeat`) and the text is free to say what
the store says: the text is the payload, never the schedule.

The census is a TURN now - `Door::Census` -> `Sessions::deliver_due_censuses` ->
`Harness::census_turn`. It went through `note_to_model` for eight hours and reached nobody: that is a
mailbox a quiet session never opens. It cannot be a `Door::Nag`, which records against the plan's
ladder. And record the census BEFORE the re-arm, or `census_said_at` is still `None` and every idle
pass runs another model call.

`next_nag_at()` is a minimum over three clocks and the sweep (`SWEEP_AFTER`, 2 s), so it is no probe
for "is the census armed" - read `Sessions::census_due_at(id)`.
