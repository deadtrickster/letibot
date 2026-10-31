<!-- abstract: MEASURED 2026-10-31 building the room's reminder rows: a daemon-written row that must read as `by: yours` cannot live in a half of its own (`TodoBoard::new` sorts by `by`, so a `Model` row is restored into the model's half), so the pass must be able to recognise its own rows from the ROW — a need that is always present, not a map of what it wrote, because a board rebuilt from the store remembers nothing. -->

# A daemon-written row that must read as *yours* has no half of its own, so it must be self-identifying

**MEASURED 2026-10-31**, branch `agent/the-rooms-reminders` (worktree of the same name), building
`docs/room.md`'s *Still owed* item 3: a holder of a room item gets a todo row, and the existing nag
carries it.

## The constraint, in one paragraph

`TodoBoard` has four halves and `TodoBoard::new` sorts a restored list into them **by `by`** — so a
row's author *is* its half. The merge queue's rows are `TodoBy::Queue` and live in a half that is
replaced wholesale every pass, which is what makes *a row leaves the board when its entry leaves the
queue* free. The room's rows cannot do that: the brief is explicit that the row is **the session's
own** (`by: yours`, i.e. `TodoBy::Model`), because that is what makes the nag say *yours* and what
lets the model dispose of it with `todo_write`'s `update`. A `Model` row is restored into the
model's half, so the pass writes *into the model's plan* and needs a way to take its own rows away
again.

## Two shapes that do not work, and the one that does

**A map of what the pass wrote** (`Mutex<BTreeMap<u64, String>>`, item → words) works until a
restart: `Harness::open` rebuilds the board from the store, so the first pass of a new process has
written nothing and remembers nothing — and a `you hold …` row whose item was finished while the
daemon was down sits there for ever. That is the claim-versus-fact shape this tree keeps paying for,
and it was found by writing the test for it, not by reasoning.

**A row with no edge when it is loud** is the same failure one layer down: nothing on the row says
whose it is, so retirement has only the words to go on.

**The shape that works: put the identity ON THE ROW.** `TodoNeed::Room { item, working }` is on
every room row, loud or quiet, and `working` carries the quiet/loud fact that would otherwise have
been the edge's *presence*. Retirement is then *a row whose room edge names an item this session no
longer holds* — no memory, survives a restart, and it is still one door.

The cost is that a `needs` entry now means two things: *what the row waits on* (`working: true`,
unmet, and the check is silent) and *what the row is* (`working: false`, met, and the check names
it). `TodoNeed::Answered` is the precedent for an always-met edge that is a record rather than a
wait, so the enum already allows it — but a reader who assumes `needs` is only a wait will get it
wrong, which is why the variant's doc says so in its first paragraph.

## The rake

**A row's identity on this board is its exact `content`, and a daemon that writes rows into somebody
else's half has to give them a second identity the board can see.** The alternatives are a memory
that a restart erases, or a second half that `by` would then lie about.

## Two smaller things from the same change

* **`Sessions::next_idle_at` must not report a clock that has no work behind it.** The room's pass
  runs on the daemon's idle tick at five seconds; arming that clock unconditionally would wake every
  daemon on every box twelve times a minute for a question only a box with a coordinator can answer.
  It is reported only when `Sessions::has_a_room()` — and the clock itself stays armed, so a session
  that opens with a room is due at once rather than after a window.
* **A row's words must not carry a clock.** `… for 4m` is a new `content` every pass, and a new
  `content` is a new row: a five-second nag machine. Everything that moves goes in the edge or the
  status.
