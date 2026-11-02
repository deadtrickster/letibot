<!-- abstract: A row that must read as the model's own is restored into the model's half by `by`, so a daemon that writes one has to put its identity on the row - `TodoNeed::Room { item, working }`. -->

# A daemon-written row that must read as *yours* has no half of its own, so it must be self-identifying

`TodoBoard` has four halves and `TodoBoard::new` sorts a restored list into them by `by`, so a row's author is its half. A room's row has to be the session's own, because that is what makes the nag say *yours* and lets the model dispose of it with `todo_write` - so the pass writes into the model's plan and needs a way to take its own rows away.

A map of what it wrote fails at a restart: `Harness::open` rebuilds the board from the store, so a `you hold …` row whose item was finished while the daemon was down sits there for ever.

The identity goes on the row. `TodoNeed::Room { item, working }` is on every room row, and `working` carries the quiet/loud fact that would otherwise have been the edge's presence. Retirement is *a row whose room edge names an item this session no longer holds* - no memory, and it survives a restart.

The cost is that a `needs` entry now means two things, what the row waits on and what the row is; `TodoNeed::Answered` is the precedent for an always-met edge. And a row's words must not carry a clock, because a new `content` is a new row and a five-second nag machine.
