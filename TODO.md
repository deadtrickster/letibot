# TODO

Work queue. **The first section is everything that can be finished without the
operator**; nothing in it waits on a decision, and each entry says how to check it
is still open before starting.

Settled work is in `TODO-settled.md` and is not repeated here.

## If you are an agent picking work from this file

1. **Take one item from READY. Nothing else.** Sections 2 and 3 are blocked on a
   human; starting one produces work that cannot land.
2. **Re-run the item's `still open?` check first.** Statuses go stale. On 2026-09-10
   three items in this file — T12, T20.4, T21.1/2 — were found already implemented
   while still filed as open, and would have been re-done from scratch.
3. **Verify with the item's `done when`, not by reading your own diff.** If a check
   needs a live model, `~/bin/model status` says what is serving; several test files
   require it and will otherwise pass vacuously.
4. **If you find it already done**, move it to `TODO-settled.md` with the evidence
   and stop. That is a complete, valuable outcome.
5. **If it turns out to need a decision**, move it to section 3 with the question
   stated. Do not guess the answer.

## Repo facts you will need

    cargo check --workspace          # clean as of 2026-09-10
    cargo test -p <crate>            # crates/: transcript turn dialect{,-glm,-qwen}
                                     #   tools harnessd sessionlog tui ui code
                                     #   backend tokencore
    python3 tests/fidelity/run_gate.py    # template fidelity gate, must print GATE PASS

Tests needing a live model server on 127.0.0.1:8080: `sessionlog/{live_e2e,late_head,
sessions,resume_frames}`, `harnessd/{loop_closes,wired}`, `turn/{live_qwen,
engine_decisions}`, `tools/{exec,background,confine}`.

---

# 1. READY — no operator needed

*Verified open by inspection on 2026-09-10; each says how to re-check.*

---

## A stop unlinks the socket without the daemon going, so the next start adds a SECOND daemon to one folder — **OPEN, diagnosed on the operator's box 2026-10-04**

**The symptom the operator met:** a session that could not be written to at all. Every append refused:

    s-1789462738453908838 · dead -> store: row 483 (s-1789462738453908838#t45.483):
        sqlite: transcript_item seq must be the next one

Not the context wall — the wall was long past. The head delivered, the daemon tried to record the row,
and sqlite refused, so no turn could start. The session was bricked: nothing could be said to it,
because saying anything is a write.

**The store is right and the trigger is doing its job.** `tokencore/src/store.rs:528`,
`transcript_item_append_only_insert`: *"A hole, an overlap, a reordering or a re-insert at an old
index is refused by the database."* MEASURED against the store: `#t45` held **510 rows, seq 0–509**,
so the next seq is 510 — and the daemon was offering **483**, twenty-seven behind. A re-insert at an
old index, refused exactly as designed. Without that trigger two daemons would have interleaved rows
into one transcript and the hash chain would have stopped meaning anything silently.

**THE CAUSE IS THREE DAEMONS ON ONE FOLDER.** Measured:

    2124394  21:27  ~/Projects/letibot
    2210747  22:26  ~/Projects/letibot
    2264222  23:09  ~/Projects/letibot     <- the head's

and two of them were **LISTENING ON THE SAME PATH WHILE THE FILE DID NOT EXIST**:

    ss -xlp:  /run/user/1000/letibot/42ce9f1aae08.sock  harnessd pid=2210747 fd=11  LISTEN
              /run/user/1000/letibot/42ce9f1aae08.sock  harnessd pid=2264222 fd=11  LISTEN
    ls:       cannot access that path: No such file or directory

**The guard is sound and is keyed on the wrong thing.** `server.rs:176` connects to the existing
socket first and refuses with *"already served by a live daemon"* when it answers, unlinking only
when it does not. Correct — **but `remove_file` also lives on `ServerHandle::drop` (`:106`, `:114`)**,
and `scripts/letibot`'s connect-or-start tests `[ -S "$SOCKET" ]`. So:

  1. the operator stops the daemon; the handle drops and the socket FILE is unlinked;
  2. the process does not actually go — the state R30 already names, *"the daemon was asked to stop
     and had not gone"* — and keeps listening on a now-nameless inode, holding the session's ledger;
  3. the next start finds no file, probes nothing, and legitimately starts a second daemon;
  4. the older one keeps appending. Its rows land; the newer one's ledger is stale; the trigger
     refuses every write the head makes.

Confirmed on the way out: `2210747` went on TERM, **`2124394` ignored TERM for fourteen seconds and
needed KILL** — the same not-going behaviour, from the other side.

**Liveness is being inferred from a filename, and an unlinked socket is indistinguishable from a dead
daemon.** That is the *absent means gone* assumption this tree has been bitten by repeatedly. The
answer is already on the fabric, in a note from 2026-08-29: *"Single-instance guards: pidfile is
defeatable, pgrep matches its own launcher, **use flock**."* A held lock dies with the process and
cannot be removed by a shutdown path, which is the property the socket file does not have.

**still open?** start a daemon for a folder, stop it so the file is unlinked, confirm the process
survives, then start again and count daemons for that folder: `ss -xlp | grep <workspace>.sock`.
Two LISTEN rows on one path is the defect.

**done when** a second daemon for a folder that already has a live one is refused by name, with the
refusal naming the live pid, and the refusal does not depend on the socket file existing.

---

## The `reasoning_content` refusal is a TRAILING SYSTEM MESSAGE plus `tools` — **OPEN, isolated by the relay 2026-10-04**

Two days of compaction refusals on `deepseek-flash`, all of this form:

    http 400: The `reasoning_content` in the thinking mode must be passed back to the API.

**It has nothing to do with reasoning.** Measured against the live endpoint, three messages:

    [user, assistant("hello"), system("Summarise.")]   WITHOUT tools  ->  200
    [user, assistant("hello"), system("Summarise.")]   WITH tools     ->  400
    [user, assistant("hello"), user("Summarise.")]     WITH tools     ->  200

Confirmed at scale: 40 exchanges with mixed `reasoning_content`, identical except the final
role — `user` 200, `system` 400. **The trigger is a trailing `system` message while `tools`
is present**, and the error text names the wrong field.

**Why every earlier theory failed.** Size was measured away correctly (976,097 -> 200;
1,248,039 -> an honest length complaint). Structure was measured away by nine permutations —
counts 1/5/20/100/309 of tool-calling assistants without `reasoning_content`, interleaved,
blocked, and empty-string — **all of which ended with a user message**, which is why they all
returned 200. The position nobody probed was the last one.

**It is ours, at `compaction.rs:446`:** the summary instruction is appended as
`TranscriptItem::System { origin: SystemOrigin::Update }`, deliberately — `:416` says *"rather
than a user message: it is appended after the cached history instead of rewriting anything"* —
and `messages::convert` renders that as a trailing `{"role":"system"}`. The summary turn carries
tools again since `6d73ba6`. That is the whole mechanism, and it explains why ONLY compaction
fails, why ordinary turns never do, and why it survived the tools revert.

**The fix belongs in `messages::convert`, not in the transcript item.** `SystemOrigin::Update`
is correct as a record and the local path reuses the prefix because of it; changing the stored
item would make the transcript lie to work around one provider's parser. `convert` is already
the layer that translates the record into a provider's dialect and already takes a per-request
property (`echo_reasoning`). **Do not special-case DeepSeek by name** — this tree has twice been
bitten by provider rules taken from documentation; gate on the transport if a gate is wanted.

**still open?** `grep -n "SystemOrigin::Update" crates/turn/src/compaction.rs` and check whether
`messages::convert` translates a trailing `System` item for the messages transport.

**done when** building messages for a transcript that ends in a `System` item, with tools
present, produces a last message whose role is not `system` — asserted in a test, because the
three-message repro costs nothing and would have caught this the day `SystemOrigin::Update`
was introduced.

---

## A fold that SUCCEEDS is thrown away by the store — **OPEN, measured on the operator's own session 2026-10-04**

On `glm-coding/glm-5.3`, so the item above does not apply:

    · compact_half — compacting half 1 of 1: the conversation so far — 292 item(s), 568330 token(s) to read
    · compact_half — half 1 of 1 answered: 6146 chars written.
    ! auto_compact_failed — store: row 594 (s-1789462738453908838#t44.594):
        sqlite: transcript_item seq must be the next one.

**The model did the work and the write refused it.** 292 items and 568,330 tokens were read, a
6,146-character summary exists, and it is discarded — so every retry pays for the summary again.
This is the more expensive of the two failures: the provider one fails before spending anything.

**Not diagnosed, and deliberately not guessed at.** The question to start from is whether `#t44`'s
first row is numbered from the PARENT's sequence rather than the fork's — row 594 arriving where
the fork expects its own next is the shape a fork-numbering bug takes, and it is cheap to check
against the store.

**still open?** reproduce a fold on a session large enough to overrun, or read `#t44`'s rows out
of `sessions.db` and compare the first `seq` against the parent's last.

**done when** a successful fold's summary is persisted, and a sequence violation on the fork's
first row is a named refusal rather than a discarded answer.

---

## R55 — a round's end and a turn's end are the same event on the wire — **OPEN, reported from leticl through the relay 2026-10-03**

The operator, watching leticl's composer edge during a multi-round turn: *"sometimes 'responding'
timers flickers to Responded and then back, without time reset. not harmful but annoying"*, and then
*"so it looks like the harnessd must be explicit here and add another real end marker"*.

The timer does not reset because you measured the turn once: `harnessd/src/harness.rs:4339`
`begin_turn_clock` — *"Called once per prompt, never per round"* — sets `turn_began_ms` and the
engine's copy, and every round's `TurnStarted` carries that same stamp. Good, and it is the half that
works. The status word toggles because the **ending** is not marked at all: `engine.rs:1318-1320`

```rust
let finish_reason = match done.finish {
    Finish::Stop | Finish::ToolCalls | Finish::Other(_) => FinishReason::Eos,
```

so a round that ended to call tools publishes a `TurnFinished { finish_reason: Eos }` that is
byte-identical to a real ending — which your own comment at `harness.rs:5800` already says out loud:
*"Per round, the same way the heads' own `TurnFinished` updates theirs."* A head therefore believes
the turn is over in the window between a round's finish and its calls being proposed (nothing is
generating and no call is unfinished yet), writes its `Responded in …` report, and the proposal flips
it back. On leticl's screen: `Responding → Responded → Responding` on every round boundary, with the
duration untouched — measured on the operator's own head, `Responded in 4m09s at 21:37` drawn while
the turn was still running.

**The ask: mark the real end.** The minimal shape, and the operator's own framing — *another real
end marker*: a `final: bool` on the existing `TurnFinished` (or a `round: u32` beside it if naming the
round is wanted). The metrics stay per round, where the heads read them; only the ending gains a name.
An absent field should mean **final**, i.e. today's behaviour, so an old head against a new daemon
keeps working and a new head against a recent daemon flickers but never lies.

**Both heads then answer it with one predicate each**: while a turn is in flight the row says
`Responding`, including that window; the report is written only on `final`, and cleared at the next
`TurnStarted` as it is today. leticl's half is one line in its `:turn-finished` arm — it is ready to
land the day the field exists, and its suite builds the event by hand, so it is testable before this
ships.

**What letibot found when it read the three sites — and why the shape is an EVENT, not a field.**
All three citations hold verbatim: `begin_turn_clock`'s *"Called once per prompt, never per round"*,
the `Finish::Stop | Finish::ToolCalls | Finish::Other(_) => Eos` fold, and — the one leticl could not
read — `harness.rs:5800`, which is **support rather than a contradiction**: it says *"Per round, the
same way the heads' own `TurnFinished` updates theirs"* in the middle of explaining why the SESSION
row's prompt size is per round, i.e. it records this as deliberate.

**The engine cannot set `final`, and that is the whole of it.** `Finish::ToolCalls` establishes only
that a continuation MAY follow; whether one does is decided one layer up, in this daemon's round loop
— `room_for_next_turn` and then `run_continuation` (`harnessd/src/sessions.rs`) — and the convergence
point that already knows a prompt is over is the one where `end_turn_clock()` is called
(`sessions.rs:1090`, whose own comment is *"Every turn ends here"*). A `final: false` written at the
engine's fold would therefore be a **guess**, and on the paths where the guess is wrong — the room
check says no, an interrupt lands while the calls are still running, `Finish::ToolCalls` on a turn
that then fails — the head is left saying *Responding* with nothing running at all. That trades the
flicker for a **stuck row**, and a stuck row is the worse defect: R13 exists because a reader who
cannot tell working from wedged is the confusion this row is for.

So the marker is published where the decision is made, and that means **a new variant — `TurnEnded`.**
Re-publishing the round's `TurnFinished` at the convergence point is the alternative and it
double-counts `usage` in any head that accumulates the turn's cost. A field would also have to carry
the right default (`absent` = final, i.e. not `#[serde(default)]`'s `false` for a positive name), which
is the smaller of the two problems.

**CORRECTED 2026-10-03, an hour after the paragraph above: the variant does NOT need the protocol
bump, because this tree already has the mechanism.** [`PROTOCOL_VERSION`]'s version-27 note is that a
new VARIANT forces a bump *because a head has no arm for it* — *"`serde` has no catch-all on this
enum — deliberately, so a head cannot silently skip a fact it does not understand"* — and `Caps`
carries, in its own words, *"**free-form feature names, for forward compatibility**"*
(`protocol.rs`: `features: Vec<String>`, currently unused and evidently waiting for its first
customer). The hub **already gates delivery per head on those caps** — `hub.rs:931` filters by
`can_decide` when routing a decision and `hub.rs:1425` bounds a head by `caps.queue` — so:

  · the head declares a feature name in its `Caps` on attach (this head would declare it, and leticl's
    half is one string where its attach already is);
  · the daemon sends `TurnEnded` **only to a head that declared it**, so no head ever receives a frame
    it cannot read and the reason the bump existed is dissolved rather than dodged;
  · and `Hello` **echoes the features the daemon honours** (a new field with `#[serde(default)]`, for
    which the rule is genuinely *no bump*), because otherwise a head cannot tell *this daemon ignores
    my feature* from *no turn has ended yet* — and guessing that right is the difference between the
    flicker and a row stuck on for ever.

So the bump is not needed after all, and the operator's deliberate pin of leticl to `v0.2.0` (protocol
27) is left intact: an old head gets today's behaviour, a new head gets the marker, and neither ever
sees a frame it has no arm for. If the operator would rather have the coarse signpost — one number
that says the two halves are skewed — then bump and the argument above is the fallback rather than the
mistake; say which in the commit, since the two routes cost leticl different things (a declared
feature, or a re-pin).

**MEASURED 2026-10-03, before any code: the gate the paragraph above rests on does not exist.** The
whole no-bump argument turns on *“the daemon sends `TurnEnded` **only to a head that declared it**”*,
and this tree has no way to send a frame to one head. `crates/sessionlog/src/hub.rs`'s entire public
surface offers exactly two routes to a head: `publish` (an `Envelope` in the log, read by every head in
`seq` order through `next_batch`) and `submit` (a `ServerFrame` **returned to the caller**, which writes
it back to the one head that asked). `deciding_heads()` is the nearest thing to a per-head gate and is
not one — it is a *query* the adjudicator makes before posting — and `Caps.features` today is
**advisory**: `FEATURE_QUESTION_ANSWERS`’ own doc says a head that does not advertise it *“can still be
sent a question”* and will simply not answer. Nothing in the tree filters a frame by a declared
feature.

So the three routes, with what each actually costs:

 · **A per-head directed frame.** New machinery in the connection layer: either the daemon holds every
   head’s writer, or an outbound queue per head that the accept loop drains. It dissolves the bump
   exactly as the paragraph above intends, and it touches the most delicate layer in the daemon — the
   one where a head that is not written to does not hear that its turn ended.
 · **A `SessionEvent::TurnEnded` and a bump to 28.** The simplest thing that works: one variant, every
   head reads it in `seq` order, a late head replays it correctly, and an old head is refused at the
   handshake rather than handed a frame it has no arm for. The cost is leticl’s — a re-pin, and the
   operator deliberately pinned leticl to 27 — which is a *sentence on the other head*, not a code
   change here.
 · **Zero-bump by reuse, and rejected twice for two different reasons.** Re-publishing the round’s
   `TurnFinished` at the convergence point double-charges every head that accumulates `usage`, and an
   old head over-billing is worse than a flicker. And a `Warning { code: "turn_ended" }` is an arm
   every head already has — so it works, and **a head that does not know the code DRAWS it**: one dim
   row per turn boundary, in the conversation the `model_slow_first_byte` ruling has just finished
   moving weather *out* of.

**Which route is the operator’s to pick**, which the paragraph above already says in the same words
(*“say which in the commit, since the two routes cost leticl different things (a declared feature, or a
re-pin)”*). **The fail-first half needs no ruling and is unaffected by the choice**: this head’s row and
the test that currently pins the flicker.

**And the convergence point is now verified rather than quoted** — both routes depend on it, and one of
its edges fails in exactly the way both are trying to prevent. `end_turn_clock` has **one** caller:
`sessions.rs:1090`, inside `after_turn`. `run_prompt` ends by calling `after_turn` unconditionally
(line 974 passes the `out` **Result**, so a failure is not a bypass), `submit` is the only way in, and
the wall-continuation loop (`1057`–`1085`, `WALL_CONTINUES`) runs **before** the clock stops — so a
marker published there lands after every continuation rather than before it. Every ending therefore
reaches it: an `eos` finish, a `length` cut, a failure, an interrupt, and each wall continuation.

**The one caveat, and it is where the publish must not go.** `end_turn_clock()` sits inside
`if let Some(h) = self.open.get_mut(session_id)` — so a session that left `open` between the round loop
and this tail skips it. That *should* mean *no heads left to tell*; the marker's publisher belongs
**outside** that guard anyway, because a head still attached to a session the daemon has closed would
otherwise sit on a stuck row, which is the failure mode this whole row exists to avoid.

**What THIS head does today, so the operator knows what to look for here.** letibot has no past tense
at all — `Responding` is the only word the row has (`app.rs:13718`), so it cannot flicker to
*Responded* — and it **drops the row** for the same window instead: the last call of a round finishing
and the next round's first delta, because `turn_busy()` is `generating || a call unfinished`
(`app.rs:8185`) and every call of the finished round is `Finished` by then. **And this tree currently
asserts that as correct**: `the_status_row_survives_a_tool_call` pins *"the call lands and nothing
else is outstanding, so the work is over and the row stands down"*. The fix therefore REWRITES that
assertion rather than adding beside it, and that is the fail-first evidence — available before any
daemon change, since it is one predicate and one test. The backstop that makes this *annoying rather
than harmful* on this head is `a_turn_that_has_gone_quiet_says_so_rather_than_spinning`: forty silent
seconds and the row says something again.

**still open?** `grep -n "final" crates/sessionlog/src/event.rs` around the `TurnFinished` variant
shows no such field, and `crates/turn/src/engine.rs:1320` still folds `Finish::ToolCalls` into `Eos`.
And the gate the no-bump route needs is *absent rather than unfinished*: `grep -n "pub fn "
crates/sessionlog/src/hub.rs` lists no directed send, so that is the piece to build if route one is
chosen.

**done when** the daemon publishes the end at the convergence point (where `end_turn_clock` is
called) by whichever route the operator picks above — a filtered `TurnEnded`, or a bumped version
whose `Hello` echo says what is honoured — so that **no head is ever handed a frame it has no arm for**;
and a two-round stream on each head shows the status row up across every round boundary and down only at
the marked end — asserted, not watched: a test per head, and on this head that test replaces the one
that currently pins the flicker.

## R56 — the view can be held, so a selection survives a streaming turn — **BOTH HEADS HAVE IT (`c9634bc` leticl, `2b49304` here)**

The operator, on losing a text selection while a turn streams: *"leticl resets selection if screen
wasnt scrolled too. so both should not do it if anything selected. whether it means stopping render and
showing me 'new content' marker - likely."*

**Two measurements decide the design, and the first kills the obvious fix.** `tmux pipe-pane` on both
heads: leticl erased NOTHING in a twenty-two minute turn (no `ESC[2J`, no `ESC[K`) and still lost the
selection — **any write into a selected cell clears it**, so erasing is not the trigger and there is no
gentler way to paint. And the head **cannot detect the selection at all**: with mouse reporting on,
holding Shift tells the TERMINAL to select and not to forward the events, which is exactly why Shift is
the gesture. So *do not repaint while something is selected* is not implementable as written; the
reader is the only party who knows, so the reader holds the view.

**The contract, and it is all of it: while held, the head writes NOTHING.** Not a spinner, not a clock,
not a counter that ticks. One written cell is one lost selection. The events keep arriving and the head
keeps folding them — it simply stops drawing — so this needs no protocol change at all. Three pieces on
leticl's side, each in the one place that owns it: `paint-wanted-p` (the loop's only gate on painting —
a held view outranks both an event and the clock, and the loop still SLEEPS on a refusal, or a held
screen would spin a core), `*frozen-frame*` (the marker cannot be drawn without a write and cannot be
drawn while nothing writes, so the freeze owes exactly one frame, spent by the paint after the bytes
are out), and `frozen-lines` (the row itself).

**The chord is `ctrl-p`, and it is AGREED rather than picked** — the operator's first choice was
`ctrl-f`, which is taken by the composer's emacs motions (*ctrl-f goes right*, and the reference's
decoder binds it too); `ctrl-p` is free because the todos pane gave it up when that moved to `ctrl-t`,
and *pause* is the better mnemonic. An operator who learns it on one head will reach for it on the
other, so it must be the same key and the same words on both.

**The wording, verbatim, so the two heads cannot say it two ways:**

    ⏸ the view is held — ctrl-p follows again

and on the release, one note: **`the view follows again — N rows arrived while it was held`**, counted
ONCE at that moment, because a live count while held would be an animation and an animation is writes.

`c9634bc` is the whole of it on leticl's side, with the probe output in the commit message.

**THE CHORD DOES NOT TRANSFER, and this is the one thing to settle before any code.** `ctrl-p` is
free on leticl *because its todos pane gave the key up and moved to `ctrl-t`* — and **here `ctrl-p` IS
the todos pane** (`term.rs:781` decodes `0x10`, and the bar reads `ctrl-p todos`). `ctrl-t` cannot be
the destination here either: this head already spends it on the payload window (`ctrl-t newest
result · /t all tool rows`, which the operator reads off the bar and which R10 narrowed to that
meaning on purpose). So on this side the freeze can only have `ctrl-p` if the TODOS PANE MOVES, and
the decoder's own comment says what is left: *"`ctrl-v` (`0x16`) and `0x1c`-`0x1e` are the only bytes
left in this table with no arm"*. None of the four is mnemonic for *plan*, and none is mnemonic for
*pause* either — `ctrl-p` is the only letter that names the new act, which is why the conflict is on
the pane rather than on the hold.

Two ways out, both honest, and they are the operator's to pick:

  · **mirror leticl: todos → `ctrl-t`, and the payload window → `ctrl-v`** ("view"). Best parity —
    the two heads then agree on the freeze AND on the pane — and it moves a key in daily use, with the
    bar entry and its tests to match.
  · **todos → `ctrl-v`, keeping `ctrl-t` as the payload window.** Nothing the operator uses today
    changes, one pane key stays divergent (which it already is: jobs is `ctrl-q` here and `ctrl-j`
    there, and nobody has minded).

**The operator's answer, 2026-10-03: *"yes leticl reworked hotkeys"*** — read as the FIRST route,
because that is the only reading in which leticl's rework is the reason rather than the trivia: leticl
has already paid for the rework, so this head converges with it. todos → `ctrl-t`, the payload window →
`ctrl-v` ("view", and free — `term.rs`'s own comment lists `0x16` as unclaimed), and `ctrl-p` is the
hold's. The words and the marker sentence are unchanged from the agreement above. If that reading is
wrong, one word stops it — and it is worth the one word, because the cost of being wrong here is the
reader's muscle memory rather than a compile error.

**still open?** No — `2b49304` is the whole of it here: `App::toggle_hold` on `ctrl-p`,
`App::screen`'s one-frame freeze with the marker on it, `App::take_redraw`'s refusal, and
the panes moved to make room (`todos` → `ctrl-t`, the payload window → `ctrl-v`). The route
taken is the FIRST one the chord section above offers — the mirror of leticl — which is what
the operator's *"yes leticl reworked hotkeys"* was read as, and it is recorded as a reading
rather than a ruling because being wrong costs muscle memory and no compiler catches it.
`a_held_view_draws_the_same_frame_while_the_turn_streams` drives all three clauses.

**done when** a multi-round turn on their head writes nothing between the freeze and the release (the
byte-level assertion leticl's suite makes), the marker appears exactly once, and the release says how
much arrived.

## Which side owns WEATHER — a classification both heads must answer the same way — **OPEN, question to settle between the heads**

The operator ruled: *"both heads should not emit it inside conversation"* for `model_slow_first_byte` —
the diagnostic is wanted, its PLACEMENT was wrong, so it leaves the transcript, lights the `⚠` and
`/notes` still lists the sentence. leticl landed its half in `973d58f`, and the shape it took is worth
copying or overruling deliberately rather than by accident:

  · the test is **not** *is this routine* but **is this an event in the record**. `+weather-warnings+` is
    `model_slow_first_byte` and `model_endpoint_retry`; `auto_compact` and `compacted` are routine too
    and STAY ROWS, because a compaction changes the conversation; `prefix_check_skipped` stays a row
    because a skipped check is *said, never counted as a pass*;
  · it is a **subset of the routine register** (`warning.rs`'s `Class::Routine`), asserted by a test
    (`821d218`) so a code cannot be silenced here alone.

**The question: where does WEATHER live?** leticl's list is head-local and yours is the daemon's
register, so the two heads now hold two answers to one question — and a code added to one and not the
other is two heads disagreeing about whether a sentence is an event, which is the two-lists defect the
archive's file set was made un-driftable for in `018fc45`.

Two shapes, and the second needs a reason written down: **the daemon classifies weather beside
`Class::Routine`** (one answer, both heads read it, neither can drift — leticl's preference, because it
is what the register already does), or the lists stay head-local with the reason they must exist
separately stated in both trees.

**done when** a code added on one side cannot silently change a head's placement without the other
side seeing it — one list, or two with a stated reason and a guard that reads the other.

## R57 — a child's completion is not a firing, so the wake never happens — **measured from leticl 2026-10-03; explained below, and the last hop is STILL UNTESTED**

The operator restarted the session that spawns `task` children so that this could be tested rather
than assumed, and the measurement is:

  · the child ran and answered — two facts off a fixture, no decisions in it;
  · **the head's agent counter flashed 1 → 0**, so the child's `opening`/`running`/`done` events DID
    reach a head and were folded. That half works;
  · **no notice row appeared** — not in the head, not in the model's own conversation;
  · `job_list` in the session that spawned it: **0 monitors, 0 fired, 0 jobs**. Nothing was armed, so
    nothing could fire;
  · the model learned the child had finished **only by polling** (`task_result`) — the sentence in the
    daemon's own banner: *"a fired monitor reaches the model only when something calls job_list. That
    is a poll, not a wake."*

The mechanism is not missing. `completion_notice` (`harness.rs:664`) publishes *"[job] a job you
backgrounded has ended"*, those notices arrive unprompted, and one arrived in leticl's transcript
today. Its signature is `fn completion_notice(done: &[JobCompletion])` — **jobs only, no children.**
D26 says the wake exists (*"T24's wake turns a firing into a user item and runs a turn"*), so the gap
is narrower than *no wake*: **a child's finish is not a firing.**

**done when** a `task` child's completion reaches the model unprompted — the same
`User { speaker: Agent }` row a job's ending produces, in a session that armed no monitor of its own.

**What letibot found reading the tree, and it changes what to do about the measurement.** The chain is
**closed on HEAD and was open in the build that was measured**:

  · **The daemon under test was a binary from the day before, and that is the whole explanation.**
    Measured on this box 2026-10-03: `target/release/harnessd` is dated **2026-10-02 20:17:36**
    (34724024 bytes), and `fd4aaad` (*"a subagent settles the same way a backgrounded job does"*) was
    committed **2026-10-03 21:16:24** — twenty-five hours later. The daemon serving leticl's workspace
    (pid 3840826) had been restarted minutes before the check and was running **that same file**, so a
    fresh PROCESS was running a stale BINARY: **a session restart does not rebuild a daemon.** The dated
    build cannot have contained the child path at all, and `0 monitors, 0 fired, 0 jobs` is exactly what
    it should do. leticl's reading of `completion_notice` as *"jobs only, no children"* is a reading of
    that build rather than of the tree.
    *(The pin is NOT the reason, and it was checked the wrong way first: leticl runs LOCALLY, so
    `LETIBOT_REF` describes its release packaging and says nothing about the daemon on this box.)*
  · On HEAD there is **one queue and one bell**, not two mechanisms: `JobWatchers::with_tasks` gives
    the watcher the session's `TaskRunner`, `watch_task` (`jobwatch.rs:432`) queues through
    `settled_here` — *"the push and the bell"* — and `Harness::wake` (`harness.rs:4442`) takes both
    kinds off `take_completions()` and partitions them **for wording only** (`job_output` for a job,
    `task_result` for a child), submitting one `User { speaker: Speaker::Agent }` item — the same row
    the ruling asked for. And the monitor waiter is **not** needed for this: the watcher rings the
    bell itself, so a session with no monitors is woken by a child.
  · **So the first thing to do is not to build anything: it is to re-measure on a daemon that has been
    REBUILT** (`cargo build --release`, then restart that daemon — the rebuild is the half that was
    missed), because the measurement above cannot distinguish *the code is wrong* from *the code was
    not in the binary* — and it is the second.

**The gap that remains, and it is mine — measured precisely, since the first version of this paragraph
overclaimed it.** The chain decomposes, and all but one piece is asserted:

| piece | asserted by |
|---|---|
| the completion is queued, with `kind: Subagent` | `a_subagent_settles_through_the_jobs_own_channel` (`jobwatch.rs:967`) |
| **the bell RINGS** for a child (`Work::Woken`) | the same test, `jobwatch.rs:1016` |
| a completion is taken once, not left for the next wake | the same test |
| a job's sentence | `a_completion_notice_names_the_job_and_says_do_not_wait` (`harness.rs:7764`) |
| **a child's sentence** | **`8533d84`** — landed, and it did not exist before |
| the partition: which noun for which kind, and none for nothing | **`a_settlement_is_two_nouns_out_of_one_channel`** — the partition is now `completion_notices`, a pure function, and it is asserted on a job, a child, both, two children, and an empty queue |
| **`wake()`'s remaining body**: taking the queue and submitting one `Speaker::Agent` item | **nothing, on either kind** |

**Narrowed again 2026-10-03 (the paragraph this replaces said the whole body was unasserted).** `wake()`
had the partition inline, where only a live `Harness` could reach it — and this crate has **no fixture
that builds a `Harness` at all** (every harnessd test is a pure-function unit test or a registry-level
integration test), which is *why* the decision had none. So the decision was lifted out into
[`completion_notices`] (`harness.rs`), which is pure and now tested; `wake()` keeps only the part that
is not a decision — take the queue, call the function, submit what it returns. What remains unexecuted
is therefore those few lines, and the reading in the bullet above is still the evidence for them:
reading rather than measurement, stated here rather than left to look stronger than it is. A `Harness`
fixture is the thing to build if even that is to be asserted.

## R58 — subagent TREES: delegation to a configurable depth, and no settlement without a worker — **LANDED 2026-10-03 (`9a9e8a1` + `51d5bd2` + `53e829c`); the design note below is kept for what it argued**

> **A feature, not a fix, and keeping that distinction is the first thing this row is for.** It exists
> because a commit message overstated a live defect that does not exist.

### The premise, verified rather than taken

**Delegation is one level today, and that is a fact about the SEAT TABLE rather than a property of the
design.** `crates/tools/src/runtime.rs`'s `roles::m2_coder()` — the seat every subagent is re-seated to
(`Seat::Coder`, `harnessd::config`, which re-seats for the tools and must not re-confine) — names
exactly

    read · write · edit · grep · glob · read_spill · todo · bash

and **`task` is not among them**, while `roles::orchestrator()` and `roles::leticode()` both name it.
So a grandchild is unreachable **in today's binary** — and the operator has ruled what the design is:
*“subagents are absolutely allowed to spawn subagents up to configured nesting level.”*

**CORRECTED 2026-10-03, twice, and the second correction is the one worth reading.** `660b585` filed
the absent `task` as a **safety property** — *“the guarantee is in the seat tables rather than in a
guard”* — concluded that there was **no live grandchild case**, and **retracted `fd4aaad`'s finding** as
an overstatement. All three are the same mistake, and it is not about subagents:

> **An inspection of what the code does today cannot correct a statement of what it is supposed to do.**
> The seat table lacking `task` is evidence about the **binary**. The ruling is evidence about the
> **requirement**. Where the two disagree, the binary is the thing that is wrong — which is what makes
> this a TODO item instead of a description.

The same shape as three defects fixed the same day — `plan_overrun`'s note describing a decision the
code no longer made, the three `rano` manifests whose comments still said *“the absolute path is
deliberate”* after the lines took a git tag, leticl's `install.sh` requiring eleven files while its
workflow packaged ten — with one difference that matters: in each of those the CODE was the authority.
Here the claim was the operator's own ruling, which is the one case where that ordering is never right.

**`fd4aaad` did not overstate it, and that retraction is WITHDRAWN.** The finding — *a nested child's
harness is built inside the runner's thread, outside `Sessions::open`, so nothing serves its wake* — was
correct when it was written and is correct now, and it is asserted in the code at `Sessions::wake`'s
early return and at the `registry.adopt` that creates the situation. Restoring it matters more than any
framing does: a reader who finds a retraction stops looking, and whoever writes the trees will need
exactly that finding. So the missing seat is the gap **this requirement exists to close**, and the
nested-wake problem below is live now rather than on the day trees land.

### The ruling

**Trees are allowed, to a configurable depth, default 3.**

### Two things settled while this is still a plan, because they change the diff

**1. The depth limit rides the CONFIG, not the seat table — and the idiom already exists.** *“Does the
seat name `task`”* can express exactly two depths, zero and unlimited, so it cannot carry a
*configured* limit at all. `base_role_for_seat` (`harness.rs:7508`) is already the shape that can:
`m2_coder` **lists** `bash`, and the config **strips** it when `!cfg.allow_bash` — the comment there
says why, and it is the same sentence this needs: *“listing it here is what makes the flag mean
something for this seat.”* So `task` is listed on the seat and stripped where the depth says no, with
the counter riding the config the child is built from — a `depth` field on the config, incremented once
at spawn (`harness.rs:7262`'s `sub_cfg`, built from `self.base`), and read where the tools are seated.

**And the refusal is by NAME, not by absence.** A seat that simply lacks `task` at the limit
manufactures the workaround, and that is this tree's own recorded lesson in the same file four hundred
lines away — `m2_coder`'s comment on `todo`: *“a capability that exists but is hidden manufactures the
workaround”*, measured at 13 tool calls and ~15k tokens of a model emulating the tool it had not been
given. A `task` call past the limit gets a refusal that names the knob.

**2. The wake fix is NOT depth-N-safe, and what is missing is the SERVANT rather than the routing.**
Verified in the code instead of argued:

  · `settled_here` (`jobwatch.rs:520`) rings `bell.ring_wake(&hub.session_id())` — and **which hub that
    is depends on whose watcher it is**: every harness builds its own, so a child that spawns a
    grandchild rings for the CHILD's session id;
  · `Sessions::wake` (`sessions.rs:1634`) is the only servant, and its second line is
    `let Some(harness) = self.open.get_mut(session_id) else { return Outcome::Ignored };`
  · a child harness is built inside the runner's thread (`harness.rs:7278`) and **adopted into the
    registry, not into `Sessions::open`** (`registry.adopt`, line 7288) — which is exactly why a head
    can peek at it and attach to it while the daemon still cannot drive it.

So depth 1 closes **because the watcher belongs to the PARENT, and the parent is in `open`**: the bell
is rung for a session the daemon owns, `wake` drains the queue, and the `[task]` row lands in the
parent's transcript. At depth 2 the watcher belongs to the CHILD, the daemon is woken for a session it
does not hold, and it **returns `Ignored` — the condition fires and is discarded**, which is the shape
of failure this row's own design note names. Routing a settlement through `completion_notice` therefore
buys nothing here: *a job is a job* is true of the **routing** and says nothing about the **servant**.

**Which is what “one slots list and one watcher set per TREE, rooted at the top session” is for** — and
it now has a reason rather than an assertion: **the bell must ring for a session the daemon can
drive**, and the only such session in a tree is its root.

### Four pieces, and the first three are mechanical

1. **Seat `task` where the depth allows it.** `task` is `Access::Session`, asserted and argued in
   `builtins/task.rs`: *"`task` runs no host command itself — it delegates to a child turn whose own
   gate governs its write/exec/network calls"*, and *"a subagent that could not even be spawned
   without a head would never run."* That is the same class as `todo`, which `m2_coder`'s own comment
   records as needing **no gate change** — so this piece is a seating decision and not a permission
   one. It does meet §8.4's ceiling: `m2_coder` is seven tools without `bash` and eight with it, so
   the daemon's `max_tools += 1` shape (`harness.rs`, the `bash` and `web_fetch` precedents) is what
   the addition follows.
2. **A depth counter on the spawn path**, so `task`'s seating is a function of where the child sits
   rather than of a flag somebody has to remember.
3. **The knob**, default 3, beside the other session settings.
4. **Orphan-proofing, which is the load-bearing piece and the reason a depth of 1 is not simply
   raised to 3.** See below.

### The piece that is not mechanical: a settlement must ring a bell that has a worker

The working design, and it is the whole of what makes depth safe: **one slots list and one watcher
set per TREE, rooted at the top session** — so a settlement always rings a bell that has a worker,
and **any ancestor can collect the handle**.

That is R57’s own finding with one more level under it. R57 is about a settlement nobody hears
(the bell and the worker are not paired, and the child's completion is a firing rather than a
wake); at depth > 1 the same defect gains a floor: a child's child settles into a tree whose root may
be the only worker left, and a handle held by the wrong level is a handle no key reaches. The shape
above is what makes "impossible at any depth" a property of the structure rather than a rule
re-checked at each level.

**Not to be confused with T24, and the word *orphan* is what conflates them.** T24 is about
**processes**: cgroups, worktrees, tmux sessions, what a child leaves running. This is about
**handles**: the slots list and the watcher set that turn a settlement into a wake. Two different
orphans, two different owners, and neither fix touches the other.

**LANDED 2026-10-03 (`9a9e8a1`, the handle-list half in `51d5bd2`, the flag in `53e829c`) — all four
pieces, with the two that mattered made testable.**

  · **1. The seat.** `m2_coder` names `task` and `task_result` (`runtime.rs`), seated at every
    depth on purpose: the cap is enforced at the CALL, not by the seat's absence, because a seat
    that simply lacked `task` at the limit manufactures the workaround this role already paid for
    once (its own `todo` note). The pair goes together for `orchestrator`'s reason.
  · **2. The depth.** `Config::depth` (0 for a root) is incremented once at `spawn_subagent`'s
    `sub_cfg`, and rides the config down the tree like `unconfined` does.
  · **3. The knob.** `Config::max_subagent_depth`, default 3, `--max-subagent-depth`. The refusal
    is `subagent_depth_refusal` (`harness.rs`), a pure function so it needs no harness to test:
    it names where the session is, the level it would have opened, and the knob that moves it.
  · **4. The worker's bell, and ONE handle list.** A child's watcher set is built from its ROOT's
    (`JobWatchers::shares_tree`): `completions`/`watching`/`settled`/`stop`/`bell` shared, and a
    new `wake_target` carrying the root's id so the ring goes **up**. The root's set rides `Parts`
    as `tree_watch` (`None` at `Parts::load`, the parent's set at spawn), and the target propagates
    unchanged, so a tree rings one bell with no parent walk. **And the same for the handles**
    (`51d5bd2`): `Parts::tree_slots` seeds the child's runner with the parent's `slots`, so the root
    can `task_result`/`job_kill`/list every handle in its tree — without it the root is *told* about
    a grandchild's settlement and answered *"no subagent … in this session"* when it asks.

Tests: `a_grandchilds_settlement_rings_the_tree_root` (jobwatch) drives a depth-2 settlement
through the CHILD's set and asserts the completion lands on the tree's queue and the ring names the
root; `the_depth_cap_refuses_the_spawn_one_past_it_by_name` (harness) pins the boundary; 
`the_coder_seat_names_the_delegation_pair` (runtime) pins the seating; and the plan-mode fixture
registers the pair — the same loud `resolve_role` path its own comment records for `todo` and
`bash`.

**still open?** `grep -rn "max_subagent_depth" crates/` finds the field, the flag, the refusal and
the two tests; `grep -n '"task"' crates/tools/src/runtime.rs` now names it three times
(`orchestrator`, `leticode`, `m2_coder`); `grep -n "wake_target\|tree_slots\|shares_tree"
crates/harnessd/src/` names the ring target and both shared channels. **Not yet exercised end to end**:
no test spawns a real depth-2 tree through the daemon, because that needs two live harnesses and a
model — the mechanism is pinned at the `JobWatchers` layer, which is where the defect was, and the
handle-list sharing rides the same untested path.

**One naming consequence, settled rather than left open.** `depth` in this tree already means the
*model's* depth (`config.rs:252`), so the SESSION's level is `Config::depth` and the KNOB is
`max_subagent_depth` — two names, not one word doing two jobs. That is the consequence this
paragraph asked for, applied.

**REFINED 2026-10-03 — and the first bullet below is WRONG, kept with its correction because the
correction is the one worth reading.** The claim was that the slots list is per-daemon and so
*"any ancestor can collect"* needed nothing. **It needed the second commit.** `Parts::load`'s
`Arc<TaskJournal>` is the dashboard's state file, not the handle list; the handles live on
`HarnessTaskRunner::slots`, which is per-SESSION — `collect`, `kill` and `started` all read it, and
`kill`'s own refusal says *"no subagent `{handle}` in this session"*. So the root could be told about a
grandchild and could not collect it. `tree_slots` (`51d5bd2`) is the fix, and the mistake is the same
shape as this row's own premise: a sound-sounding inspection of one structure used to conclude about
another.

  · **(WRONG — see the correction above.)** The slots list is per-DAEMON, not per-tree. `Parts::load` (`harness.rs:138`) builds ONE
    `Arc<TaskJournal>` for the process — the same one-0.6s-load argument as the vocab — and every child
    inherits that same Arc (`spawn_subagent`'s `Parts { tasks: self.tasks.clone() }`, `harness.rs:7285`).
    So `task_result` already reaches any handle from any ancestor. *"Every ancestor's handle can be
    collected from any level"* is satisfied **more widely than the tree**; nothing to build. (If a
    tightening to *exactly* the tree is wanted later — so a sibling root cannot collect another root's
    child — that is a separate, deliberate narrowing and not part of this.)
  · **So the whole of piece 4 is the watcher set, and it is one field.** `settled_here`
    (`jobwatch.rs:520`) rings `bell.ring_wake(&hub.session_id())` — the **child's** id — and queues the
    `JobCompletion` on the **child's** `completions`. Both are per-session because the child's set is
    built fresh in `open_with_registry` (`harness.rs:2283`). At depth 2 that queues onto, and rings for,
    a session `Sessions::open` does not hold — the `Ignored` above, from one field rather than a missing
    structure.
  · **The shape the fix takes.** The tree's watcher set is the ROOT's and a child's is built from it:
    `completions`, `watching`, `settled`, `stop` and `bell` shared (Arcs — `with_tasks` already shares
    three of them), a `wake_target: String` carrying the root id so the ring goes **up** rather than out,
    and only `hub`/`host`/`tasks` new per child. The root's set rides `Parts` as a
    `tree_watch: Option<Arc<JobWatchers>>` — `None` from `Parts::load` for a root, `Some(self.job_watch)`
    at `spawn_subagent` — so `open_with_registry` finds it and shares instead of building. No new root
    walk: the set it is built from already knows its root.
  · **And piece 4 cannot land alone.** At depth 1 the watcher that rings belongs to the parent, which IS
    the root, so the change is **unobservable and untestable today** — a grandchild cannot exist until
    `task` is seated past level 1. Pieces 1 and 4 land together or the diff is scaffolding nothing can
    exercise.

**done when** a session at depth 3 spawns a child that spawns a child, each child's settlement wakes
the tree exactly once, **every ancestor's handle can be collected from any level**, and a depth one
past the knob is refused by name rather than by a stack that quietly runs out — with the correction
above carried wherever `fd4aaad` is read.

## R59 — the context wall's denominator moves a third between firings — **OPEN, raised by the operator 2026-10-03**

The operator, on letibot's compaction: *"something is off here, like too much too early too wtf."* Four
`context wall` lines on one daemon whose `context_window` is 999,999 — **and no way to say which session
each was about**, because the wall line carried no id, which is why that is the first thing fixed below.

    stopped after 13 round(s) at 1400086 of 1118167 tokens
    stopped after  7 round(s) at 1463534 of 1484966 tokens
    stopped after  2 round(s) at 1257759 of 1199811 tokens
    stopped after 16 round(s) at 1429974 of 1484829 tokens

**The denominator is `planning_window()`, and it is a measurement rather than a bug.** `Config::ledger_scale`
is `(ledger.len(), the last round's prompt_tokens)`, refreshed at the end of **every** round
(`harness.rs:5848`), and `planning_window` is `context_window * ledger / provider` clamped to
`[w/4, 4w]`. The four windows above are 1.12x–1.48x the configured 1M, i.e. the ledger ran 12% to 48%
wider than the provider's own count of the same prompt — **how much of that session was reasoning the
messages provider is never sent**, which the docstring already says varies per session and over the life
of one. So the ratio is doing its job; what moves is the printed pair.

**And the threshold the wall FIRES on does not move at all — the swing is in the display only.**
Verified from the code rather than argued: the check is `resident + headroom() >=
planning_window()` where `resident = session.ledger.len()` at the top of the round loop and `ledger_scale`
was taken at the bottom of the previous one, so `resident == ledger`; `headroom()` is `w/16` at these
sizes; substituting gives `ledger >= 15/16 * w * ledger/provider`, the `ledger` cancels, and the
condition is exactly **`provider_prompt_tokens >= 15/16 * context_window`** — one fixed fraction of the
configured window, independent of the ratio. The wall fires in the provider's units; only the *ledger*
pair printed beside it swings. That is the same unit confusion the operator already caught once
(*"1.35 is a lie - that top was shown as 900+"*, `Config::shown_tokens`' docstring), now in a log line
instead of a notice.

**Which makes the two asks different sizes.**

  · **The measurement is theirs to rule on: smooth or not.** A smoothed ratio (an EMA) would hold the
    printed denominator still and cost lag — the window would track a genuinely changing reasoning
    fraction a turn or two late — and it would not move the fail point, because the fail point is
    already ratio-independent. This tree's own argument for the ratio is *"it is not a constant: it is
    how much of the conversation is reasoning"*, and smoothing is the one change that makes it less of
    the measurement it was written to be. Recommendation: **leave it per-round**; if the printed number
    is the problem, fix the printing.
  · **The display is not: print the provider's figure.** `{resident} of {window}` is in ledger tokens
    and every notice a head draws is in the provider's (`shown_tokens`). Printing the provider-side
    number (or both, as the wall *notice* already does with its `(counted as the provider counts them;
    …the ledger says X of Y)` aside) makes the log line read in the same unit as the screen it
    describes.

**The id that makes it decidable is landed** — `bd012ed` gives the `context wall` line the session name
its `compacting:` neighbour has had since this same morning, so the next occurrence pairs a wall with the
compaction beneath it **by id rather than by adjacency**. That is what answers *"too much too early"*:
whether the 1.25M-to-633k folds and the 985k-to-8k compactions in the operator's log are one session
behaving differently or two behaving consistently — and note the two are different paths with different
contracts (a fold and a compaction), which the operator's own caution says not to conflate.

**still open?** `grep -n "context wall:" crates/harnessd/src/harness.rs` finds the id;
`grep -n "ledger_scale =" crates/harnessd/src/harness.rs` finds the one per-round refresh at 5849 and the
recovery at 2833; no smoothing exists anywhere, so both routes above are unstarted.

**done when** the wall's log line and the compaction's can be paired by id (done, `bd012ed`), and either
the ratio is left per-round with the printing moved to the provider's unit, or a smoothed ratio is added
with the argument for the lag written down — so that the denominator on the screen and in the log is the
number the operator can check, and the analysis *"too much too early"* rests on attributed lines rather
than adjacency.

### The same mismatch from the DISPLAY side — **added 2026-10-04**

The operator, on a compaction that fired while the header read half a million: *"it showed me
compaction when screen showed only 500k tokens."*

**The number on the screen and the number that decides are different units.** `Config::shown_tokens`
is `provider_tokens(ledger_tokens)` — what a request would actually carry — while `should_compact`
and the `compacting:` line are in LEDGER tokens. MEASURED on that session:

    log:     compacting: 1390742 of 1000000 tokens resident   -> compacted: 834514   (the fold)
    log:     compacting:  947896 of 1000000 tokens resident   -> compacted:  12594
    header:  947896 of 1000000                                 (agreeing, once the ratio was near 1)

The two agree now and did not then: a ~2.8x gap, inside the clamp of four. **The gap IS the reasoning
the messages transport never sends** — the ledger holds it, the provider's count does not.

**And `config.rs` already names the direction of the error**, in `tokens_are_converted`'s neighbour:
*"a ledger figure standing in for a provider's — up to the clamp's factor of four out, **in the
direction that compacts early**."* So *"too much too early"* is not a misreading by the operator; it
is the documented failure mode seen from the only place they can see it.

**This is the same root as the entry above, from the other end.** There the denominator moved between
firings in the log; here the numerator is printed to the operator in one unit and decided in another,
with nothing on the glass saying which is which. A reader cannot predict a compaction from the
display, which is the whole of the complaint.

**done when** one unit reaches the operator — either the decision is stated in the unit the header
shows, or the header names both and says which one compaction uses. `tokens_are_converted()` exists
precisely so a message *"can name the other number once rather than per figure"*; nothing is calling
it on the compaction path.

## The `Subagent` event's `prompt` is a title, and on the finish it is the child's answer — **LANDED 2026-10-03 (`83be154`) on letibot's side; the wire now carries the task**

**(No R-number: the series is yours to number.)**

The operator, reading the subagents pane: *"the first prompt is truncated too early"* and *"I want to
be able to easily see it in full"*. MEASURED, and it is not a truncation the head can undo:

```rust
let title = derive_title(prompt);   // harness.rs:7177 — "the title is the subtask's first line"
publish("opening", &title);
publish("running", &title);
publish("done",    &first_line);   // harness.rs:7322 — the CHILD'S ANSWER's first line
```

So the one field a head has for *what this child was asked to do* is the task's first line from the
start, and on the finishing event it is replaced by what the child said. Read out of the running head:
the row for a child spawned from a session carried **122 characters which were its answer**, while the
task was two lines and ~250 characters — the head had no copy of the task anywhere, and the pane's
first line is where the operator saw it.

**The ask, and it is two small things:** carry the task itself on the event (the head can truncate for
a row — it already truncates everything else it draws), and if the picker wants the child's answer as
a subtitle then that is a second field, not this one rewritten: a field named `prompt` that holds a
title on two states and an answer on the third cannot be read by either party without knowing which
state it is.

leticl's half is already in: the child's task is drawn in full in the peek, from the transcript read
(`821d218`… `the-subagent-pane-draws-the-task-in-full-and-follows-the-rung`). The pane's *row* cannot
be fixed until this is.

**LANDED, 2026-10-03 (`83be154`).** Both halves of the ask, and the wire is letibot's so leticl's
pane row can be fixed now:

  · **`task: String`** on `SessionEvent::Subagent` — the subtask in full, the same string on every
    state, `#[serde(default)]`, and **never truncated by the daemon**: the head truncates for a row as
    it does for everything else it draws.
  · **`answer: Option<String>`** — the child's answer's first line, `Some` only on the finish. The
    same string `prompt` carries there, given its own name so a reader does not have to know the
    state.
  · **`prompt` is left exactly as it was**, two meanings and all, which is what keeps a head older
    than these fields byte-identical; a new head reads the row from `task` and falls back to `prompt`
    when `task` is empty — which is the pre-field behaviour, not a blank row. letibot's row does that
    (`subagents_lines`), flattens newlines because the row is one line, and draws `answer` as the
    subtitle; Enter still opens the whole thing in the peek.

`a_subagent_row_shows_the_task_and_the_answer_beside_it` pins both halves; the pane's older fixtures
carry `task: ""` and keep exercising the fallback.

**leticl:** the fields are on the wire with no protocol bump (a volunteered frame may grow a field,
not a variant), so its row can read `task` where it reads `prompt` today and its subtitle `answer`.

**still open?** `grep -n "task: task.clone()" crates/harnessd/src/harness.rs` finds the publication;
`grep -n "s.task.is_empty()" crates/tui/src/app.rs` finds the fallback. Nothing open on this side.

**done when** a long, multi-line task reads whole on the pane's row (or an unfold of it does) without
opening the child.

## A sub-session is a SESSION a head may ATTACH to — the filter that hides it is the bug — **OPEN, corrected from leticl 2026-10-03**

**CORRECTED THE SAME HOUR, AND THE CORRECTION IS BIGGER THAN THE ASK BELOW.** The operator: *"why readonly?
subagent session is more like you driving others via tmux. I already can post to subagent, and agent can
talk back and forth too"*. **A child is not a thing to be VIEWED; it is a session to be ATTACHED to** —
the same relationship this session has with the head it is spoken through, one level down — and the
defect is five lines that hide it on purpose:

```
letibot   app.rs:3421   .filter(|s| s.parent_session_id.is_none())
          app.rs:3505   .filter(|s| s.parent_session_id.is_none())
leticl    head.lisp:1053    (remove-if … :parent-session-id)
          session.lisp:382  (remove-if … :parent-session-id)
          panes.lisp:77     (remove-if … :parent-session-id)
```

leticl's `panes.lisp:71` records the provenance — *the reference filters `parent_session_id.is_none()` on
both `Hello` and …* — and `chrome.lisp:363` states the belief out loud: **a subagent is not a session a
picker lists**. That belief is the bug. Everything downstream of it — `/peek`, `Peeked`,
`sub_out_lines`, `subagent-out-lines` — exists to work around a session that was hidden on purpose.

**So the ask is not a richer `Peek` at all, and the read-only snapshot I asked for was a workaround for
the filter. Everything that follows is DELETION:**

  · **the picker lists sub-sessions**, under their parent or marked with it — the parent id is already
    on the row, so the information is there and is being thrown away;
  · **attaching to one is ordinary**: `Attach { since_seq: 0 }` answers with a `Snapshot`, which both
    heads already draw with the real renderer — markdown, air rule, tool cards, the rung. Nothing to
    build;
  · **`sub_out_lines` and `subagent-out-lines` are deleted**, not taught and not re-pointed;
  · **the subagents pane becomes a view onto sessions-with-a-parent**, not a second renderer with its
    own idea of what a row looks like;
  · and **leticl's `a216cfa`** — the copy taught about tasks and rungs — goes with the copy. It is
    scaffolding around a session hidden by five lines, and worth naming as such.

**The question that is actually yours:** does `Peek` still earn its place once the filter is gone? It may
— reading a child without leaving the parent is a real convenience rather than a necessity — and that is
yours to judge. The correction above is why I am not asking for a snapshot variant any more.

**And one thing neither head should lose in the deletion:** the filter existed because a picker full of
children is noise, and twenty subagents would bury the four conversations the operator cares about.
**Nesting is the answer, not listing flat** — a child under its parent is both discoverable and quiet.
The reference's instinct was right about the symptom and wrong about the cure.

**MEASURED ON THE REBUILT DAEMON, 2026-10-03 23:40 — AND THE MEASUREMENT WAS RIGHT WHILE THE
CONCLUSION DRAWN FROM IT WAS NOT.** Daemon 652570, started 23:34:31 from the 23:28 rebuild — the field
exists, and that peek answered

    events  : 652
    snapshot: NIL

**`snapshot: NIL` beside 652 events is the daemon answering an `Events` request exactly as designed, and
"not reachable" (this paragraph's first form, and leticl's `eb896f3`) is the stale-claim defect this pair
has now corrected four times.** `server.rs:765` makes the two shapes **alternatives, not a pair** — a head
that asks for `Rows` gets the snapshot and an EMPTY ring, because sending both would put the same content
on the wire twice. So the field was reachable all along; the peek simply did not opt in, and
**a one-field change on the `Peek` a head sends (`shape: PeekShape::Rows`) is the whole of what was
missing.** leticl's half is still inert only because its peek sends `Events`; the `shape` field is on the
wire and the daemon has answered `Rows` since `1520bb5`.

leticl's half is ready and inert: `%peek-snapshot-pane` draws rows through the real renderer the moment
any arrive, the event path stays as the degraded one and SAYS so, and the copy is deleted the day the
pane's own peek answers with rows.

**still open?** `grep -n "parent_session_id.is_none" crates/tui/src/app.rs` answers **one** — and it is not a
filter: it is the header's root COUNT (`app.rs:12002`), which deliberately counts conversations rather
than rows so an expanded tree does not change the position indicator. The two filters this section was
filed against are gone.

**LANDED ON LETIBOT'S SIDE, 2026-10-03.** `c840aee` removed both `.filter(|s| s.parent_session_id.is_none())`
call sites the grep above used to answer, and put the whole enumeration behind one `App::session_rows` /
`push_children` / `session_root` (five readers routed through it, `ctrl-s` the picker, children nested
under their parent and counted as conversations in the header). `dc60a0c` then made this head **ask for
rows** — `PeekShape::Rows` — and draw them through `item_lines`, the one renderer a transcript row uses,
so a peeked child is a tool card on the pane rather than a hand-rolled dump; the event path survives only
as the explicitly-degraded fallback an older daemon forces, and it says so. So on this head the
**rendering** half of *"both plain-string renderers are DELETED, not taught"* is satisfied by making
`sub_out_lines` a thin shell over the real renderer — kept, because the `Peek` door was kept — rather than
deleted; the deletion the section asks for is only reachable once the peek's own answer is never the ring.

**RULED, 2026-10-03: `Peek` EARNS ITS PLACE — the FIRST of the operator's three answers — and
`PeekShape::Rows` is its shape.**

  · **It is not the filter's workaround and did not die with the filter.** What `Peek` does that attaching
    cannot is **read a session without moving the head's own**. Attaching changes `session_id`, and that
    changes *where the next prompt goes*; and by this head's own words in `switch_to`, going to a session
    "has no state" kept and "this head does not keep per-session marks" — so the reader's place is lost
    and coming back is a fresh resync. **A read that moves the head is not a read**, and that is the
    reason this survives the deletion of the filter, the picker's flatness and the second renderer.
  · **With `Rows` it costs no second renderer.** `sub_out_from_rows` runs the session's items through
    `item_lines` — the same one every transcript row uses — so the cost that made the deletion attractive
    is already gone; what remains of the hand-rolled path is the degraded fallback alone.
  · **So the two plain-string renderers are FALLBACKS, not alternatives.** Each head keeps at most one,
    and it says so — the operator's own rule: a `None` snapshot falls back to the ring **and says so** —
    or deletes it if it will not answer a daemon older than the field. **Neither is deleted because the
    peek is redundant, because the peek is not.**
  · **leticl carries the one-field change**: `Peek { shape: PeekShape::Rows }`, and its pane draws through
    the real renderer the moment rows arrive (its half is already written and inert, per its own note).
  · **One recommendation, for parity rather than the wire**: the two heads should agree that **Enter on a
    row READS and a neighbour ATTACHES** — attaching on Enter silently redirects the next prompt to the
    child, which is exactly the hazard the read exists to avoid. This head is Enter = read, `o` = attach;
    leticl's new Enter = attach is the half to reconsider, and the operator's call.

**done when** a head attaches to a sub-session like any other, and neither `sub_out_lines` nor
`subagent-out-lines` exists.

**THE ASK AS FIRST FILED, kept for the record and superseded by the correction above.**

**(No R-number: the series is yours to number.)**

The operator, correcting leticl's plan before it was built: *"yes subagents are not even scratch
session they are session, just sub sessions"*. That settles the whole shape:

  · **`Peeked` is why there is a second renderer in each head.** Its own docstring says the events it
    carries are *for reading, NOT FOR FOLDING INTO THE HEAD'S STATE* — so a head with a peek may not
    fold them, and has nothing to do but draw them by hand. That is `sub_out_lines` here
    (`app.rs:12682`) and `subagent-out-lines` in leticl, a copy of it;
  · **the door that returns the right thing already exists**: `SessionEvent::Subagent`'s `subagent_id`
    is *the subagent's own session id*, and `Attach`/`Resync` with `since_seq = 0` answers with a
    `Snapshot` — rows, which both heads already draw with their real renderer, markdown, air rule,
    tool cards and rung and all. **Nothing new is invented; a door that is unused for children.**

**The ask: `Peek { session_id }` answers with a `snapshot` as well as (or instead of) `events`** — or,
if you prefer, a read-only attach to a child id. leticl's preference is the first, and the reason is
the frame name: `Peek` promises *this is a read, not an attachment*, which is a property worth keeping
explicit — an attach that only read would blur the two gestures the subagents pane already owns
(Enter reads a child, `o` switches into it).

Then **both plain-string renderers are DELETED, not taught**: leticl deletes `subagent-out-lines` the
day the snapshot lands, and its half is ready. Worth a day's wait rather than a head-local fold,
because two folds for one conversation is the drift this pair has hit five times in two days — the
thinking count drawn twice, leticl's three file lists, `BINARIES`, the weather list, and now this.

**still open?** `grep -n "Peeked" crates/sessionlog/src/protocol.rs` — the reply has `events` and no
`snapshot` field.

**done when** a peeked child is drawn by the same renderer as any session in both heads, and neither
`sub_out_lines` nor `subagent-out-lines` exists.

## Completion notices are one-liners in leticl and full prose here — **CLOSED on this side 2026-10-04 by `1f0146a`; two pieces named below are not in it**

**(No R-number: the series is yours to number.)**

The operator, reading a settled job and a finished subagent: *"too much, for example i dont want to see
that message to you 'This is the completion…' I also dont care about 'sabagent you started…' it must be
something like Job <id> <command summary or wrap> finished <result result summary or wrap> same for
agents."*

**leticl folds them, and the whole of it is existing machinery you already have on your side**: the
notice arrives as a `User` row with `speaker: agent`, the fact lines are the `- ` lines under the
daemon's own heading, and **the closing paragraph is a promise TO THE MODEL** — which is exactly what the
operator does not want to read. leticl drops it, keeps the daemon's words verbatim otherwise, and the
full message stays one verb away (`/t` on the row that has one). Its row, read off the operator's live
screen:

    session · Job j83 exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done
    session · Agent …-sub-1791065633114 · Answer with one word: ready. · done: ready.

Three details worth copying rather than rediscovering:

  · **the vocabulary is TWO openings, and it was one.** `[job] ` was folded and `[task] ` was not, so an
    agent's completion — the same shape, the same speaker, the same paragraph — drew as full prose while
    a job's was a line. It is a list now (`[job] `, `[task] `), and the agent's row is named
    `Agent <id> · <task> · done: <answer>`;
  · **the agent's own task is LOOKED UP, not parsed.** The notice carries only what the child answered;
    what it was asked is on the subagent row (the title — its task's first line), so the row and the
    subagents pane cannot disagree;
  · **counts are per GROUP, not per opening.** `[job] 3 jobs … have ended:` is ONE heading with three
    settlements under it — counting headings read `1 job ended (j12, j15, j19)` — and a coalesced row
    can hold a job's and an agent's, where "2 jobs ended" calls a subagent a job.

leticl's commit: the one-liner is `9b22de3`'s shape and the coalesced-count fix is in the commit after it.

**CLOSED on letibot's side: `1f0146a`.** `folded_notice` (`app.rs`) folds a `[job]`/`[task]` notice
at DRAW time, in the `Speaker::Agent` arm, and the paragraph is simply not drawn. It keeps every
fact, one per line — `Job j57 exited 0 after 7m06s, wrote 508 bytes: sleep 3; echo done`,
`Agent s-…-sub-… · done: 3529`. **The refusal is the half worth keeping**: the opening must be one
of the two known ones, a settlement line must be a bullet with its handle in backticks, and the
only text that may follow is the promise, by name — anything else returns `None` and the row draws
raw, so a notice whose shape changes fails visibly (*"the notices got long again"*) rather than
silently (*"a notice lost a line"*). The `[monitor]` notice is the live case: same voice, same
bullets, deliberately not folded, because leticl does not fold it either.

**still open?** `grep -n "you do not need to wait for it" crates/tui/src/app.rs` — the phrase now
appears only in `folded_notice`'s refusal check and its tests, so a *drawing* reference is what this
is looking for.

**TWO PIECES ARE NOT IN `1f0146a`**, and they are the remainder of this row:

  · **the child's own task in the `Agent` line.** leticl draws `Agent <id> · <task> · done: <answer>`
    and the task is LOOKED UP — the notice carries only what the child answered, and the title lives
    on the subagent row. This head draws the handle and the answer, no task. Threading it means one
    more field on `ItemCtx` (`subagents: &'a [SubagentState]`, four construction sites) — not done,
    and **not to be parsed out of the notice**: a second source for the same fact is how the row and
    the pane come to disagree;
  · **opening the whole message on demand.** leticl's folded line says `· /t opens it` and its `/t`
    reaches a `User` row. letibot's `/t` is *all tool rows*, so this needs a verb or nothing. Ruled:
    nothing for now — the transcript and the model's own copy are untouched, which is what makes the
    fold a rendering rather than a loss.

**THE PLAN AS MEASURED, 2026-10-04** (kept because it is what the two pieces above are against). The render site is
`app.rs:18197`, the `Speaker::Agent` arm of `item_lines` — `(RowClass::Other,
session_block(&text, its.ts, cfg))`, the raw paragraph. The two shapes on the wire, read
out of the store (`transcript_item.item_json`, `"type":"user"`, `"speaker":"agent"`):

    [job] a job you backgrounded has ended:
      - `j57` exited 0 after 7m06s, wrote 508 bytes: <the command>
    <the paragraph, from `completion_notice`, harness.rs:691 — `job_output` verb>

    [task] a subagent you started has finished:
      - `s-…-sub-…` done: 3529
    <the paragraph, from `subagent_notice`, harness.rs:732 — `task_result` verb>

and **one row can hold BOTH groups**, joined by a blank line (measured: a `[job] 3 jobs …`
heading with three settlements, then a `[task]` heading under it) — which is why the group
counts have to be per GROUP and not per row.

**Three things make it more than a pure function here, and they are the whole cost:**

  · the TASK line wants the child's own task in it (`Agent <id> · <task> · done: <answer>`),
    and leticl LOOKS IT UP — the notice carries only what the child answered, and the title
    (the task's first line) is on the subagent row. `item_lines` is given `ItemCtx` and not
    the subagents pane, so the title has to be threaded in or the row resigned to the id
    alone. Do not parse it out of the notice: a second source for the same fact is how the
    row and the pane come to disagree;
  · **leticl's row keeps the whole message one verb away** (`/t` on the row, and its folded
    line says `· /t opens it`). letibot has no such verb for a `User` row — `/t` is *all tool
    rows* — so either the paragraph is simply not drawn (the text stays the row's in the
    transcript and the model still reads it) or a verb is added. **Ruled: do not add a key
    for this yet.** Draw the fold, and say in the row's own doc that the record is intact;
  · **the fold must not touch what the MODEL gets.** The paragraph is the half of R7 that
    tells the model not to `job_wait` — it is a promise to the model, which is exactly why
    the operator does not want to read it. Display only, and a test that asserts the raw
    text is unchanged by the fold.

**done when** both heads draw a settled job or a finished subagent as one row that names it, drops the
model-facing paragraph, and opens the whole message on demand. **leticl's half is landed, `/t` and all;
letibot's is the plan above, with that last clause deliberately not in this change — and the raw text
staying in the transcript, and in the model's context, is what makes that honest rather than a loss.**

## The notice rows stopped on the daemon rebuilt at ~14:2x — **CLOSED 2026-10-04: they never stopped; two heads render one row two ways**

**(No R-number: the series is yours to number.)**

The operator, watching for the two things this file asked for: *"interesting - the job finished but i dont
see notification about it"*, and then *"subagent finished too - no notification. i think we had that
before restart"* — and the second sentence is the diagnosis.

**Measured, on the daemon rebuilt at ~14:2x (leticl head 1700841 attached to it):**

  · **the EVENTS are fine**: the head's jobs list holds `(("j19" "exited 0"))` — the `job_settled` event
    arrived and updated the row — and the `Subagent` events arrive too, so the subagents pane and the
    completion cards' inputs are all there;
  · **the NOTICE ROW is absent**: no `User { speaker: Agent }` row for either settlement — not on the
    glass, and not delivered to the model either. Both were present for every job and child before the
    restart (`Job j83 exited 0 …`, `Agent … · done: …`).

**CORRECTED THE SAME HOUR — THE PUBLISH PATH IS FINE, AND THE STORE PROVES IT.** Querying the daemon's own
store (`~/.local/share/letibot/sessions.db`, `transcript_item.item_json`):

    120138 | 2026-10-04 14:44:58 | {"type":"user","parts":[{"kind":"text","text":"[job] a job you backgrounded has ended:
    120012 | 2026-10-04 14:40:48 | {"type":"user","parts":[{"kind":"text","text":"[job] a job you backgrounded has ended:

Both written AFTER the ~14:2x restart. So `completion_notice` publishes, the row is in the model's
transcript, and **the operator's other head receives it** — *"yet a newly restarted letibot gets job
completion event"*. What does NOT have it is **leticl's attached head**: its newest agent-speaker row is
`j111`, from before the restart, and no notice row since appears in its items at all.

**So this is per-head DELIVERY, not the publish path** — one connection gets those rows and the other
does not — and the question is now narrow: what differs between the two heads' attaches (identity, hub
membership, or the seq they are served from) such that a `User { speaker: Agent }` row written to the
store reaches one and not the other. leticl's folding half is known good: given such a row its parser
turns it into the right card (measured on the pre-restart rows, `Job …, result …`).

**RESOLVED THE SAME HOUR, AND THERE WAS NO DEFECT.** A child was spawned to land a notice while the
operator watched, and leticl's own screen, read seconds later:

    session · Agent Report the number of lines in /home/dead/Projects/leticl/src/head.lisp, as one number, with no other words.
                  result  done: 2201 · /t opens it

The notice is delivered to leticl, folded and drawn. What the operator had been seeing was **letibot's
rendering of the same row**: `letibot_transcript::Speaker::Agent => (RowClass::Other,
session_block(&text, its.ts, cfg))` — the raw paragraph, unchanged, which is exactly the text they
pasted. leticl folds that row into a card; letibot wraps it whole. **Two heads, one row, two
renderings — and the parity question is whether letibot wants the card too**, not whether anything
is broken. Everything in this entry above is superseded: the publish path was fine, the delivery was
fine, and the one real finding was mine looking at the wrong pane.

**AND THE ONE FIELD THAT DIFFERS BETWEEN THE TWO CONNECTIONS IS THE IDENTITY.** leticl attaches with
`:identity "leticl"` (`src/head.lisp`, `%try-reconnect`'s `make-attach`), and the letibot head the
operator watches attaches with `--identity dead`. Everything else about them is the same daemon, the same
store, the same session, the same moment — and one of them is served a `User { speaker: Agent }` row and
the other is not. That is worth a look before anything else: **what the hub does differently for a
connection whose identity is not the one the row is addressed to** is the whole question now, and it is
one word to test from either side.

**still open?** start a background job or a `task` child on this box and watch for the `[job]`/`[task]`
row in the parent's transcript — absent means it is still open.

**done when** a settled job and a finished child each produce one notice row on the parent's hub, on a
daemon built from the current tip.

## A session's JOB notices STALL — 29 s late, or never — while its CHILD notices keep coming — **CORRECTED AND CLOSED 2026-10-04 15:40: it was never per-KIND; two settlements were swept by a transcript FORK**

**(This supersedes the CLOSED heading on the notice-rows entry above; that closure was too broad.)**

Two background jobs on this box, twenty minutes apart, both started by the model through the same path:

    j19  45009 ms  exited 0  -> its `[job]` notice IS in the store (14:44, and leticl drew it)
    j56   5012 ms  exited 0  -> NO `[job]` row anywhere in the store, minutes later

**And leticl's head saw BOTH settlements** — its jobs list reads
`(("j19" "exited 0" 45009) ("j56" "exited 0" 5012))` — so the `JobSettled` hop is fine for both. What
differs is the WAKE: the notice row reaches the transcript for one and not the other.

**The shape to look at is the queue-take**, and your own words point at it: *`wake()` is left with the
part that is not a decision: taking the queue and submitting what this returns* — and *an empty queue
returns nothing* rather than running a turn with nothing to say. A settlement that lands at, or after,
the moment the queue is taken has no second chance: nothing re-arms. Same family as D26 (*a wake spends a
generation*).

**What is NOT broken, so nobody re-tests it:**

  · **child completions publish**: one landed minutes ago and leticl drew the card off it, verified on
    the glass (`session · Agent Report the number of lines in head.lisp…` over `result done: 2201`);
  · **the publish path writes the store** (j19's row is there);
  · **leticl's fold is correct** on every row it receives, and the head is served them.

**still open?** start two background jobs a few minutes apart and query the store for both notices:
`select ... from transcript_item where item_json like '%<job id>%'`. One missing is this entry.

**MEASURED FURTHER THE SAME HOUR — THE SPLIT IS PER SESSION, NOT PER KIND.** Job notices are being
published on this daemon all day: 12 today, 21 yesterday, 25 the day before; the newest is 14:54:43.
**8 went to `s-1789462738453908838#t41` (newest 14:54:43) and 4 to `s-1791017230755743833`, whose newest
is 14:09:14** — that is the session which started `j19` and `j56`, and which has received no job notice
since 14:09. Its `[task]` child notices kept arriving throughout the same window (newest 14:56:50), so
this is not one session going deaf: **in one session one kind stops and the other kind keeps coming.**
Three jobs settled after 14:09: `j19` (~14:44), `j56` (~14:57), `j2` (`sleep 8; echo job-demo-two-settled`,
launched 15:08:30). **`j2`'s notice did arrive — 29 seconds late**, written 15:09:07 and handed to the model
right after; a store query one second before that read 6 notices, newest 14:09:14, which is why it first
looked lost. So the shape is a **stall, not a hole**: 29 s for `j2`, while `j19` and `j56` are still absent
12 and 25 minutes on. Latency against settlement, measured: `j2` 29 s; `j19`, `j56` unbounded so far.

**And the head was restarted at ~15:0x** — the operator's doing. The restarted head's job registry holds
**only `j2`**: `j19` and `j56` are not in it at all. So the restart is a concrete drop point: a settlement
whose notice is still queued when the head reattaches never gets submitted, and its job leaves the client's
registry with it. `j2`, started and settled after the restart, got its notice 29 s late — when the session
next went idle, not at the settlement.

**Prediction to test:** start a job and restart the head before the session next goes idle — the notice is
lost. A job that settles while the session stays continuously busy is delivered at the next idle turn,
however long that is. **Take every reading twice**: this defect has been misread four times here, each time
from one query treated as final.
**Take the reading twice before calling anything missing** — this defect has been misread four times here,
every time from a single query treated as final. So the wake is not dropping settlements at random — it
**keeps publishing job notices for one session and stops for another**, while children keep getting
through in the same session. Look at what makes a session eligible at the moment of the take, rather
than at the settlement itself.

**done when** every settlement that `JobSettled` reports is also submitted as a wake item, once, however
the timing falls.

**CORRECTED AND CLOSED — THE FIFTH READING, AND IT IS THE ONE THE ENTRY ASKED FOR TWICE.** The mistake
was the dimension the query grouped by: **`transcript_id` is not stable, and every reading above crossed a
fork without saying so.** Read per transcript, out of the store (`transcript_item`, `"type":"user"`):

    s-1791017230755743833#t2   14 job notices, newest 2026-10-04 14:09:14   tasks newest 14:56:50
    s-1791017230755743833#t3    3 job notices, newest 2026-10-04 15:34:36   tasks newest 15:35:05

`#t3` begins at **14:58:48**, and its first rows are a CARRY of the last few rows of `#t2` (`what child`,
a `[task]` notice, `ok now do job`) all stamped 14:58:48 by the copy — so the session forked 79 seconds
before that stamp, which is the context wall firing (`Your previous turn was stopped at the context wall`
is the first row of every fork on this box, mine included: `s-1789462738453908838#t42` at 15:18:17).

**So the two missing settlements were not a channel going deaf: they were swept by the fork.** `j19`
settled ~14:44 and `j56` ~14:57, both while `#t2` was current; the fork at ~14:57–14:58 replaced the
conversation, and **a wake that was queued for a turn that had not run yet does not survive its
transcript being replaced** — the same family as `7fc4cec` (a fill's bar ends when the snapshot replaces
the stream) and R16's echoes, which is why those two were fixed the same way and this was not noticed.
Everything the entry read as *jobs stop, tasks continue* is that fork: **`#t3` receives BOTH kinds, and
has all afternoon** — 3 job notices (15:09:07 `j2`, 15:12:32 and 15:34:36, both `3 jobs … have ended`)
and 4 task notices, the newest 15:35:05.

**And the entry's own prediction is FALSIFIED, which is what closes it.** *"Start a job and restart the
head before the session next goes idle — the notice is lost."* The operator restarted twice in that
window (`yeah I irestartred you`, 15:09:29, and `restarted, lets tet 3 jobs and 3 agents again`,
15:33:46) and **every job notice after each restart arrived**, `j2`'s 29 s later and the 3-job group 5 s
after the second. The restart is not the drop point; the fork is. The one thing that would falsify THAT
is a settlement whose notice is absent while its transcript was never replaced — one job, one query, no
fork in between, **and the `transcript_id` read the second time as well as the first**.

**What is left is narrow and it is still a defect**: a settlement that lands while a turn is queued and
whose transcript is then replaced loses its notice to nobody's decision. Filed below as the wake's own
half of R16 rather than as the stall this entry spent four hours naming wrongly.

## A wake that has not run yet does not survive its transcript being replaced — **NEW 2026-10-04, split out of the stall entry**

`j19` and `j56` are the two measured cases: both settled while `s-1791017230755743833#t2` was current,
neither notice is in `#t2`, and neither is in `#t3` either. The daemon's wake submits the notice as a
user item and runs a turn; if the turn has not run when the transcript forks, the queued item goes with
the transcript it was addressed to — and nothing re-arms it against the new one.

**What READING establishes, so the experiment starts where it should** (all cited, no live head needed):

  · **`Harness::wake` is the only road a settlement takes to the model, and it DRAINS before it
    submits** — `jobwatch::JobWatchers::take_completions` (`:303`) is a `g.drain(..).collect()` from
    the shared pen, and `harness.rs:4553` is the `submit_item` that turns it into a row. Nothing
    re-arms the pen, so *drained and not delivered* is a loss with no second chance;
  · **the pen is per TREE and shared** (`Parts::tree_watch`, R58) — which is exactly why a child's
    notice keeps arriving while a job's does not: they are two settlements in ONE pen, and the
    asymmetry the stall entry spent four hours on cannot come from two queues, because there is one;
  · **a compaction does not rebuild the harness at this level**: `Sessions::compact_if_at_the_wall`
    (`sessions.rs:1296`) reads `self.open.get(session_id)` and returns — so a pen held by a live
    `Harness` survives a compaction.

**Which leaves the fork as the suspect and not the compaction**: if the session's harness is
RE-OPENED across a fork under a fresh `Parts`, the pen goes with the old one and an undrained
completion is gone — and that is a question about `Harness::open`'s callers, not about the wake.
**The experiment decides between them**: start a job on an idle session and fork that session before
the notice's turn runs, then look in the NEW transcript. Absent means the pen is per-open and has to
be re-armed at the fork; present means the loss is inside the wake itself and the drain above is
where to look.

**The shape to build:** the queue is the session's and the transcript is the conversation's, so a fork
has to either flush the queue first or carry it across, and the daemon already knows the moment (it is
the same `auto_compact`/`compacted` pair the head marks its echoes with — see
`a_compaction_resolves_the_echoes_it_supersedes`). **The falsifiable form:** start a job on an idle
session, fork it (a `/compact`) before the notice's turn runs, and look for the notice in the NEW
transcript. Present means this entry is wrong too, and the answer is somewhere in the wake's own
eligibility rule — which is where the previous four readings were looking.

**still open?** the query above, grouped by `transcript_id` — a reading that does not do that cannot
see a fork, and this entry is the fifth measurement to prove it.

**done when** a settlement whose turn has not run yet is either delivered after the fork or reported as
lost, in both cases with the transcript it belongs to named.

## Subagents launched in ONE ROUND share a single id — **CLOSED 2026-10-04: `79d1165` mints a unique id at creation**

Three `task` launches in a single round (counting lines of `panes.lisp`, `cards.lisp`, `chrome.lisp`) came
back with the **same** subagent id — `s-1791017230755743833-sub-1791119445423`, three times over. What that
costs, all measured on leticl's head at 15:10:

  · the head held **seven `Subagent` events, every one under that one `:subagent-id`** (states mixed
    `running` / `done` / `failed`), so any fold keyed on the id collapses three children into one row whose
    state is whichever event arrived last — leticl's fold does this, and so does letibot's own
    (`tui/src/app.rs:1846`), so the reference is not better off;
  · the running count drawn on the box's top edge therefore read **no subagents at all while three were
    live** (`composer-title` counts `(subagent-rows head)` filtered to `running`);
  · `task_result` listing this session's subagents prints three identical lines — so they cannot be
    addressed individually there either, and a `[task]` notice naming that id names three children at once.

**The fix belongs at creation**: a per-subagent id (counter or random suffix) rather than something derived
from the round's timestamp. A smarter fold downstream is not the fix — leticl keys on the id because the id
is what the wire offers, and guessing a better key here would paper over the wire.

**And it costs ANSWERS, not just rows.** Measured at 15:12, when the round's notices were released: three
children produced a single `[task]` line — `- `s-1791017230755743833-sub-1791119445423` done: 3529` — so
the model is told that one child finished and receives one answer. **The other two children's answers are
never delivered at all**, to the model or to any head. Per-subagent ids fix the pane and the notices
together, which is one more reason not to patch the fold instead.

**VERIFIED ON THE GLASS 2026-10-04, after `79d1165`** (leticl head `1776144`): a second round of three children
came back with three ids (`…146225190`, `…146494815`, `…146744031`); the head held eleven `Subagent` events
across them rather than one collapsed row; the box's top edge drew **`1 subagent running · 2 jobs running`**
while one child and two jobs were live; and the completion notice named **each child with its own id and its
own answer** (`done: 2633`, `done: 2201`, `done: 2904`), drawn as a card with each answer on its own row.
`2201` is `src/head.lisp`'s line count — the same number a child asked that question returned before the fix
— so the separated answers are the right ones, not merely distinct.

**Closed by `79d1165`**, and the fix is the one this entry asked for, one prefix along: the id is minted
from `crate::config::now_ns()` — the entropy this daemon already names its own sessions with — followed by
`server.rs::mint_session_id`'s counter loop, so the same-shape collision is answered where every other id
in the tree already answers it. **Two checks, because the check this entry first implies is the wrong
one**: a registry lookup alone still lets three children in a round share an id, since the mint returns
while `adopt` is still inside the child's thread opening a whole harness. So the caller also carries what
it has handed out, and the two together are what `three_children_minted_at_one_instant_get_three_ids`
drives at one nanosecond. The timestamp stays in front because it is what stops a post-restart child from
colliding with a persisted one — a bare counter resets and `adopt` would RESUME the old child rather than
spawning a new one. And a duplicate that ever does happen is no longer reported as the child failing: it
names the id, says the mint is what is wrong, and says what it costs.

## A 429 that names money was retried six times — and the interrupt did not reach the retry — **MEASURED 2026-10-04 20:43**

The operator, on a session they had just put on `glm/glm-5.3` with `/model glm/glm-5.3`:

    · model_endpoint_retry — the model endpoint at open.bigmodel.cn did not answer:
      http 429: 余额不足或无可用资源包,请充值。. Taking this round again in 1s (attempt 1 of 6).
      Nothing was recorded, so the retry sends exactly the bytes this one did.
    coulnt interrupt too

**THE BILLING HALF IS FIXED — `http_retry_after` now reads the body.** It keyed on the status alone
(`matches!(code, 408 | 429) || *code >= 500`), so every 429 was "not yet" — and a 429 is also the status
a provider uses when the account behind the key cannot pay. The body says which: a refusal naming money
is terminal and is no longer taken again, and a rate limit still is (the assertion the fix would be
worthless without). The sentence was wrong twice over, which is the same shape as `retry_host`'s note one
function below: **the endpoint did answer**, and what it said was actionable — 请充值 is a thing to do,
and "did not answer" sends the reader to a network problem.

**THE INTERRUPT HALF IS OPEN, and it is the half the operator actually complained about.** Two candidates,
and this needs a live head to separate them:

  * **the daemon cannot hear it while it waits.** The retry sleeps in `sleep_unless_closed`
    (`harness.rs:6429`), which watches **only `self.hub.is_closed()`** — a daemon shutdown, not a
    person. The one channel an interrupt rides is the hub's steering command queue, which the loop
    reads at the top of the NEXT iteration (`let mut steering = self.steering()`), into a round that
    hits the same 429 within milliseconds: so the interrupt is either honoured there or consumed by a
    round that fails before it can matter;
  * **the head never sent it.** Esc-esc to `Action::Interrupt` is gated on `turn_busy()`, and during a
    retry the round's own `TurnFinished` may already have been published — the exact gap R51 item 16
    fixed for a running *call*, one state further along.

**The measurement that separates them**: with the endpoint refusing, watch the daemon's log for the
`interrupt` line (`hub.rs:1079`) while pressing esc-esc. Present means the daemon heard it and the sleep
ignored it — and then the fix is that the wait has to watch the same door the tool-call path watches.
Absent means the head's own gate, and the fix is in `turn_busy`.

**still open?** the screen above, on a build with the classifier fix: a billing 429 now fails the turn
once and says whose money it is; whether the interrupt lands during a *transient* wait is still open.

**done when** a person who presses interrupt during a retry's wait gets the turn back, however the
retry was classified.

## R18 — every hand-rolled lexer replaced by rano + tree-sitter — **given 2026-09-20**

> lets extend todo with this task - completely replace handrolled code with rano and
> treesitter. I really want us to rely on rano as much as possible, you are free to go
> and patch/extend it too

Rano is the hub: `~/Projects/rano/rano`, a path dependency of `crates/ui` and
`crates/tui`. It owns the tree-sitter engine — 28 languages, one capture walk, the
palette left to the caller. The rule stated in `crates/ui/Cargo.toml:27-34` is that a
**second tree-sitter integration in this process is the thing rano exists to prevent**.
There are three. Surveyed 2026-09-20:

| hand-rolled | where | size | what rano has instead |
|---|---|---|---|
| `StreamingCode` | `crates/ui/src/highlight.rs` | 699 lines, **10 languages** | `Stream` + `captures`, 28 languages |
| its own grammar set | `crates/code/Cargo.toml:20-26` | tree-sitter 0.27 + **6 grammars** | the same tree-sitter, 28 grammars |
| `scan_script` | `crates/tools/src/intent.rs:3390` | a hand-written shell scan | `crates/code::shell::shape` |

Everything below `crates/tui/src/markdown.rs`'s projection is already rano's (the block
pass, the inline pass, the `Node` tree) — `docs/streaming-markdown-plan.md` is the
record. What that file keeps by hand is **not** in scope: `fences_in`, `mask`,
`cut_point`, `stable_boundary_with`, `list_kind`, `unescape`. Each exists because the
grammar gets something wrong (the closing fence is not line-anchored; a loose list
cannot be settled by the tree) and finding it in the text *is* the fix. Deleting those
would reintroduce the bugs of 2026-09-20.

### R18.1 — `StreamingCode` goes, and the conversation gets 28 languages — **DONE 2026-09-20 (`50b005c` + rano `e58600c`)**

The visible win: a code fence is coloured by a 10-language hand-written lexer today, so a
fence tagged `tsx`, `lua`, `ruby`, `diff` and eighteen others renders plain. Rano knows
them all.

Done: `crates/ui/src/highlight.rs` 699 → 118 lines, holding `role_for_capture` and
nothing else; `sidediff` shares that table instead of keeping its own; `CodePaint` is a
rano `Stream` + `Stream::spans`; `BlockCache`'s instrument is `parses` rather than
`bytes_highlighted`. Three rano additions landed with it — `Lang::from_token`,
`Lang::name`, `Stream::spans` — and the walk got 1.5–2.5× cheaper on the way
(`for_each_capture` no longer builds a `Vec<char>` and a char→byte map per line).
Measured: ~470 µs per push at the end of a 5.4 KB Rust fence, a frame's budget. What is
still open is in rano's `TODO.md` §9: the walk is O(text), which is a fence's budget and
not an editor's whole-file repaint.

**The trade, and measure it before deleting anything.** `StreamingCode`'s guarantee is
that *a complete line is highlighted exactly once, ever* — rano cannot promise that, and
rano measured why: markdown's push re-lexes the whole document (~106 ns/byte) and rust's
reuses ~97% (~3.5 ns/byte). At the last figure a 10 KB fence costs ~35 µs per push, which
is a frame's worth of nothing; at markdown's it would not be. So the number to take is
rano's `bytes_reparsed`-equivalent for a rust/go/ts fence grown one token at a time, and
the decision follows it. If it is not flat enough, the fence gets the same window
discipline the conversation has.

**Where.** `CodePaint` (`crates/tui/src/render.rs:176`) holds the `StreamingCode` and the
byte offset it has been fed; `render_block_with`'s `Block::Code` arm draws what it
returns. It becomes a rano `Stream` plus a capture walk over the fence's text. Delete
`highlight.rs`'s lexer (`Syntax`, `State`, `StreamingCode`, the 10-language table) once
nothing calls it, and its `bytes_highlighted` instrument with it — the number it exists
for has a replacement in rano.

### R18.2 — `crates/code` stops carrying its own tree-sitter — **DONE 2026-09-20**

`crates/code` was a second integration: its own `tree-sitter = "0.27"` and six grammars
(rust, python, go, c, bash, json), used for `outline` (`crates/tools`) and the shell shape
(`crates/harnessd`'s etalon map). All six are rano's already.

Done: `Cargo.toml` drops the seven dependencies for one `rano` path dep; `outline` and
`shell::normalise` drive `Stream` and walk rano's `Node`; `examples/dump.rs` — the tool
that prints a grammar's node skeleton, and how every kind string in `classify` was chosen
— reads the same `Node`, so what it prints is what the outline sees.

**What it is not**: a merge of the two crates. `crates/code` exists because the grammars
are C and pull a `cc` build, and `crates/tools` deliberately has no `build.rs` — that
reason stands, and this crate is still where it lives. What went was the second engine
underneath it.

**Rano grew four things to make it possible**, all of them "the hub's node should carry
what its consumers need": `Node::field` (which field a child sits in — 17
`child_by_field_name` call sites here), `Node::start_point`/`end_point` as rano's own
`Point`, `Node::id` (two nodes can share a byte range, so identity is not a coordinate),
and rano's `Point` no longer being tree-sitter's in its public API.

**One bug this found, in this crate and not in the port.** `shell.rs`'s `named_children`
was misnamed — it collected *every* child, anonymous tokens included. A reader taking the
name at its word filtered on `Node::named` and lost the `&&`, `|` and `;` the module
decides *by*: `test -f x && rm x` came back `Certainty::Always` for `rm` instead of
`Conditional`. Renamed `kids` with the misnomer written down, because an operator token is
exactly the kind of child a grammar leaves anonymous.

### R18.3 — the intent scanner's shell scan — **CLOSED 2026-09-20: it stays, and not as a duplicate**

`crates/tools/src/intent.rs::scan_script` is read at the call sites rather than guessed at,
and it does not read shell at all: `ScriptLang::Shell` bodies already go through
`shell::normalise` and the grammar. This handles `ScriptLang::Other` — a **script in
another language**, named by its interpreter (`python`, `node`, `perl`, `ruby`, `php`),
which a bash grammar would read wrongly and no single rano grammar covers.

Its question is also a different one: not what the shell will *do*, but which
**capabilities** the text names — a network import, a shell-out, a host, a secret path —
deliberately **body-wide rather than adjacency-based**, because `host = "192.0.2.10"` three
lines above `urlopen(f"http://{host}/")` is a real shape. A parse does not change that.

What a parse *would* improve is precision (`import urllib.request` names its module in a
node, where this matches a name list against a whole line) — noted in the function's own
doc rather than done, because it changes what a gate decides and that is not a refactor.
So: no grammar here, and the reason is recorded where the next reader asks.

### Rano patches this needed — **all three landed** (`e58600c`)

- **`Lang::from_token(&str)`** — done, with its own alias table rather than `detect`'s
  (`mk` is Make as an extension and not a token; `sh` is bash as a token and `/bin/sh` as
  a path).
- **A route from `Lang` to its highlight query** — done as `Stream::spans`, which is
  better than making `query()` public: the embedder hands over *nothing* and gets capture
  names back, so no query text crosses the boundary at all. `classes()` was written first
  and removed — see rano's commit.
- **`Stream` over a growing text** — it already existed; the fence feeds it the delta.

**Still open?** Nothing — all three subtasks are closed. What *was* the check, for the
record, is now the answer:

    grep -c 'name: "' crates/ui/src/highlight.rs        # 0  (was 10)
    grep -c '^tree-sitter-' crates/code/Cargo.toml      # 0  (was 6)
    ls crates/tools/build.rs                             # absent, as it must stay

**Done when.** A fence tagged `tsx` (or `lua`, `ruby`, `diff`) renders coloured in a live
answer — the rendering is pinned by tests in `crates/tui/src/render.rs`, and the one thing
a test cannot assert is a live answer; `crates/ui/src/highlight.rs` holds no lexer;
`crates/code` declares no `tree-sitter-*` of its own and `crates/tools` still has no
`build.rs`; the conversation's per-push cost is measured against the old one and the number
is in rano's `TODO.md` §9; 2026-09-20's markdown fixtures pass, since they are the record of
what the grammar gets wrong.

### R18.4 — attaching to a big session takes seconds, and it is the markdown parse

**Reported 2026-09-20**, after the migration shipped: *"startup time skyrocketed.
literally seconds"*, and then *"I do `leticode --continue` and it just does nothing, then
chrome appears with empty conversation history and then after a while it renders
history"*.

**Measured**, release, on this workspace's own stored transcripts:

| | |
|---|---|
| markdown block grammar | 133 ns/byte (555 KB → 74 ms) |
| markdown inline grammar | 250 ns/byte (450 KB → 133 ms) |
| Rust, for scale | ~3 ns/byte |
| a stored session | **4.1 MB and 5.9 MB** (`s-…838#t18`, `s-…813#t11`) |
| so one attach | **1.6-2.7 s** of lexing, which is the "after a while" |

Both markdown grammars run external scanners and that is the constant; tree-sitter is
not the problem. A row is lexed once (the walk is monotonic — see
`rendering_the_history_does_not_grow_with_the_session_either`), so this is one linear
pass, not a repeat.

**Fixed (`5884ddf`, rano)**: the query cache. `Query::new` is 8.7-10.2 ms and was paid
per *code fence* and per *diff excerpt*; it is compiled once per process per language
now. This session alone holds 132 fenced messages, so that was ~1.2 s of pure query
compilation per attach — our bug, and it is gone.

**Chrome first (done 2026-09-20).** The head used to draw its first frame *after*
the whole attach — and `HeadClient::attach` blocks on the daemon's `Hello`, which
**carries the snapshot**. So on a big session the operator's previous screen stayed
up for the entire round trip and then the transcript appeared at once, which is
exactly the reported *"does nothing, then chrome appears with empty conversation
history and then after a while it renders history"*.

Measured on this daemon: 0.01 s of process start, 0.17 s of attach (the `Hello`,
snapshot included), 0.08 s of first frame. Only the first of those was ever visible
as *letibot*.

Now the terminal is taken over and a frame is drawn **before** the attach, so the
composer and the hint bar are on screen in the time the process takes to start, and
the history fills in when the daemon answers. The frame drawn in the meantime must
not lie: the empty-transcript banner says *"this session has said nothing yet"*,
which is false when the truth is *"nobody has told this head yet"*, so `App` carries
an `attaching` flag that suppresses it — `the_frame_before_the_attach_claims_nothing_about_the_session`.

`Terminal`'s `Drop` restores the termios and leaves the alternate screen, and it does
so before printing its own reports, so entering the screen before a refused attach
still lands the error on a restored terminal.

**The design, and it is the operator's rather than mine.** Stated twice, and I said
yes to it both times and then planned as though I had not:

> so in a way we are looking back right, instead of looking in the future, interesting
> challenge for treesitter  — 2026-09-20, on the tail problem

> render chrome asap, then render the very current frame of text, then some scroll up
> buffer if needed — mind the bounds of blocks, etc  — earlier the same day

So: **stream backwards.** The document is finished and the head wants its *end* — the
current frame, then scrollback above it, fetched as it is looked at. That is a viewport
over a buffer the daemon already owns, which is what an editor has, and it is *not* the
three-patch list a first pass at this item produced (lex less, cache the render, fill
over frames). Those were written after `tail_cut` was built from the operator's own
insight and then not wired up — the mechanism for this design already exists in
`crates/tui/src/markdown.rs` and every one of those three would have worked around it.

**Stage 1 — the window is a window (no protocol change).**
`tail_cut(src, min_bytes)` returns the offset near the end from which the suffix parses
standalone. Committed and tested (`815200e`, `ad0a10a`): `lex(tail)[1..]` is the
document's blocks, the first may be ragged and goes off the top, and a 4 KB tail of
152 KB measures **3.0%** of the whole's cost. What is missing is only the caller: the
walk in `body_window` still lexes each row whole.

**Stage 2 — a row is a logical string, and the head holds a viewport.**
The operator's own framing, and it is the right one: a tool result of 418 KB is a
*logical* string that wraps to thousands of *display* lines, of which 40 are on screen.
Wrapping exists; paging does not, so an unfolded row is unreachable past the fold. The
fix is the editor's, not a cap: the daemon owns the buffer, the head holds the window,
and a row fetched on demand when it is unfolded. `Peek` already fetches a stored
conversation for the session picker, so the mechanism is built — it needs a row-shaped
form rather than a new protocol.

**Stage 3 — bound the snapshot in bytes (`ViewBounds`).**
`items: 2_000` bounds *count*, and the operator has sessions of **160 MB and thousands
of turns** where a row may be 418 KB — so the snapshot can be most of a gigabyte. This
is the first half of the viewport rather than an optimisation: without it the window has
nothing to be a window *over*. Do it first only in the sense that it is small and needs
no protocol change; on its own it just drops old rows and still ships the fat one.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano. Nor is capping a row's payload at the source: that cuts the logical
string to fix a display problem, which the operator named as the editor mistake.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano.

**The one piece left, and it is rano's.** The capture walk is O(text) per repaint. A code
fence is a frame's budget at fence sizes — measured, and why this shipped — but an editor
repainting a 200 KB file per keystroke is not, and that is exactly the consumer rano's
README says it is for. rano's `TODO.md` §9 has it as open with the shape of the fix
(a range-limited `QueryCursor` widened back to any overlapping node, which is where the
correctness lives). It is not filed here as well, because it is one item and it belongs to
the engine.

---

## R19 — stream backwards: the head holds a window, not a buffer — **given 2026-09-20**

> so in a way we are looking back right, instead of looking in the future, interesting
> challenge for treesitter

> render chrome asap, then render the very current frame of text, then some scroll up
> buffer if needed — mind the bounds of blocks, etc

The implementation of R18.4's design, which is the operator's and was written down there
only after a first pass around it. Three stages, in order, and the first is head-side
only with no protocol change.

### R19.1 — wire `tail_cut` into the walk

`crates/tui/src/markdown.rs::tail_cut(src, min_bytes)` is committed and tested
(`815200e`, `ad0a10a`) and has **no caller**. `lex(tail)[1..]` is the document's own
blocks — exact below a possibly-ragged first block that goes off the top — and a 4 KB
tail of 152 KB measures **3.0%** of the whole's cost.

`App::body_window` still lexes every row in full from row 0, then slices the bottom
`room` rows out of `segs`. The change: render the **tail** of the conversation, from the
end, until the window is covered — the current frame first. `retarget_before(k)` already
scans backward for the per-row call context, and the other per-row inputs (`edit`,
`decision`, `elapsed_ms`, `answered`) are maps keyed by item id, so nothing about them
depends on direction. The one fiddly part is `hist_marks`, which is dense and indexed by
absolute row; a tail walk wants a floor and an "N lines above" count instead.

**Measured, and it rules out one wrong idea**: the forward walk is *not* the cost. A cold
frame over 6000 rows is 41 ms and every frame after is 0.06 ms. So this is not
"the walk is slow" — it is that a 160 MB session's *lex* is, and a tail avoids it.

### R19.2 — a row is a logical string; the head holds a viewport — **DONE 2026-09-20 (the paging half)**

**Done**: a payload past its first screenful is now reachable. `ctrl-t` opens the fold
*and* a view on the newest payload row; `↑`/`↓` page inside it; the seam says which key
does what (`… +N lines · ↓ pages down · esc closes`, and `… end of output` at the end);
`esc` closes the view and gives the arrows back to the transcript.

**The bug was that `ctrl-t` revealed nothing.** It raised the *budget* — how many rows a
card may draw — and there was **no offset**. So a 418 KB payload drew its head, said
`… +N lines · ctrl-t`, and the chord showed you none of them. The rest was unreachable.

Three things this needed, each found by a test rather than reasoned out:

- **The view is keyed on the item id, not the call id.** The first version keyed it on
  the call id, which `item_lines` does not hold — so the view was silently closed and the
  seam went on saying `ctrl-t pages` while `ctrl-t` had been pressed.
- **A page move must invalidate the history buffer.** `hist_lines` is a cache of rendered
  rows and a page offset changes what one of them renders to, so `redraw` alone
  re-drew the old lines: the page said 10 and the screen said line 0.
- **`Up` at the top is a no-op, not a bug.** A test asserting "the arrows page" with `Up`
  was wrong, not the code.

**A contract that changed, and it is a real one**: while the view is open the arrows page
it rather than scrolling the transcript. `scroll_still_works_after_ctrl_t` — written for
the mouse-reporting bug — now asserts the *new* contract explicitly, including that `esc`
gives the arrows back. A view that held them for ever would be the same defect with a new
cause.

**Still open** — two layers, and they are one design: *the head must not hold what it is not
looking at.*

**Measured 2026-09-20** (400 messages, 80×24, scrolled to the top): 23 KB of source becomes
**67 KB of rendered lines** — wrapping and escape codes make rendering about **3×** the source
— and `hist_lines` holds every line from the floor down, **for ever**. It never shrinks. So the
coarse bound is `3 × ViewBounds::item_bytes` ≈ 24 MB today, and the moment anything can fetch
rows on demand it becomes unbounded: scrolling through a 160 MB conversation would render all
of it and keep all of it.

**First, a correction about what "dropped" means, because an earlier version of this item got it
wrong in a way that changes the design.** There are not one but **two rings in the daemon's
memory**, with two different bounds, plus the disk:

| where | holds | bound | who reads it |
|---|---|---|---|
| **view** (`ViewBounds`) | materialised rows, bodies included | 2,000 rows / 8 MiB | the `Hello` snapshot |
| **log** (`LogBounds`) | *every event*, including `TranscriptContent` — **which carries the body** | 20,000 events / 16 MiB | **`Peek`**, and a resume gap |
| store | every row, forever | disk | resume, `--replay` |

So a row trimmed from the **view** may still be whole in the **log**, and `Peek` already reads
the log (`hub.retained()`) — the mechanism exists and is wired. `FetchRow`'s `None` for a trimmed
row is therefore **not necessarily final**; the parts below say what to try before reaching for
the disk. A row costs several events (`TranscriptAppended`, `TranscriptContent`, and for a call
`ToolStarted`/`ToolProgress`/`ToolFinished`), so the log's window is a few thousand rows rather
than 20,000 — larger than the view's 2,000, not unlimited.

**(a) A ring over rendered lines — the head holds a window, not a history.** `hist_lines` is a
`Vec<String>` indexed by *absolute line* (`take_window` slices `[start, end)` across the
concatenation of segs, with `hist_lines` as seg 0). Two things make this more than a `VecDeque`
swap, and both were found by trying to write it down:

- **It only makes sense in tail mode.** In an ordinary session `hist_floor == 0` and the window
  *is* the history; a session small enough to walk fully is small enough to hold. So the ring
  is a tail-mode mechanism, which keeps it away from `hist_marks` — the forward walk's rewind
  index, which is empty exactly when tail mode is on.
- **Eviction needs a per-row line count that tail mode does not keep.** Dropping whole rows from
  the front means knowing how many *lines* they became, and `hist_marks` is the only row→line map
  — empty in tail mode. So the ring needs one small parallel structure of its own: the line
  count per rendered row of the window. Then `hist_line_base` (lines evicted above) and its
  counterpart (lines evicted below, which must still count towards `total`, or the scroll
  position jumps when a row is re-rendered).

With those, eviction is at both ends around the viewport, refilled by `fill_backward` above and a
matching `fill_forward` below — both the same shape as the existing one, since rendering a row is
`item_lines` and nothing else. **This half pays for itself with no protocol traffic at all**: the
head can re-render any row it holds, so a ring costs a re-render and saves the memory.

**(b) `FetchRow`, and the daemon is a *proxy*.** The operator's framing, and it is the right
one: *"it is a cache problem - the daemon is optimized for normal usecase - heads show the latest
+ some scroll back. if a head wants something in the way past - daemon is simply a proxy from the
store to the head."*

So the two in-memory rings are not tiers a head navigates — they are **a cache tuned for the
normal case**, and a request outside them is an ordinary cache miss that the daemon resolves from
the store. The head asks **once**, by ordinal, and never learns which of the three answered. That
is why the frame takes an ordinal and not a tier, and why `body: None` means *the row does not
exist* rather than *ask somewhere else*.

This also replaces the "three steps" an earlier draft of this item had, which had the head
reasoning about log ring versus store. It does not: resolution belongs behind the frame.

**What is still missing:**

1. **The head does not ask yet** — no flag for "this ordinal is missing", no tracking of an
   answer, and no partial-body render. Blocking, and independent of where the answer comes from.
2. **The daemon does not proxy yet.** `row_body_at` answers from the view and returns `None` on a
   miss (`crates/sessionlog/src/view.rs`), so today a trimmed row answers `None` even though the
   log ring — and the store — still have it. Resolution wants to be one method: view, then log
   ring, then the store through
   [`SessionSource`](../../crates/sessionlog/src/registry.rs), the trait that already exists for
   exactly this reason (*"A **trait and not a `Store`**… the daemon passes an implementation in"*),
   implemented as `StoreSessions` in `crates/harnessd/src/sessions.rs`.

**What the store path has to answer**, recorded because it would otherwise be found by a row
landing in the wrong place: **a session is not one table of rows.** Resume forks make the rows a
*chain* of transcripts — `crates/harnessd/src/transcript_source.rs:129` walks `transcript` ordered
by `created_at` rather than querying `transcript_item` directly — so the ordinal the head counts
must be the sequence that chain produces.

**And the head's window and the daemon's ring are allowed to disagree** — the operator's call:
*"regarding different heads that can display disjoint sets - it is their problem essentially"*.
The daemon serves rows **by session ordinal**, and that ordinal is a property of the session, not
of any head's buffer. Whether a given head can render what it asked for is the head's business: a
head holding its own rendered window, a head showing forty rows, a head that never scrolls — the
daemon satisfies none of them individually and all of them by answering "row N". So there is no
reconciliation to design between `ViewBounds`, `LogBounds` and the head's ring; they are three
different windows over one sequence, and only the sequence is shared.

That is what makes the pieces independent rather than a single design:

| piece | whose | needs |
|---|---|---|
| a ring over **rendered lines** | the head | nothing — stop holding every line it has drawn |
| asking for a **missing ordinal** | the head | `items_dropped` plus its own index; no flag on the wire |
| answering **by ordinal** | the daemon | the store read, behind the `SessionSource` seam |
| heads with disjoint sets | theirs | — |

**Still to write, in this order** — the order changed once the two rings above were separated out:

1. **The head does not ask yet.** It has no flag for "this ordinal is missing", no tracking of an
   answer, and no partial-body render. This is the blocking piece; the two below are about where
   the answer comes from.
2. **A row trimmed from the *view* is probably still in the log.** `Peek` already reads it, and
   `TranscriptContent` carries the body. So the cheap next step is not a new store read but a
   **log-ring read shaped like `FetchRow`** — same ordinal, same window, one source. Worth
   measuring first how many rows the log's 20,000 events actually cover, since a row costs several
   events and the honest answer may be "a few thousand more than the view, not a different order".
3. **Only past both rings does the disk matter** — and the seam it needs **already exists**.
   `SessionSource` (`crates/sessionlog/src/registry.rs:311`) is a trait `sessionlog` defines and
   `harnessd` implements as `StoreSessions` (`crates/harnessd/src/sessions.rs:1809`), introduced
   for exactly this reason: *"A **trait and not a `Store`**: `letibot-sessionlog` does not depend
   on `letibot-tokencore` and should not start… the daemon passes an implementation in, and a
   registry with no source behaves exactly as it did before."* So a `row_body(session_id, ordinal)`
   method on that trait is the whole of step 3, with no new direction of dependency —
   `crates/harnessd/src/transcript_source.rs` already reads the rows in order
   (`SELECT … FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC`).

   **What step 3 does have to answer** is that a session is not one table of rows: resume forks
   mean the rows reaching the head are a **chain** of transcripts, which is why
   `crates/harnessd/src/transcript_source.rs:129` walks `SELECT … FROM transcript … WHERE
   t.session_id = ?1 ORDER BY t.created_at DESC` rather than querying items directly. The ordinal
   the head counts must be the same sequence the chain produces, or a fetched row lands in the
   wrong place.

**(a) is the one that matters first**, and it is also the smaller: it needs no daemon change, no
store read and no protocol, and it is what makes (b) safe to add — without it, giving the head
access to more rows would only move the memory problem from the daemon to the head.

**And it is not `Peek`, which an earlier draft of this item said.** `Peek` fetches a whole
session's scrollback and has **no position**: you name a session, you get all of it. That is
right for the picker — "what was that session about" is a whole-thing question — and it is the
wrong shape for this, which is positional. The *pattern* worth reusing is that it reads without
moving your seat; the cursor and the window are new.

**Checked before building, 2026-09-20**: across the whole store there are **8 rows over
64 KB**, and the largest is a `tool_result` (418 KB) — which never goes through the
markdown lex at all, because a tool result is drawn as payload text with its own bound.
The largest *markdown* rows are a 42 KB assistant answer and an 86 KB reasoning block.
At ~450 ns/byte those are ~19 ms and ~39 ms, once each.

So a single huge row is **not** where a 160 MB session's cost lives — it is thousands of
ordinary rows, which R19.1 now renders only the tail of. The stage below stays worth
doing for its own sake (an unfolded row is unreachable past the fold today: `render_bounded`
draws a tail and an elision count, and nothing fetches the rest), but it is a *fidelity*
fix rather than the performance one, and the number that said otherwise was never taken.

### R19.2 (detail) — a row is a logical string; the head holds a viewport

The operator's framing, and it is the correct one: a 418 KB tool result is a **logical**
string that wraps to thousands of **display** lines, of which 40 are on screen. Wrapping
exists (61 call sites); **paging** does not, so an unfolded row is unreachable past the
fold.

So the fix is the editor's, not a cap: the daemon owns the buffer, the head holds the
window, and a row is fetched when it is unfolded. `Peek` already fetches a stored
conversation for the session picker — this needs a row-shaped form of it rather than a
new mechanism.

**Not the fix**: capping a row's payload at the source. That cuts a logical string to
solve a display problem, which is the editor mistake. A `read` of a large file, a build
log: the bytes are the truth and the screen shows 40 lines of them.

### R19.3 — bound the snapshot in bytes — **DONE 2026-09-20**

`ViewBounds::items: 2_000` bounded a **count**, and the operator has sessions of **160 MB
and thousands of turns** where one row may be 418 KB — so the daemon's snapshot could be
most of a gigabyte, cloned per attach and sent over the socket.

Done: `ViewBounds::item_bytes` (default 8 MB) beside the count, and one `trim()` that
both bounds run through. Three things the implementation needed that a one-liner would
have missed:

- **Two places can go over, not one.** A row is *announced* before it is filled, so the
  count is checked at the announcement and the **bytes at the fill** — a row that arrives
  empty and lands 418 KB later blows the bound the moment it lands, and trimming only on
  the next announcement would leave the snapshot oversized until one came.
- **The newest row is kept whatever its size.** A view with nothing in it cannot be read
  or scrolled, and a bound that emptied the transcript would break the thing it exists to
  protect.
- **`TranscriptItem::bytes` is the one definition** of "how big is this row", in
  `letibot-transcript` where both sides can see it — the daemon bounds by it and the head
  decides whether a transcript is too big to walk by it. The head had its own copy before
  this, which is exactly the drift the shared one prevents.

`items_dropped` reports the trim, so a head can say "N rows above" honestly.

**Done when.** Attaching to a 160 MB session draws the current frame at the same cost as
attaching to a 1 MB one; scroll-up extends it without a full lex; an unfolded 418 KB row
is reachable rather than truncated; and `eval` stays out of it — no logical string is cut
to fit a screen.

---

## R22 — a 1.35M-token conversation compacted to a 2,250-token summary — **OPEN, designed with the operator 2026-09-20**

Measured on the operator's own session, 2026-09-20, `s-1789462738453908838`:

    ! compacted — compacted: 1352917 → 11353 tokens, on transcript …#t22

The base is 11,353 tokens and 9,103 of those are the stable prefix, so **the summary
standing in for 3,590 rows is about 2,250 tokens**. A 600:1 reduction. The conversation
itself is not lost — `#t21` still holds all 3,590 rows and 1,343,686 tokens in the store —
but the live session continued on a base that cannot carry it.

### It was not truncated, and that matters

Nothing caps the output, on either path, and both are deliberate:

* **Local.** `CompletionRequest` has no `n_predict` field at all, and
  `there_is_no_n_predict_cap_anywhere_in_the_body` asserts it. §5.7's argument: *"a cap
  is a truncation you chose"*, written because opencode capped at 32,000 tokens and threw
  the resulting `finish_reason` away.
* **Provider.** `TurnRequest::max_output_tokens` exists and `openai.rs:71` would set
  `max_tokens` from it — but **both call sites pass `None`** (`compaction.rs:273` for the
  summary turn, `harness.rs:4340` for an ordinary one). `max_tokens` is never in the body.

`report.fork.truncated` was false and the `compacted` line carried no CUT OFF clause,
which is the other half of the same evidence. **The model chose to stop.**

### So the lever is the instruction, and the instruction argues for this

`compaction.rs`'s `SUMMARY_INSTRUCTION` is a constant that says *"Write a **compact**
factual record"* and names no budget at all. It was asked to be compact, given no sense of
scale, and obliged. With a 1,440,006-token window and a 9,103-token prefix the summary
could have been 100k+ and still left the session most of its room.

### The design, as the operator framed it

*"we should live overruns as is and for normal cases try to preserve as much as possible
but not more, some fixed ratio that includes total context, our base prompt and desired
fdelity."*

1. **Leave the overrun path alone.** Folding halves is the right answer when the
   conversation does not fit in front of the window. Different failure, different shape.
2. **A budget from a ratio.** Room is `planning_window − prefix − headroom`; the summary
   takes `room × fidelity` and the rest is working span. For the measured session: ~1.34M
   of room, so a fidelity of 0.15 is ~200k ledger ≈ 135k provider tokens, and the session
   still gets 85% of its window before compacting again.
3. **The budget alone will not work.** Telling a model it *may* write 135k tokens does not
   make it write them, and the word "compact" is doing damage no number beside it undoes.
   The instruction has to change character from compression to **enumeration** — one line
   per decision, per file changed, per command and outcome, per open question; do not
   merge entries; do not summarise across items. The budget is permission; the enumeration
   is the mechanism.
4. **A fidelity check, which is the strongest piece and needs no model.** Count the
   distinct file paths, commands and decisions in the conversation, then count how many
   appear in the summary. A summary naming 3 of the 47 files touched has failed, and it
   can be told exactly that and re-asked once. That is a closed loop in
   `docs/closed-loop.md`'s own terms and the same shape as `stall_rounds` and
   `UNFINISHED_REASONING_NOTICE`, both of which work. It is the difference between hoping
   for a good summary and **detecting a bad one**.

### Per-topic budgets, and why not a SOM

The operator: *"I wonder tho, if we can compute some sort of similarity metric on the whole
conversation, like a SOM and then analyzer how many topics, and act accordingly with per
topic budget and maybe a nudge."*

Per-topic budgeting is a good idea — not every thread in a long conversation deserves equal
compression, and a dead one should not cost what a live one does. **Derive the topics from
structure rather than from geometry.** That transcript is 3,590 rows and **82 of them are
user turns**: the operator's own messages are where topics change, and segmenting there is
free, deterministic and auditable. The todo list is a second declared decomposition,
already in the store.

Three objections to the SOM, in order of weight:

1. **It is not explainable, and this codebase is built on explaining.** `Prereq::how()`,
   `Disclosure` and `progress::evidence` all say *why*. "The map says 7 topics" is exactly
   the unarguable number the rest of the system refuses — and when a compaction drops
   something that was needed, there would be no way to ask why.
2. **It would be tuned to produce a number that could have been chosen.** Cluster count is
   a hyperparameter; the map depends on initialisation and schedule.
3. **There is no embedding path in the tree**, so it is a new model dependency in a path
   that has to work when the main model is metered and remote.

Keep it as a research question off the critical path. If structural boundaries prove too
coarse, it earns its own experiment then.

**Order.** (2) + (3) + (4) first — contained, and expected to be most of the win. Per-topic
budgets on structural boundaries second, once it is visible whether flat budgeting was
already enough.

**Still open?** `grep -n 'compact factual record' crates/turn/src/compaction.rs` — if
`SUMMARY_INSTRUCTION` still names no budget, this is open.

**Done when.** A compaction of a conversation near the window produces a summary whose size
is a stated fraction of the room rather than the model's guess; a summary that omits most
of the files the conversation touched is detected and re-asked once, with the gap named;
and the overrun path is untouched.

---

## R21 — the jobs pane cannot show a job's output, and the reply path is why — **DONE 2026-09-20 (`3aabe4f`)**

The operator, 2026-09-20: *"when i press enter on jobs pane im not shown the job output im brought
back to the main conversation with /job <id> posted - this is not what i want - when i press enter
i want to see job output"*. Narrowed by them a moment later: *"entering the running job works fine
- but finished does /job <id>"*.

**One path serves both, and nothing about running versus finished differs in the code** — so the
"works fine" on a running job is the reply being one *sentence* (`j12 is still running and has
written nothing yet`), which reads acceptably as a line in the chat, while a finished job's reply
is a 16 KB dump that does not. The pane closes in both cases, deliberately.

### What the reply is, and why the pane has to close

`Action::Slash { line: "job j12" }` → `ClientFrame::Slash` → `CommandKind::Slash` (the command
queue) → `Sessions::slash_parsed` → `Harness::job_output(job, offset, 16 KiB)`, and the answer is
published as

    SessionEvent::Warning { code: "slash", detail: "…/job j12\n<the output>" }

— `crates/harnessd/src/sessions.rs:1546`. So a slash reply is a **warning on the session log**,
which is the design (*"a frame per verb would have every head learn every verb"*), and the head
closes the pane so the operator can read it there.

### The two shortcuts that do not work, both checked

1. **Match the warning in the head and draw it in the pane.** The head does know it asked for
   `/job j12`, and the reply's first line is that exact string — but the event is still published
   to the session, so the 16 KB would appear *both* in the pane and in the conversation. Hiding it
   in this head would make the head's screen disagree with the log every other head sees.
2. **`ClientFrame::ReadJobOutput` answered on the asking connection.** This is the shape
   `Peek`/`Settings`/`Jobs` use, and it is what a pane wants — but those all read something the
   **server** can reach (the hub's view, a registry mailbox), and job output lives in the **exec
   host**, which is the daemon worker's. The server would have to register a reply channel, queue a
   command, and block its own read loop on the answer — which stalls that connection's live events
   for the duration. The `secrets` map is that pattern and it is the only one: a reply channel the
   worker fills.

### The design that fits, and it needs no protocol change at all

**Publish the window as its own `SessionEvent`**, and let the pane be a view of it:

    SessionEvent::JobOutput { job, from, to, produced, dropped, state, lines, next }

- It is a **slash reply**, so the log is exactly where it belongs by the existing rule — no lie,
  no suppression, every head sees it.
- It carries the **offsets beside the text**, which the `Warning` prose cannot: today
  `Harness::job_output` builds a footer (`[exited 0 — bytes 0..16384 of 40000 produced]`, `more:
  /job j12 --offset 16384`) that a pane would have to parse. The window type and that method's
  structured twin were written and are small.
- The head needs **no new frame and no reply channel**: `CommandKind` + the fan-out already carry
  it, and the head's `ServerEvent` arm fills a `job_out` pane that mirrors `sub_out`.
- Paging is the same event again: the answer carries `next`, the offset to ask at when the ring
  still holds more, and the head asks a `ReadJobOutput` at it. The head keeps a stack of the
  offsets it was given rather than the page size, because the page size is the daemon's.
- **No `PROTOCOL_VERSION` bump**: `SessionEvent` is an internally-tagged enum and this is an
  additive variant, the same way `TranscriptContent` and `JobSettled` were added.

**Where.** `crates/sessionlog/src/event.rs` (the variant), `crates/sessionlog/src/hub.rs`
(`CommandKind::ReadJobOutput`), `crates/harnessd/src/harness.rs` (`job_output_window`, structured),
`crates/harnessd/src/sessions.rs` (the dispatch arm publishing it), `crates/tui/src/app.rs`
(`job_out`, the Enter arm, the render, the event arm), and tests on both sides.

**Done when.** Enter on a job row — running or finished — draws its output in the pane, esc returns
to the list, `↓` pages when there is more and says so when there is not, and the conversation gains
nothing the operator did not ask to put there.

**Done.** Implemented in `3aabe4f`, and one thing is different from the design above: paging is
**not** a slash reply. Once the window is a `ReadJobOutput` there is no reason to route "the next
page" back through the verb, so the event carries `next` and the head asks a second
`ReadJobOutput` at it — the head keeps the offsets (`←` walks back the way `→` came) rather than
recomputing a window it does not size. One thing was also **added** that the design did not name:
the window is ephemeral, so `scrub::is_interactive` returns true and `StoredProjection::keep`
strips it and counts it — a window from four minutes ago is a lie about now.

---

## R20 — the header names the wrong model, and the diff toggle is undiscoverable

Two reportable things from one message (*"when i start leticode - despite the fact that
the model is you - deepseek, it still shows qwen. also how to switch diff style?"*).

### R20.1 — the header says `qwen-3.8-27b` while the turns go to deepseek — **FIXED 2026-09-20 (head side); the general fix is below**

**Reproduced, diagnosed, fixed, and the regression test fails without the fix.**

The operator's `/config` reading settled it: the model row said
`deepseek/deepseek-flash` while the header said `qwen-3.8-27b`. So the row was right and
the head had it — which contradicted both my earlier guesses.

**The cause is in the protocol, not in either end's logic.** `ServerFrame::Settings` has
exactly **one** send site and it is the *answer* to a `ClientFrame::Settings` request:

    crates/sessionlog/src/server.rs:500

`set_settings` fills the registry's mailbox and **nothing pushes**. So:

- a head attaches and asks → reads the rows once;
- the operator switches provider → `publish_settings` updates the registry, the turns go
  to the new model, and the *config pane* shows it (because opening `/config` asks again);
- the attached head is never told, and goes on drawing the row it read at attach.

`TurnStarted`, by contrast, **does** arrive unprompted and names the model answering that
turn. So the header now takes whichever it was told more recently, by `seq` — the only
clock it has, and the right one, since both are facts about the same stream
(`model_from_settings_at`, `model_from_turn_at`).

`a_provider_switch_reaches_an_already_attached_head` is the reproduction, and it was
checked by mutation: with the old row-first preference restored it fails with
`1/1 · qwen-3.8-27b` — the operator's own line.

**Still open, and it is the general fix**: push the rows to attached heads when they
change, rather than only answering. That needs a frame queue beside the head's event
queue (`Hub`'s `queue` is a `VecDeque<Envelope>`, so a `ServerFrame` has nowhere to go
today) — and it makes every other settings row live rather than only this one. Worth
doing; not needed for the symptom, which is gone.

### R20.2 — `how to switch diff style?` — the answer is nowhere on the screen

It is **`/config`, first row (`diff view`), Enter** — split ↔ unified. But `/diff` was
deliberately removed (*"one place to change a setting, not two"*) and **nothing in
`/help` or the hint bar says where it went**. The operator asking is the evidence.

**Where.** `crates/tui/src/app.rs:7540`'s `help_lines` table, and the composer's hint
bar. One line, and it is the kind that stops being asked.

---

## R12 — the firecode backend for subagents, and the cookbook — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `crates/tools/src/firecode.rs` + the harness placement; live test
`crates/tools/tests/firecode_live.rs` (up 6.9 s, read/write/list/stat/run/job,
down landing the guest's writes); an end-to-end run where a leticode session
spawned `task(role: researcher, access: read-only, where: firecode)` and the child
listed the workspace inside the VM and had no shell; `docs/cookbook.md`, filed as
skill `01M2FRW9D3A8K8V9660BJQX5PZ`. Left open, in `docs/subagents.md` §4:
checkpoints, `--cwd` made after boot, the child's own voice.

**Given by the operator 2026-09-14**, leaving for the day: *"1. firecode + cookbook"*.
Design and measured numbers in `docs/subagents.md` §3. The `task` tool's `where:
firecode` is a declared seam that refuses by name (`4f3d1d7`); this fills it.

**Where.** `crates/tools/src/firecode.rs` (`FirecodeBackend: ExecBackend`, a
`FirecodeConfinement` wrapping every job as `firecode in`), and `harness.rs`
placement: `Placement::Firecode` opens that backend for the child, `backend_confined
= true`, mode allow-all inside. The child works on a **copy** of the parent's
workspace under `~/.cache/letibot/firecode/` (firecode's shared-tree guard refuses a
main checkout with worktrees and uncommitted tracked changes — exactly when a parent
spawns; and `/tmp` scratch evaporates). Cold boots only: checkpoint/restore is not
dependable on the host yet (claude-host-lab's note `01M2FR5A1VAJJ13M7S2XRRZK33`). The
operator's direction for startup: **hierarchical image caches**, so a boot is
milliseconds — firecode's side of the seam; recorded in the cookbook, not built here.

**Done when.** A `task(where: "firecode")` from a leticode session boots a VM, runs
its tools inside it (read/write/list/run over vsock), ends with `down`, and returns
its answer naming the sibling directory where its writes landed; a live test behind
`FIRECODE_LIVE=1` holds it; `docs/cookbook.md` exists, is filed on the fabric as
`kind=skill`, and covers firecode, flowy, subagents.

---

## R13 — cloud GLM, DeepSeek and Grok as turn backends — **SETTLED 2026-09-14 (claude-lab2x1), one knob open**

Done: `crates/provider`, `MessagesBackend` in `letibot-backend`,
`TurnEngine::run_turn_messages`, `--provider/--api-key/--thinking`, the `provider`
disclosure, cost in the footer; fake-provider tests and an end-to-end harness run
(`docs/providers.md`). **Not verified live** — no key on lab2x1. Open, per the
operator's note: an eager-compaction budget under a meter (compaction is manual
today), and the providers' prompt caches are relied on through the stable prefix.

*"2. i want cloud glm and deepseek and grok to work in letibot/code."* D10 reserved
the seam (`crates/backend`: `TurnRequest` holds the transcript, `BackendCaps` states
facts, `Meter::Money`, `PrefixGuarantee::None`). All three speak an OpenAI-compatible
chat-completions API with tool calling; one `messages` backend with three endpoint
presets. Keys from the environment (`ZHIPU_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`)
or `~/.config/letibot/providers.toml`. D10's rule holds: the invariant suites skip
loudly under a provider — `skip_reason()` says the prefix check did not run.

**Done when.** `harnessd --provider deepseek --model deepseek-chat` (and `glm`,
`grok`) runs a turn with tool calls through the same engine; a fake-server test
covers the message conversion, streaming and tool-call parsing; live tests behind the
key being set; cost reported as `TurnCost.micros_usd`.

---

## R17 — Brave behind `web_search` — **SETTLED 2026-09-14 (claude-lab2x1), not yet run live**

*"I bought brave search api key, I want web search tool"* (2026-09-14). The seam
was already there: `web_search` has shipped as a refusing tool since the external
module was written, precisely so the schema could be fixed before anything cached
against it. This fills it.

- New crate `letibot-websearch` — `Brave` implements
  `letibot_tools::…::web::SearchProvider` over ureq+rustls. Its own crate because
  `letibot-tools` is deliberately *a function of a call and a filesystem*; the
  trait is the seam that keeps TLS out of it.
- Key: `--brave-key` → `$BRAVE_API_KEY` / `$BRAVE_SEARCH_API_KEY` → `[brave] key`
  in `~/.config/letibot/providers.toml`. **Refuses at attach**, naming all three.
  The key is held in a `OnceLock` in the websearch crate, never in `Config`,
  which derives `Debug`.
- Seated **only when attached** (`role_for_seat`, like `flowy`'s door), so a
  session without `--web-search` is byte-identical to yesterday's and nothing
  re-prefills. `max_tools` rises by one when it is.
- Only `web.results` is read; `<strong>` stripped; a URL-less hit dropped;
  `considered` carries the denominator; Brave's `query.altered` is surfaced as
  `rewritten_query` because a silently rewritten query is a rewritten query.
- Tests: 3 unit (key order and refusal text, the `[brave]` section read, markup),
  4 against a stand-in Brave (the key rides in `X-Subscription-Token`, `site:`
  goes out as Brave's operator, 401/429/transport each say which they are, an
  empty result is empty rather than invented).

**Verified live 2026-09-15.** The operator's key landed and
`crates/websearch/tests/brave_live.rs` (BRAVE_LIVE=1) ran a real query:
`Brave Search, key from ~/.config/letibot/providers.toml`, 3 hits of 3
considered, titles/URLs/snippets all parsed, no `<strong>` reaching the model.
The field names written from the documented shape are the ones Brave sends, so
the remaining unknown named here is closed.

Two notes from doing it. The key was pasted onto the commented placeholder line
and stayed a comment, which reads as "no key" — correctly, and the refusal named
all three places, but the operator's belief was that it was set; the placeholder
being *inside* the section is what made that easy. And the end-to-end through a
local-model session could not be run: `ggml-cuda.cu:108` on model load, the
standing CUDA fragility, not anything to do with search. `--provider deepseek
--web-search brave` is the path that avoids the local GPU.

---

## R16 — the preapproved list, Always allow, `ps`, a shell by default, and sudo-to-the-head — **SETTLED 2026-09-14 (claude-lab2x1)**

The operator, 2026-09-14: *"we badly need a list of preapproved globs, like all
read only git and gh commands, cargo, go, and other tests. and good old Allow
Always from opencode and Claude Code"*; *"when i do leticode --bash must be a
default"*; and, from the two transcript scans on this box (`PS_USE.md`: 294
`ps` pipelines in 22 days; `SUDO_USE.md`: 40 `sudo` attempts, 0 succeeded):
*"we need tools/skills for ps - i dont want models to reinvent the same commands
and i want a solution for sudo"*.

Done:

- **The preapproved list** — `permission::DEFAULT_ALLOW`, 142 prefix rules for
  `bash`: read-only git and gh, cargo/go/npm/pytest build-and-test verbs, the
  shell's read-only utilities. Deliberately absent: `find` (`-delete`), `awk`
  (`system()`), `sed` without `-n`, `env`, `gh api`, `cargo run`, `git branch
  -d`. A compound command is tested **one simple command at a time** — every
  segment must match — and a substitution, a redirection to a file, a group, a
  here-doc or a leading assignment is never matched (`bash_segments`). That is
  a parser for the purpose of NOT admitting, the one job `docs/tool-survey.md`
  allows a shell parser in a gate. Precedence: shipped < `~/.config/letibot/
  permission.json` < `$LETIBOT_PERMISSION`, last match wins; a `deny` row
  outranks everything. The `preapproved` disclosure counts them.
- **Always allow** — offered on every prompt whose tier is not always-ask, exec
  included (the 2026-09-11 rule *"exec asks every time"* is revised: a session
  GRANT is still never offered for exec; a durable RULE, in a file the operator
  reads and edits, is the operator's own preapproval). For `bash` the rule is
  the program and its verb (`cargo run --bin x` → `cargo run*`); for a file
  tool, the path. Written to `permission.json` through a sink; the row says
  when it could not be.
- **`ps`** — read-only; `pattern` / `pid` / `children_of` / `top cpu|mem`;
  pid, ppid, age, state, CPU%, RSS, command line; never lists this process;
  `PROTECTED` and `job` marks as `pkill`. Seated with the shell.
- **`leticode` seats the shell by default** (`--no-bash` to refuse it), the
  way opencode's coder has bash. What it runs unasked is the list above.

**Sudo — SETTLED 2026-09-14, shape 1.** The operator's ask: *"if model wants a
sudo i must be able to enter password safely and let it run … some other shim …
i honestly dont care now"*. Built as `SUDO_ASKPASS` routed to the head:

- Every command of a host session runs with `SUDO_ASKPASS` pointing at
  `letibot-askpass` (a fourth binary in the harnessd crate) and a `sudo` **shim**
  ahead of `PATH` (`crate::sudo`, written once to `$XDG_RUNTIME_DIR/letibot/
  shims`) that execs the real sudo with `-A` — except for `sudo -n`, left alone,
  so a probe still answers *no* honestly. The daemon also puts `LETIBOT_SOCKET`,
  `LETIBOT_SESSION` and `LETIBOT_COMMAND` in every command's env (a new
  `HostProcesses::set_standing_env` / `set_path_prefix`).
- The command itself passed the gate first — `sudo …` is privilege escalation,
  always-ask — so the operator saw and admitted it before any password.
- The helper attaches as an `askpass` head and sends one `Askpass` frame
  (PROTOCOL_VERSION 12). The daemon raises `SecretRequested` to every head; the
  TUI shows a card naming the command and sudo's prompt with a **masked field
  that owns the keyboard** (dots on screen, never the composer, never its
  history); the head answers with a `Secret` frame; the password goes head →
  daemon → helper → sudo's stdin and **nowhere else** — no log, no view, no
  transcript, no `CommandIssued`. The log gets `SecretSettled { given, by }`,
  the record without the secret. Deadline two minutes; Esc refuses; a late or
  duplicate answer is a `secret_late` warning.
- Tests: `letibot-sessionlog`'s `askpass` socket test (the secret reaches only
  the helper; refusal and late answer are honest; the hub state and log never
  hold it); the TUI `password_field` test (dots, keyboard ownership, no history);
  `letibot-tools`… the `sudo::install` shim test (adds `-A`, leaves `-n`); and
  the live `sudo_live` (SUDO_LIVE=1) that runs the box's real sudo through the
  shim and gets its wrong-password refusal, not a tty error.

Shape 2 (`Defaults timestamp_type=global`) and shape 3 (a detached privileged
shell) were the alternatives; shape 1 is the one that keeps the secret off the
log and scopes to the session. Not yet done: the remote/ACP heads have no
password card (only the TUI does), so a session driven only by one of those
still cannot answer a sudo — the frames are there, the UI is not.

The behaviour when no head answers is the one the scan shows Claude Code
already has: probe `sudo -n true`, announce the refusal, write the root half as
a script for the operator, and stop — never `script -q` or `echo '' | sudo -S`.

---

## R15 — a leticode session started at `/`, and searched it — **SETTLED 2026-09-14 (claude-lab2x1)**

Measured twice, driving local GLM as a one-shot coder on this repository: `read
crates/flowy/src/context.rs` → `no file`, because the whole-host backend joined the
relative path to its root and the root is `/`. The model then did the reasonable
thing — `glob` and `grep` from `/home` down for the file — and the daemon read a
27 GB model shard into memory and spent twelve minutes in the kernel with no head
able to reach it. Four fixes, all with tests:

- `HostBackend` has a `cwd`; relative paths and a command's `cwd` start there
  (`with_cwd`, `ExecBackend::workdir`); the harness sets it to the workspace for
  the `/`-rooted backend; the disclosure says `relative paths start at DIR`.
- `grep` opens no file over 16 MiB, stops a rung after 512 MiB read, and the
  result counts what it did not open. `read` refuses a file over 256 MiB by size
  before opening it. A walk never enters `/proc`, `/sys`, `/dev`, `/run`.
- The VM copy leaves out `.git/worktrees`: firecode's shared-tree guard counted
  the source's 28 and refused the copy.
- A NEW session's open-time notes reach the banner and the head (`open_note`):
  the project store choosing `writes allowed` over `--mode allow-all` used to go
  into a report only a resumed session printed.

After the fix the same GLM one-shot read the file, made the edit, added the two
tests (5/5 pass) and, having no shell, said it could not run them rather than
inventing a result line. DeepSeek, live, did the same on `grep.rs` for ≈ $0.02
(`docs/providers.md` "Verified").

**Then, the same afternoon: both models in a VM each.** `--where firecode` /
`letibot --vm` place a ROOT session in a VM (the placement a subagent already
had), `--vm-arg` hands `firecode up` its options, one-shots get their own
socket. DeepSeek and local GLM each wrote the `read` ceiling test and ran
`cargo test` inside — 10 min / $0.075 and 35 min respectively — after both
discovered the guest lacks llama.cpp and the sqlite dev symlink and stubbed them
outside the tree. `docs/cookbook.md` §4a. Then, the operator's rule — *"allow-all should be the
true allow-all"* — `Mode` gained a boundary axis: `allow-all` is `Structural`
and admits the always-ask list (`sudo -n id -u` in a VM: `0`, no ask); the
flow rule (a secret across the boundary) still refuses. Layers follow the copy
(`firecode layer inherit`, in the firecode tree, uncommitted). **Open:** the
layer itself for this project (toolchain, llama.cpp fork, `libsqlite3-dev`) —
needs docker, which lab2x1 lacks.

**Open, small.** `Mode::ALLOW_ALL` on a bare host is refused by its confinement
prerequisite and the project store overrides `--mode` — both by design — but a
one-shot with no adjudicator then has a `bash` that fails closed on every call.
A one-shot wanting a shell needs `--bash` and an adjudicator, or a VM. Whether
`--mode` on the command line should beat the store row is the operator's call.

---

## R14 — inject the fabric's skills and memories into the session — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `letibot_flowy::context` + `Sessions::with_fabric` / `refresh_fabric`, the
`fabric` disclosure, live test (8 skills, 42 memories; cache served with the node
away). Not run through a `--flowy` daemon end to end on this box: the seat's reader
is held by another session's listener and `Seat::open` refuses, as it should.
`docs/flowy-monitor.md` §5c.

*"3. I want flowy skills, memories etc to be injected. how? skills are summaries of
full pages, memories are titles."* At session open (and after a compaction), when
the daemon holds a seat: the shelf's skills as one line each (title + first
paragraph as the summary), memories as titles, into a `fabric` block of the system
prompt, so the model knows what exists and loads a body through `skill` /
`flowy get` when it needs it. **Offline mode** (`docs/tool-design-brief.md` §3b,
`docs/closed-loop.md` §5): a node that is away is a declared state, the block is
served from the last cached copy on disk and says its age; no seat, no block, said
so in the disclosure.

**Done when.** A session's system prompt carries the block; a compaction refreshes
it; a stalled seat serves the cached block labelled with its age; a session with no
seat has no block and the disclosure says `fabric: OFF`.

---

## R7 — A turn that ends inside its own reasoning is reported as success — **SETTLED 2026-09-13, see TODO-settled.md (R7) — a6b970e**

---

## R8 — `edit`'s near-miss recovery is defeated by exactly the error this model makes — **SETTLED 2026-09-12, see TODO-settled.md (R8) — ad23f39**

**Measured 2026-09-11**, in the live letibot session, twice in one turn.

The model sent `old_string` beginning `                if!text.is_empty() {`. The file
holds `                if !text.is_empty() {` at line 2543 — **one missing space** after
`if`. That is a reproduction artefact of this model (` !` versus `!`), not a typo, and
it is the same family as R7: GLM emitting something that is not quite the bytes it read.

The edit failed, which is correct. What is wrong is that **both** recovery paths in
`builtins/edit.rs::no_match` are defeated by that single space:

- `probe()` normalises whitespace, indentation and case — but normalising cannot
  **re-insert a deleted** character, so it finds nothing.
- `anchor_lines()` anchors on *"the longest whitespace-delimited token"* of the first
  line. Deleting the space **merges two tokens into one** — `if!text.is_empty()` —
  producing a token that occurs nowhere in the file.

So the tool honestly reports *"nothing in `…` matches that text, and no part of your
first line occurs anywhere in it either"* and then prints the file's **first 20 lines**,
which for `crates/tui/src/app.rs` is its module doc comment: pure waste, and no closer
to the answer. Two rounds burned, and the operator watching.

`text.is_empty() {` **does** occur, at 2543. One shorter anchor would have landed on it
and the model would have seen its own missing space.

**Still open?** `grep -n -A20 'fn no_match' crates/tools/src/builtins/edit.rs` — the
`_ =>` arm that dumps `file.lf.lines().take(20)`.

**Where.** `anchor_lines`, in the same file. When the longest token does not occur, fall
back to progressively shorter ones — next-longest, then any token over a few characters —
before giving up. Print the candidate lines, not the top of the file.

**Done when.** An `old_string` differing from a real line by one inserted or deleted
whitespace character comes back with that line and its number, and a test in this file
asserts it on the `if !text` case verbatim. §2.1's rule is the bar: the miss is
self-correcting **in the same call**.

---

## R9 — a refusal says `host_other` about a path inside the workspace — **SETTLED 2026-09-12, see TODO-settled.md (R9) — 545d9d7**

**Seen 2026-09-10** in the live session, on an `edit` of `crates/tui/src/app.rs`:

    reading: ask — intents [write_file] over [host_other]

The path is plainly inside the workspace, and the classifier that **decides** agrees:
`GateCall::path_is_inside` returns true for a relative path with no `..`, so
`ActionClass::host(access, inside, creates)` yields `EffectScope::HostProject`. The
grant key is right.

What is wrong is the **reporting**. The `host_other` in that line comes from
`crate::intent`'s `Region`, a different classifier written for shell-argument analysis,
whose `region_of` falls back to `Region::HostOther` for anything it cannot place. It
reaches the operator through the baseline string.

**Why it is worth fixing rather than tolerating.** It cost real time: the wrong scope in
that message was the first hypothesis for why `allow_session` did not stick, and it was
wrong — the cause was the mode's grant scope. A misleading fact in a refusal is worse
than no fact, because a refusal is what somebody reads when they are already confused.

**Still open?** `grep -rn 'region_of' crates/tools/src/intent.rs` and read the
`Region::HostOther` fallback at the end.

**Where.** Either give `region_of` the workspace so a file path resolves the way
`path_is_inside` does, or keep the baseline silent about region for a call whose class
was decided from a path. The second is smaller and loses nothing.

**Done when.** A gated `edit` inside the workspace reports a project-scoped region, or
none, and never `host_other`. A test on the rendered refusal payload, not on the class.

---

## R10 — layer 1's two env-hygiene lines, which `bash` waits on — **SETTLED 2026-09-12, see TODO-settled.md (R10) — 61afc57**

`docs/boundary-and-adjudication.md` §5 states these as **requirements rather than
assumptions**, *"because a requirement crosses a merge where an assumption does not"* —
and they are what stands between the exec substrate and
`Surroundings::with_pinned_shell` being honestly callable:

1. **`env_clear()` before the explicit `env` pairs**, so `PATH` is *pinned* rather than
   inherited.
2. **Unset `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`.** `BASH_ENV` **is** sourced by bash
   for non-interactive shells, so a distribution where `/bin/sh` is bash has a real
   injection point this box does not — which is exactly the kind of thing that is true
   here and false one machine over.

**Why this is the prerequisite and not caution.** *"A resolved parse is not a resolved
meaning."* A grammar reads text; a shell resolves a bare command name through aliases,
functions and `PATH`, none of which are **in** the text — which is how the survey's best
parser is defeated. So `intent::ShellTrust` defaults to `Unknown`: a bare name is
unresolved and the command is `not_run`, while an absolute path is not shadowable and
passes. Layer A can only mean something if layer 1 pins what the words resolve to.

The gap is small because `exec/host.rs` already spawns `/bin/sh -c`, non-interactive and
non-login, so no rc file is read, and `/bin/sh` here is `dash`, which reads `$ENV` only
when interactive.

**Still open?** `grep -rn 'env_clear\|BASH_ENV' crates/tools/src/exec/host.rs`

**Where.** The spawn path in `crates/tools/src/exec/host.rs`. `Prereq::Confinement`
already exists as the seam to assert it against.

**Done when.** The spawn clears the environment before setting its own pairs, the four
variables are unset, a test asserts a `BASH_ENV` planted in the parent does not reach the
child, and `Surroundings::with_pinned_shell` is called where it was previously only
described. Note what this does **not** do: it does not seat `bash` — see N4 and the
transcript-edge choke point, which is the other half.

---

## R11 — the audit rows are written and never read — **SETTLED 2026-09-13, see TODO-settled.md (R11) — 74abade**

---

# 2. NEEDS A NOD — small question first, then unblocked

## N5 — the agent should know the view is hermetic, and propose the grant it needs

Operator, 2026-09-11: *"i guess the agent should know it is hermetic and prompt me with
his idea of shared data which i can allow."*

**Measured 2026-09-11, and it is why this matters now.** The confined backend builds on
this box — `bubblewrap 0.11.1`, cgroup entered, nested-namespaces held, no-new-privs
held — and it is `writable + EXEC`, so confinement costs no file access. But its view is:

    project /home/dead/Projects/letibot (rw); 19 read-only system paths;
    $HOME is a FRESH TMPFS at /run/letibot/home
      (so a build cache under $HOME is empty every run); no grants
    egress: DENIED — no interfaces, no routes, no DNS

So `cargo test` cannot run: `~/.cargo` is absent and there is no network to refetch it.
That is the boundary working — *"a secret outside it is ABSENT, not denied"* — pointed at
a toolchain instead of a secret.

### Most of this exists

`exec/confine.rs` already has the mechanism, and its discipline is the interesting part:

- `Grant::{ReadOnly, ReadWrite, AgentSocket}`, each carrying a **`why`**, because *"a
  grant nobody can explain is a grant nobody can revoke"*
- `ViewSpec::granting()`, and `Boundary::describe` prints **the consequence next to the
  grant** — `ReadOnly` means readable *into the transcript*, since §3's second half is not
  enforced by that module
- the `agent_from_env` precedent: when a key is needed there is no grant that binds it.
  The socket is bound, the key is not, and *"the error path is the important half"* — the
  refusal names the mechanism nobody built rather than quietly widening

### What is missing is the loop, and it is three things

1. **The model does not know it is hermetic.** The boundary description is a startup
   banner for the operator; nothing puts it in the model's context, so a failure inside
   the view looks like a broken toolchain rather than an absent one.
2. **An absence does not say what would fix it.** `cargo: command not found` or a missing
   registry should come back as *absent because outside the view*, with the grant that
   would change it — the same shape as `edit`'s read-before-write refusal and R8's
   near-miss, self-correcting in the same call.
3. **Grants are construction-time.** `ViewSpec` is built before the spawn; nothing adds
   one for the rest of a session after an operator approves it.

### The questions, which is why this is a nod

**Does a grant go through the gate like any other decision?** It should — the prompt is
*"the model wants `~/.cargo` read-only, because: to run the workspace's tests"*, and
`grants session` then makes it stick for the session, which is the machinery that already
exists. The consequence line has to be in the prompt, not only in the banner: binding
`~/.cargo` read-only means its contents can reach the transcript.

**And which asks must be refused rather than prompted?** The ssh case is the precedent
and the flow rule (§3) is the test: a grant that would make secret bytes *readable* is
inexpressible, not adjudicable, however politely the model asks for it. A model that can
propose grants must not be able to propose that one and have it arrive as an ordinary
prompt. `NEVER_WRITE` is the first precheck and the tier mints no `Adjudicable` for an
inexpressible action — so the pieces are there; what is undecided is that a
model-proposed grant is routed through them rather than around them.

**Smallest version that closes the loop for tests:** the view learns one grant shape
(`ReadOnly` on a path), the model is told the view is hermetic and what it holds, an exec
failure names the absence, and the grant prompt carries its `why` and its consequence.
That is N4's answer too — see there for why a shaped runner is wanted rather than `bash`.

---

## N4 — a coder can write a test and cannot run it

**Observed 2026-09-11**: letibot wrote a regression test and then had no way to execute
it. Not a bug — a consequence, and the role table shows it plainly:

    coder()       write,edit + bash       <- §8.4's spec
    m2_coder()    write,edit, NO bash     <- what the daemon seats
    m2_runner()   bash, NO write,edit     <- the only role with a shell

**No implemented role can both change the code and run it.** This is the same shape as
`todo` before it was seated, one level up and costlier: there the encoder was missing, here
it is the *verifier*. A model that writes and cannot check is the closed loop of
`docs/closed-loop.md` §2 left open at the point where it would have paid.

**Why `bash` is not the answer, and the reason is good.** From the daemon's own
disclosure: *"`bash` is the tool whose result is an **arbitrary byte stream**. The job
verbs and `monitor` are seated and shaped."* §5's choke point — the single place every
tool result would pass through — does not exist, so nothing stops a result carrying bytes
from inside the view into the transcript. Every other tool's result is shaped: `read`
returns numbered lines, `grep` returns matches, the job verbs return structured state.
Seating `bash` on `coder` would widen the hole and add a capability by side effect, which
is exactly what `m2_coder`'s comment refuses.

**Nor does role switching help yet.** The mechanism exists (branch `role-switch`: the
registry holds the union, `ToolRuntime::active` gates admission, so a switch moves no
prompt byte) — but switching to `runner` still needs `bash` to be seated somewhere, so it
is blocked on the same §5 hole.

### The question, which is why this is a nod and not a task

**Is a shaped test verb acceptable where a shell is not?**

A `test` tool that takes **no command from the model**, runs the workspace's test command,
and returns a *shaped* result — passed, failed, and per-failure the test name and its
assertion — is not an arbitrary byte stream. `cargo test`'s output is parseable into
exactly those fields.

It narrows the hole rather than closing it: a failing assertion can still print whatever
the test printed, so the result is bounded to *test-framework output* instead of *anything
at all*. That is a real reduction and not zero, which is precisely why it wants a decision
rather than an implementation. Per-failure spill and truncation are available if the
answer is "yes, but bounded".

### Measured 2026-09-11, after this was filed — the blocker is not the choke point

A confined backend **builds on this box** (bwrap 0.11.1, cgroup entered, nested-namespaces
held) and is `writable + EXEC`, so it costs no file access. `needs_exec_backend()` is
`matches!(self, Seat::Runner)` — one line from covering `Coder`.

Two things stand in the way and neither is §5's transcript edge:

1. **Nothing starts a process.** `m2_runner` seats `job_list`, `job_output`, `job_wait`,
   `job_kill` and `monitor` — the verbs that *manage* a job. `bash` is the only one that
   *starts* one, and it is off. A shaped starter is the gap.
2. **`cargo` cannot run inside.** `$HOME` is a fresh tmpfs, so `~/.cargo` is absent, and
   egress is denied so it cannot be refetched. See **N5** — the boundary already has a
   grant mechanism and what is missing is the loop that proposes one.

**If the answer is no**, the honest consequence should be written into the disclosure: a
`coder` session states that it can write tests and not run them, so the operator runs them
and nobody is surprised.

---

## N3 — a refusal claims "the operator has been told" without knowing it

From the same refusal, verbatim:

    The operator has been told, with this: grant `adj-…-0001` (edit) for this session,
    or answer the pending decision.

The operator had **not** been told: they were away from the keyboard, the 300-second
window expired, and when they came back they asked *"what decision"*. The prompt was
gone and the question was unrecoverable — the ask is not a transcript row, only the
refusal is.

This is the fleet's own rule inverted. *Guard the fact, not the proxy*: "a prompt was
emitted" is not "the operator saw it", and the sentence states the second while knowing
only the first.

**Why this needs a nod rather than a fix.** The minimal change is honest and small —
say what is true (*"a decision was raised and timed out unanswered"*) instead of
asserting delivery. The larger change is the one that would actually help: make a
pending or expired decision **recoverable**, so `what decision` has an answer after the
window closes. That is a question about what the transcript records, which is N1's
territory, and it should be decided with N1 rather than beside it.

**Still open?** `grep -rn 'has been told' crates/tools/src/` — the payload builder.

**The question:** does an ask become a transcript row, or does the head keep a durable
list of expired decisions? Either answers *"what decision"*; they differ in whether the
model sees it too.

---

## N1 — Put content on `TranscriptAppended` (was T13.1)

**This is the structural one.** T14 settled that composable KV is impossible on hybrid
models and an addressable record is the fallback — and a log whose transcript events
carry no content **cannot be that record**. A head cannot reconstruct a conversation
from the log at all; the daemon reconciles out of band via `Hub::record_item`.

**Still open?** `grep -n 'TranscriptAppended' crates/harnessd/src/harness.rs` — the
comment at line 19 states the gap.

**Why it is not in section 1.** W8 kept §4.5's event exactly as specified and made the
gap explicit **rather than widening it unilaterally**, which was the right call: this
changes the head protocol. The question for the operator is one line — *does the event
carry the content, or a handle the head resolves?* — and after that it is code.

## N2 — `Dialect` split, rendering-as-data (T2), and structural eviction (T15)

Both arguments are finished and written down. Neither needs an answer so much as a
decision to spend the time; each is large enough that starting one is a scheduling
choice, not a task pickup.

---

# 3. BLOCKED ON THE OPERATOR — do not start

- **T5, T25** — 17 open decisions. Several gate everything else.
- **T22 and T17** — both wait on **D11**. "Until it is decided, M1 remains formally
  unexited."
- **T19** — rewrite-or-not is the decision itself.
- **T24** — needs **D3** (firecode's two asks).
- **T16** — several items explicitly want an operator or a strand owner.
- **T3** — the minja bug report is written and ready, but `llama.cpp/AGENTS.md` says
  reports and PR text are the operator's to write.
- **T6** — not started; `pytest` is still not installed, which `docs/workstreams.md`
  calls blocking for W1/W2.

---

# 4. Reference — the full text of everything above



## T5 — Operator decisions still open

Carried from `DECISIONS.md`; see there for the full statement of each.

- **D3** — firecode's two asks (parent cgroup, persistent shell). A written "no" is a
  complete answer and unblocks the work; it selects which of two tool runtimes gets
  built.
- **D5** — Falsifier B scoring rubric.
- **D6** — `max_inline_bytes` (deliberately has no default).
- **D7** — the 27B preset.
- **D8** — the harness licence. The workspace currently declares Apache-2.0 and there
  is no GitHub remote yet; the repo is intended to be public, so this should not stay
  open.

---

---

## T25 — Everything waiting on the operator

Filed on request, so a question does not live only in a scrollback. Continues the D
series. **Two arrived answered in the same message and are recorded as resolved** rather
than dropped, because the answer is the interesting part.

### Open

**D11 — Land the four branches?** session-resume (protocol 4, `--continue`), exec,
intent, outside-world. All green, all rebase cleanly. Landing costs the running daemon
(pid 384248) its live session, because protocol 3 → 4 makes a fresh head refuse it by
name. Every transcript is on disk and `--continue` now genuinely returns the newest.

> **Overtaken 2026-09-10: `PROTOCOL_VERSION` is 6.** The four branches landed and the
> assembly on top of them needed a wire change of its own — `SessionEvent::DenialRaised`,
> §4b's requirement that a refusal reach the operator when it happens. The cost D11 named
> is now paid once for both: a running daemon on 5 loses its live session to a fresh head
> either way. T24's `PromoteJob` was the *other* claimant on 6 (`ProcessHost::promote`'s
> doc specifies the frame verbatim) and it is **not** in this bump — it is a `ClientFrame`
> and needs a head that sends it, whereas a denial is a `SessionEvent` and needs only a
> head that renders it. Adding it later is another integer; the constant is the one line
> every wire-touching branch edits and taking 6 for the thing that was ready is the same
> "coordinate rather than race" D10 settled.

**D12 — A mounted board makes `todo` gated.** The flowy backing declares
`Access::Network`, so mounting a board without an adjudicator attached makes `todo` refuse
*entirely* — the unmounted local list works, the mounted one does not. Correct by the
rules and possibly intolerable in practice.

> **Sharper as of 2026-09-10, and generalised.** The same shape now decides whether a
> session *opens at all*: a seat whose tools declare `Write`, `Exec` or `Network` and has
> no adjudicator refuses to start, rather than starting and failing every call. That is
> the right end of the trade for `write` and `bash`. For **`say`** it is the D12 problem
> with a bigger blast radius — the planner seat needs an adjudicator because it can talk
> to a room, and a planner that cannot plan without one is exactly *"correct by the rules
> and intolerable in practice"*. The unasked question underneath both: is *reaching the
> fabric the operator already authorised this seat to be on* the same class of act as
> *reaching an arbitrary host*. `Surroundings::seen_hosts` says first contact and second
> contact differ; nothing yet says a **declared** attachment differs from a discovered one.

**D13 — `Access::Session`, a new access class.** Declaring the intent tools `Read` would
have reproduced the gap the survey names in grok-build (§1.4). The wire carries access as
a String so nothing breaks, but no head has rendered one.

**D14 — `loop_closes` needs an exclusive model server.** It drives live inference on
`:8080`; it passes alone in 11–32 s and times out under `cargo test --workspace` whenever
the box is busy. **Three separate agents have now reported it as a possible regression**,
and one of them burned a re-run attributing it. A test that reads as a failure whenever
the box is loaded is a broken instrument. Proposal: gate it behind an env var or `#[ignore]`
so `--workspace` means what it says.

> Fourth data point, 2026-09-10: green in 22.2 s standalone and green under
> `--workspace` on an unloaded box, alongside `turn/tests/live_qwen` (4 tests, 6.0 s).
> Two passes are not evidence against the entry — the failure mode is contention and
> the box was quiet — but they do say the instrument is not broken in some second way,
> which is what a fifth agent would otherwise spend a re-run finding out.

**D15 — 13 stale worktrees and one 30-hour orphan tmux** (`nano_test`). Safe to prune the
finished ones; two belong to live agents, so not a blanket sweep. T24 is the mechanism,
this is the backlog it would have prevented.

**D16 — `~/bin/letibot` is not version-controlled**, and two real bugs were found in it
tonight (a `--continue` that showed an empty screen, and an `up()` that would have
orphaned a version-skewed daemon holding every session). It belongs in the repo.

**D17 — Group 2, subagents and admission.** Held all evening because it lands in
`crates/harnessd/src/sessions.rs`, which session-resume was rewriting. **Now unblocked.**
Its hard requirement is already written: `Fits{ceiling, cost_per_id, pool_free}`, because
436.7 MiB per sequence id charged at allocation makes N subagents an OOM path — and two of
the five surveyed harnesses ship subagents with no ceiling at all.

**D18 — LSP.** The one gap in the survey's ten that no group covers. Four of five
harnesses have it, the most complete is 14 actions, and it needs a language server per
language. Its own decision, not a group.

**D20 — `never_hit` now sees network arguments.** A `web_search` or `github` string that
merely *mentions* `.config/gh` or `.password-store` is denied by the never-write list.
Fail-closed, and a real false positive.

**D21 — Enforcement binds the model, or the seat?** From `docs/closed-loop.md` §9. They
differ when a human is driving, and the operator should not be locked out of their own
restart because the model cannot be trusted with it. Tonight's blocked
`systemctl --user restart` is the worked example.

**D22 — The tolerance band: one, or per-operation?** Also §9. How large a deviation is
corrected silently versus faulted to a human is the whole design decision, and both ends
have measured costs. **One data point now exists**: T21.3's nudge is capped at one per
user turn, chosen at its narrowest for want of a measurement rather than because one is
right.

**D25 — The console adjudicator reads the daemon's own stdin, and a head cannot answer
it.** New, and the direct consequence of making roles reachable. `--role coder` gets
`ConsoleAdjudicator::stdio` by default, which blocks on stdin: correct and usable for
`harnessd --prompt …` and for a daemon in a foreground terminal, and **not** usable for
the shape people actually run — a daemon in the background with `letibot-tui` attached.
The head's answer affordance is D10, which is specified (`Answer { option, note, free }`,
a protocol bump, an affordance in the head) and not built.

So today a write or exec seat is a foreground thing. The startup disclosure says so; it
is not silent. What it means in practice is that the reachability this strand added is
reachable **from a terminal**, and a head-driven write session waits on D10. Two ways
out and they are not equivalent: wire the adjudicator to the head (D10, correct, costs a
protocol bump and head work), or attach the model adjudicator (D13, which needs an
oracle and a reviewed always-ask list, and is what the survey's 83% is about).

**D26 — A wake spends a generation, and nothing bounds a chatty monitor.** T24's wake
turns a firing into a user item and runs a turn. A monitor watching a path that changes
every second would spend the box's slots on itself until its TTL expires. The TTL is the
only bound and it is up to an hour. A firing that should be *noted* rather than *acted
on* has no spelling — and inventing one is a decision about what a monitor is for, not a
fix.

**D27 — Six adjudication items from the assembly agent, none filed until now.**
Carried out of a subagent's report and into scrollback, which is exactly the failure
T25 exists to prevent. In descending order of how often they bite:
`allow_session` does not stick (likely the `host_other` misclassification breaking a
class-keyed grant); `host_other` misclassification itself; `/mode` and `--mode` are
unwired; mode does not persist per project; a resumed session does not disclose a
tool-list mismatch; and the write prompt has no one-keypress answer -- the operator
had to type the whole word, and mistype it. Operator, 2026-09-10: *"ok so got the
write prompt, but it wasnt as a choice but as sometihng i have to type (and
mistype) myself"*.

**2026-09-13 (D13):** two of the six are now wired — *mode does not persist per
project* and the mode side of *`/mode` and `--mode` are unwired*. The per-project
`ModeStore` is threaded into `Parts` and consulted in `open_with` after the
workspace is resolved, so a session's point is its project's row (longest ancestor
wins), not the daemon's `--mode` default. A new named point `allow-all` (opencode's
`bypassPermissions`) ships, and `Mode::parse` accepts opencode's four permission-mode
names. Still open: the `/mode` head command to *move* a project at runtime, the
`todo_write` rename completion, and the remaining four D27 items.

**D28 — Text selection in the TUI, still undiagnosed and needing one answer from
the operator.** The repaint hypothesis is REFUTED by measurement: before/after
frames byte-identical over 1,256,038 bytes, and 30 s idle wrote **zero bytes**.
Two candidates remain and one question splits them: *can you select while it is
idle, between turns?* Yes -> the complaint is mid-turn and is content change under
the selection, not gratuitous repaint. No -> it is not this code at all, it is the
TUI holding SGR mouse mode so the terminal hands drags to the application instead
of doing native selection, and the fix is a mode toggle.

### Not ours, tracked because we caused or found them

**D23 — The 702 long chunks.** 702 of 43,967,653 exceed ~2048 tokens and already hold
ollama vectors of *truncated* text. Routing them to TEI makes new ≠ old for exactly those
documents. Small enough to re-embed rather than manage; lubuntu3's corpus, our capacity.

**D24 — The chunker length assertion.** lubuntu1's close, unowned: while chunks stay under
~2048 every backend agrees and none of the truncation split matters. Nobody enforces it.



---

## T22 — `f_keep` names two different quantities, and C4 applies one's threshold to the other

Found while answering "how is `f_keep` computed". The answer is: **two ways, and the
plan uses both under one name.**

### The two

`server-task.cpp:2603,2614` — a cache **selection** heuristic, with a hard floor
refusing any entry under `f_keep < 0.25`:

```
f_keep = lcp / cached_entry.size()      denominator = the CACHED entry
f_sim  = lcp / new_prompt.size()        denominator = the NEW prompt
```

`lcp` is the longest common prefix — how many leading tokens of the new prompt match
the cached entry, token for token, via `server_tokens::get_common_prefix`.

§18.2's **C4** defines it as `usage.prompt_tokens_details.cached_tokens /
prompt_tokens`. **That is `f_sim`, under `f_keep`'s name.**

### Why it matters — one turn of the M1 run

```
cached entry (turn N)     9,000 tokens
new prompt  (turn N+1)   10,093 tokens   ← appended a 1,093-token tool result
lcp                       9,000 tokens   ← the whole entry is a prefix; nothing rewritten
```

That is a **perfect** turn — full reuse, nothing recomputed.

| | formula | value |
|---|---|---|
| llama's `f_keep` | lcp / cached | **1.000** |
| llama's `f_sim` | lcp / new prompt | 0.892 |
| **C4's `f_keep`** | cached_tokens / prompt_tokens | **0.892** |

The gap is not a cache miss. **It is the tool result that was just appended.** C4's
number falls purely as a function of how much the conversation grew.

### The defect

C4 says it is *"directly comparable to the 0.000 → 0.999 measurement"*. **It is not.**
That 0.999 was read off the server's trace lines, so it was llama's `f_keep` —
denominator the cached entry, and therefore **indifferent to growth**. The plan even
notes at line 64 that the two are "a hit-ratio pair", then migrates a threshold from
one to the other.

So a bar measured on metric A is applied to metric B. On A, 0.99 is reachable and was
reached. On B it is arithmetically impossible for any session that returns tool
output.

### SETTLED 2026-09-09 — option 1, see D11

`f_keep = lcp / cached_entry`, computed as
`cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))` — **no server change
needed**, since the denominator is what we left in the cache and the numerator is what
the server already returns. It is the ratio form of C3's inequality, over C3's own
quantities.

**M1 must be re-measured.** 0.8771 was a correct measurement of the wrong metric.

### The choice as it stood — operator's

1. **Restate C4 as `lcp / cached_entry`** — directly comparable to the 0.999 that
   motivated it, indifferent to growth, and what the append-only invariant actually
   claims. Needs the server's `lcp`, which is S1 or `-lv 4`.
2. **Keep `cached_tokens / prompt_tokens`** and set a bar reachable for tool-using
   sessions. The harness-only version of this is T17's C4b, which measured p10 1.0000.

Until it is decided, **M1 remains formally unexited** and the run stands as recorded.

This is the seventh instance in one day of a number whose meaning was not checked
before use, and unlike the others it is **ours** — it is in the plan, not in something
we inherited.

---



---

## T17 — harnessd — **BUILT 2026-09-09. The loop runs; M1's exit criterion is NOT met as written.**

`crates/harnessd`. Scripted 30-turn session against `qwen-3.8-flash-next`: 53
submissions, 25 tool calls, 161 persisted rows, 22,201 final prompt tokens, 85 s.

| check | result |
|---|---|
| C1 exact prefix extension | **PASS**, off the ledger's token vectors |
| C3 generation-inclusive prefix | never violated; 3/52 server-side shortfalls |
| **C4 `f_keep` p10** | **0.8771 — FAILS ≥0.99** |
| C4b cache efficiency p10 | **1.0000** |
| C5 reasoning replay | PASS (worst 0.9859) |
| C9 mid-session system change | PASS — stable prefix byte-identical |
| C10 disjoint cache accounting | PASS on all 53 |
| C6 / C7 / C8 | not run — see below |

**See T22 first — C4 is mis-specified, and the run measured what the plan asked for
rather than what the plan meant.**

**On the failure, and why I do not think it is the harness.** `f_keep`'s denominator
is the *whole* prompt, so a submission appending a 1,093-token tool result to a
9,000-token prompt cannot exceed 0.88 however perfect the cache is. 19 of 52
submissions add more than 1% of their own prompt. The **ceiling the script allows is
p10 0.8813, and we got 0.8771 of it**. Session-wide, 14,901 of 640,076 prompt tokens
were prefilled — **97.7% avoided**.

So **§17's exit criterion measures the script's shape as much as the harness**, and
≥0.99 is unreachable for *any* tool-using session. The harness-only number is C4b, at
p10 1.0000. **The criterion needs rewriting; the plan is wrong here, not the code** —
but it is recorded as a failure because that is what it is against the stated bar.

C6/C7/C8 could not run: C6 needs a tool-call-only assistant turn, which did not occur;
C7/C8 need a `length` finish, which requires either the `n_predict` cap §5.7 removed or
a 262k context on a production box.

**C3's shortfalls have an exact signature** — every one equalled `previous generation +
3`: the server reused to the end of the last committed item and discarded the 4-token
generation prompt *and* the whole generation. It is intermittent (14/55 on an earlier
run of the same script), so it is a server-side cache event, not structural. Settling it
needs `-lv 4` and `glm-why-no-cache` — a server-log question the harness cannot answer
from its own metrics.

---

---

## T19 — Rewrite or not, on the KV representation — **the angle UNVERIFIED-16 should be read from**

The operator's framing, and it is the right one: llama.cpp's data structures are not
the question. The question is **what would require a rewrite, and whether that rewrite
pays.** Separating the two changes what T14's negative actually settles.

### llama.cpp's choices — changeable

- recurrent cells *are* sequence ids, so a save is one 111.4 MiB blob per sequence
- attention KV and recurrent state are saved as a single unit, all-or-nothing
- rewind has **three fixed re-entry points**: free to `d ≤ 3`, one checkpoint to
  `d ≤ 516`, then **the entire prefix from token 0**

### Mathematics — survives any rewrite

A recurrent state is a **fold**: the state at position *n* encodes all *n* tokens, so
there is no "block B's contribution" independent of what preceded it. It is not stored
badly; it does not exist as a separable quantity.

This is also why CacheBlend works on attention-only models and cannot here. Attention
has cross-block dependence too, but it is **diffuse** — a weighted average whose
distribution barely shifts under a different prefix, so recomputing the ~15% that
deviate most corrects the rest. A fold has no diffuseness to exploit: there is no
subset whose recomputation repairs the remainder.

### The split

| | verdict | why |
|---|---|---|
| composition across a divergence | **do not rewrite** | mathematically blocked, not an implementation limit |
| splitting attention from recurrent in the save format | **do not rewrite** | after a divergence every layer differs anyway, so there is nothing to reuse |
| **checkpoint density / rewind granularity** | **worth doing** | pure implementation; the `d > 516` cliff is checkpoint placement and nothing else |

**The third row is the actionable one and it was measured by accident.** Rewind is what
fork (§5.5), backtracking and eviction all actually need, and today it falls off a
cliff at 516 tokens. Storing recurrent state at more positions makes rewind cheap. That
is a parameter and a representation choice, not a redesign.

---



---

## T24 — The harness owns what a subagent leaves behind, and monitors need the same owner

Operator, 2026-09-09: *"you guys like to accumulate monitors and shells"* — and, on
being shown the measurement, *"you are fine, others nt"*. That correction is the
requirement. The leak is not the top-level agent's own housekeeping; it is **everything
its children spawn and do not clean up.**

Measured on this box the same evening, mid-session:

| | |
|---|---|
| git worktrees in `.claude/worktrees/` | **13**, most from agents that finished hours ago |
| orphan tmux sessions | `nano_test`, **30 hours** old, from a finished agent |
| the top-level agent's own leak | 1 listener, 1 poll loop — correct |

`docs/tui-testing.md` already says *"Kill your sessions. An orphaned tmux session holds a
`harnessd` and its socket, and the next run then attaches to a daemon it did not start."*
Written here, loaded in context, and it did not bind. That is `docs/closed-loop.md` §1
again and the fix is the same: not a better rule.

### The mechanism was already chosen

D4, answered by the operator earlier the same day: *"just do byobu, which connects to
cgroups — use dependent. if a vm is temporary then it is a session cgroup otherwise not.
still must be clearly reapable."*

So: **every process a turn spawns lands in a cgroup owned by a scope.** Three scopes and
no fourth — `turn`, `session`, and `explicit` (survives the session because somebody said
so, and is listed as such). Reaping is `cgroup.kill`, and a subagent's cgroup is a child
of its parent's, so a parent that ends reaps its children by construction rather than by
remembering to.

### The part that pays for itself immediately

**Cgroups retire pattern matching for both reaping and liveness**, which is T21.1 and
T21.2 dissolved rather than guarded:

- `pkill -f X` becomes "kill this cgroup" — no pattern, so nothing to self-match.
- `until pgrep -f X` becomes "is this cgroup non-empty" — no predicate that can match its
  own waiter.

The evidence that a guard is not enough: on 2026-09-09 a process check self-matched
**five times in one session**, with `process-checks-that-self-match` loaded in memory and
T21 open in this file. The fifth was `grep -E '[h]arnessd'` — the bracket trick defeats
`pgrep`, but the shell wrapper echoes the expanded pattern back into its own command line,
so the literal string was there to be found. A hazard with that many spellings is not
one you check for; it is one you make unspellable.

### Monitors, which are the same problem wearing a different hat

Operator, same message: *"btw we need monitors in letibot"*. A monitor is a condition
watched **between** turns that wakes the loop when it fires — the encoder running while
the model is not, in `docs/closed-loop.md`'s terms, and the only correct shape for
listening. The seat brief pays for that distinction: *a Stop hook fires when a session
goes idle, and a seat that is rate limited, has crashed, or never started is not running
a session, so no stop event ever fires and the silence looks exactly like a quiet room.*

A monitor is also, structurally, a long-lived process. **Adding monitors before the
lifetime work multiplies the leak this entry exists to stop**, so they land together:

1. **Scoped at creation.** No monitor without an owner; the default is the session.
2. **Listable and attributable.** The head can show what is watching and for whom, the
   way it now shows sessions. An invisible watcher is an unreapable one.
3. **Reports why it fired**, not just that it did.
4. **One waiter per name**, enforced. The fleet already learned this: two processes under
   one reader means the roster shows a seat attached while the real one hears nothing.
5. **Bounded.** A TTL or an explicit renewal, so a monitor whose reason has passed dies
   without anybody remembering it.

### What the fleet answered, 2026-09-09 — and what it ruled out

Asked in `Lab/#general` with the counts stated first so replies were comparable.

| box | worktrees | orphan tmux | listeners |
|---|---|---|---|
| **.79 (here)** | **13**, from finished subagents | 1, 30 h | 1 + 1, correct |
| .78 (lubuntu2) | 4, **deliberate** — four llama.cpp checkouts, 3 live | 1, **14 days** | 1 + 1, correct |
| .76 (lubuntu1) | 0 | 0 (the one present is the operator's login) | 1 Monitor + 1 unit |

**lubuntu1's reading is the finding, and it is better than the counts:**

> *"the leak scales with children spawned, not with seat uptime. .76 has been up as long
> as you and leaked nothing. Reaping belongs wherever subagents are created, not in a
> periodic sweep on each box."*

That **rules out a design this entry left open**: a per-box reaper on a timer. A sweeper
cannot tell debris from a deliberate long-lived resource, and it runs on boxes with
nothing to sweep while the box doing fan-out is the only one that needs it. Reaping is a
property of the *creation site*, which is what the parent/child cgroup shape already
gives — and it means the mechanism ships with subagents (S8/M6) rather than as fleet
housekeeping.

**lubuntu2 supplies the case a sweeper would get wrong**: four worktrees that are not a
leak at all — three are live llama.cpp variants under active comparison. A long-lived
resource with a declared owner is correct. That is the `explicit` scope above, and .78 is
the reason it must exist rather than be a convenience.

**And lubuntu1's caveat is the most useful sentence in the thread**, because it is about
how the measurement lies:

> *"I killed a stray llama-server earlier tonight after a benchmark, and I only noticed
> because I went looking. Had I not, it would be in this count. My zero is partly
> attention, not only design."*

A zero produced by vigilance and a zero produced by a mechanism are the same number and
different facts — `docs/closed-loop.md` §3 exactly, one layer up. So the acceptance test
for this work is **not** "the counts are low". It is that the counts stay low when nobody
is watching, which means the reaper has to be observable: a scope that ended must record
what it killed, or its zero is unfalsifiable too.

### Blocks on

Nothing. This is substrate for M6 subagents (S8) and it is **cheaper to build before them
than after**: the leak measured above came from subagents the harness does not yet have,
run by an agent that does. The requirement was discovered before the feature, which is the
rare ordering.

### Landed, 2026-09-10 — monitors and background promotion

`crates/tools/src/exec/monitor.rs`, `crates/tools/src/builtins/monitor.rs`,
`crates/tools/tests/background.rs`. All five requirements above are mechanised and each
has a test. What is worth recording is the three decisions that were **not** obvious.

**The promotion threshold is 15 s and it is reactive.** Measured, warm, on `.79` at load
1.4: the whole routine set — `find`, `grep -rn`, `cargo check -p`, `cargo check
--workspace`, `cargo clippy --workspace --all-targets`, `cargo test -p letibot-tools`, the
fidelity gate — tops out at **6.5 s**. 15 s clears the slowest by 2.3×, and is 6× under the
`sleep 90` the operator named as the case that must promote. The previous default was 120 s,
which would have let `sleep 90` block the turn for a minute and a half and never promote.
The threshold fires on **elapsed time and never on the command text**: `cargo build` is 3 s
warm and 4 minutes cold and the string is the same both times, so a predictive rule is
`docs/closed-loop.md`'s open-loop stepper in a new costume.

**`m2_runner` is nine tools, one over §8.4's ceiling, and it says so in its own
`max_tools`.** The ceiling's evidence is about confusion between *similar* choices, so the
monitor surface was cut twice before it was allowed to cost a seat: declare/renew/retire are
one tool taking an `action` because all three act on one named handle, and **listing is not a
tool at all** — monitors are in `job_list` beside the jobs, the scopes, the promotions and
the reap log, because "what is running and what is watching" is one question and a second
listing nobody opens is how a watcher becomes invisible without anybody hiding it. What is
left cannot fold into `job_wait`: that blocks *inside* the turn, and a flag switching between
the two would make "I believed I had waited" spellable. A ceiling quietly raised for
everybody is not a ceiling; one role declaring its own number with the trade written down is
a decision somebody can reverse, and a test pins both numbers.

> **2026-09-10: the role is seatable, and it seats eight of the nine.** `--role runner`
> over `HostBackend::confined`. `bash` needs `--bash` on top, because
> `docs/boundary-and-adjudication.md` §5's transcript choke point does not exist: the
> boundary keeps secret bytes out of the process's **view** — `~/.ssh` is *absent* from
> the mount namespace, not denied — and nothing yet stops a tool result carrying bytes
> from inside that view into the transcript. `bash` is the tool whose result is an
> arbitrary byte stream; the job verbs and `monitor` are shaped by the tool. So the
> common case is eight tools, which is `DEFAULT_MAX_TOOLS` exactly, and the ninth seat is
> spent only when somebody asks for it. That is a nicer answer than the entry above
> expected and it is a **consequence**, not a plan: the ceiling was not what kept `bash`
> off.
>
> Measured on `.79`: `HostBackend::confined` builds — delegated cgroup v2 subtree and
> `bwrap` both present — so the runner seat is real here rather than a constructor that
> always errors. A box without either gets a refusal naming which half is missing, and
> never a silent fall back to `HostBackend::executable`, which buys cgroup lifetime and
> no view at all.

**A port watch has no `host` argument, and that omission is load-bearing.** A monitor that
could reach an arbitrary address would have to declare `Access::Network` on *every* call,
including the ones watching a cgroup — which is D12's shape (a network declaration making an
unrelated capability refuse entirely). Loopback-only keeps the whole tool at `Access::Exec`,
which is also the honest class: it leaves something watching after the turn ends.

### Still open — the head cannot promote a job yet

Requirement 3 of the operator's three (*"the operator promotes it, mid-flight, from the
head"*) is **half done**: the daemon-side verb is `ProcessHost::promote`, it is tested with
`Backgrounding::Operator`, and the frame the head must send is specified verbatim in that
method's doc comment — `ClientFrame::PromoteJob { client_request_id, expected_seq, job,
identity }`, queued, idempotent, answered by calling `promote`.

It is **not wired**, deliberately, and the reason belongs with D11. A new `ClientFrame`
variant is a wire change, so `PROTOCOL_VERSION` goes 5 → 6; both sides refuse a mismatch by
name; and landing a bump costs the running daemon its live session. Against that, the verb
buys nothing in production today: `harnessd` seats `m1_orchestrator`, which has no `bash`,
no jobs, and therefore no job to promote. The constant is also the one line every
wire-touching branch edits, and 5 was taken deliberately *"to coordinate rather than race"*
(D10). So: one variant and one integer, whenever a head is ready to send it.

### ~~Still open — nothing calls the monitor wake seam~~ — **CLOSED 2026-09-10**

`Monitors::wait_for_any` now has a caller, and the interesting part is what had to be
added before it could have one.

**`Bell::ring` was not the door.** `Registry::next_work` skips a session whose command
queue is empty, so ringing the existing bell for a firing would have been a no-op that
*read* like a wake — worse than the poll it was replacing, because the poll was honest.
So `Bell::ring_wake` and `Work::Woken` are new: a third queue, drained **last**, because
a head that pressed enter is waiting and a monitor is not. A third `Work` variant rather
than a synthetic `Command`, because a command has an issuing head, an identity and a
`client_request_id`, and inventing three of those would put a head's name on something no
head did.

**Two cursors, and they answer different questions.** The waiter thread keeps *"have I
rung for this?"*; the harness and its steering source share *"has the model seen this?"*.
That is what makes a firing arrive exactly once whether it is picked up mid-turn (through
steering, at the next step boundary) or between turns (through the wake). A wake that
raced a mid-turn pickup returns `Outcome::Ignored` rather than running a turn about
something already delivered.

**No monitors, no thread**, the same rule `Monitors`' own poller keeps and the same rule
this entry exists to enforce: the waiter is armed after a turn in which something is
actually watching, one per session, tracked so a second cannot start. Its `wait_for_any`
deadline is **not** delivery — the condvar is — it is the only thing a thread blocked in
that condvar can do about `Bell::close`, which notifies a different one. Named here
because "no timer, no poll loop" (§18.1-I12) is a property this daemon states about
itself, and a re-check that went undescribed would read as a violation of it.

What is **not** closed: the wake runs a full turn with the firing as a user item. A
session with a chatty monitor therefore spends generations on it, and nothing bounds that
except the monitor's own TTL. A firing that should be noted and not acted on has no
spelling yet.

### Still open — the disclosure can say `POLL ONLY` and the operator cannot tell why

`GateWiring::monitor_wake` is stamped by `Sessions::arm_wake`, so a `Harness` driven
directly by a test or by `letibot-m1` honestly reports `false`. What it does not
distinguish is *"no daemon armed one"* from *"the thread failed to start"* — the second
publishes a `Warning`, the first is silent, and both render the same line. Small, and the
kind of thing that costs an hour once.

---

---

## T16 — W9's open questions — **several want an operator or a strand owner, not me**

From the tool runtime, in descending order of consequence.

1. **§8.1 clause 3 gives the outcome vocabulary but never the mapping.** Which outcome
   is an empty `grep`? W9 decided: matches in scope or elsewhere ⇒ `Ok`; nothing
   anywhere under any relaxation ⇒ `Abstained`; a nonexistent path ⇒ `Failed`. That
   choice determines how often `propagate()` blocks a caller and should be recorded
   rather than inherited.
2. **D6 answered means M1 ships with clause 5 switched off.** `NoBudget` is correct per
   D6 — unset is a genuine no-op — so **nothing spills until a budget is configured**.
   A session-config gap, not a code one, and invisible unless said.
3. **`Gate` will collide with W11.** W9 defined the smallest adjudication seam the
   runtime needs (`admit(name, access, args)`). W11 should absorb it rather than build
   a parallel one.
4. **§8.1 clause 1 and §9.4 pull against each other.** Relaxing a pattern *is*
   rewriting the query, which §9.4 forbids. The reconciling word is **visible**: every
   relaxation appears in the result. Worth stating in §9.4 so nobody deletes one of
   the two.
5. **§8.4's `orchestrator` role has no `read_spill`** though its `read`/`grep` can
   spill — so for that role the omission notice is advisory, which §8.3 says it must
   not be. W9 shipped `read_spill` in its place.
6. **Retrieval is inert — no MCP server is running anywhere.** Confirmed by lubuntu3
   on 2026-09-09, checked rather than recalled.
   - `192.168.1.55:9755` is the **flowy node**, not oracle. Never was the address.
   - **RAGFlow and oracle are on lubuntu3 (192.168.1.82)** — but its MCP server **is
     not started by that deployment**. Docker publishes 9382-9384, so a **TCP connect
     succeeds while nothing is behind it** (`curl` → HTTP 000, zero mcp processes in
     the container). That is why a client reports "unable to connect to the url": the
     connection is fine, the service is absent. Same shape as the embed wedge — a
     check that succeeds without touching the thing it checks.
   - `:8100/sse` **looks** like an SSE endpoint and is not — it is arxiv-search, whose
     catch-all returns identical HTML for every path. Do not use it.
   - **Use the REST API instead: `192.168.1.82:9380`, base `/api/v1`, bearer auth.**
     Do not ask for an MCP listener to be started while ingestion is live.
   - `ask_code` is a **separate problem**: per-box, against the local codebase, and
     **nothing is running on this box** (checked — only qwen/bge/postgres listen).
   - **Open and load-bearing:** does RAGFlow signal "the corpus does not cover this"
     in a *field*, or only in prose? §8.1 clause 3 requires abstention to be
     structurally distinguishable. If it is a field, it maps to `Abstained` and the
     harness can never report it as grounded. **If it is only prose, we will not parse
     the prose and pretend** — the tool returns `Ok` with the text and the hole stays
     documented. Asked; awaiting an answer.
   - Note from lubuntu3: `.82:11434` enforces **one request per backend and will hold
     a call rather than refuse it**. Do not point volume at it unannounced.
7. **Call ids are positional per turn** (`call_0`; GLM carries none), so an id-derived
   mark repeats across turns. Fine now, wrong once a head correlates across a session.

---

---

## T3 — Report the minja scoping bug upstream — **ready, not filed**

`{% set %}` inside `{% for %}` is not scoped per iteration in llama.cpp's minja.
Repro needs no GPU: a template, four messages, one `/apply-template` call.
Full writeup and both engines' output in `docs/chat-templates.md` §2.

Affects any llama.cpp user serving GLM with `messages` — the model is conditioned on
reasoning it never produced. We are insulated because we submit token ids.

---

---

## T6 — Not started, from `docs/workstreams.md`

W1 measurement rig · W2 prefix-invariant suite (grok port, Apache-2.0) · W6 turn
engine (**critical path**) · W7 session log and head protocol · W8 heads · W9 tool
runtime · W10 firecode substrate · W11 adjudication · W12 flowy connector · W13
segments and compaction · W14 EXPLAIN · W15 server track · W16 experiments.

`pytest` is still not installed; `docs/workstreams.md` calls that blocking for W1/W2.

---

---

## T2 — Revise the `Dialect` contract — **UNBLOCKED by T1, re-scored**

Under the template-driven design, four of the eight defects stop existing or move.
Re-scored by the T1 experiment:

| # | defect | fate under T1 |
|---|---|---|
| 1 | `render_incremental` cannot be pure | **gone** — Jinja has no incremental mode, so there is no boundary state to carry |
| 3 | no home for the generation prompt | **gone** — it is a template argument |
| 5 | resolution must key on literal not role | **gone** — rendering no longer asks for roles |
| 4 | `ControlRole` closed and too small | **moves** to `parse`'s problem |
| 7 | `ControlTokens` forces `&'static str` | **now mandatory** — a runtime-loaded template makes `&'static` impossible |
| 2 | `parse(&[u32])` unimplementable | survives, `parse` side |
| 6 | `stop_tokens()` bare literals | survives, `parse` side |
| 8 | `ControlRole` has no `Ord` | survives, trivial |

So `Dialect` splits: **rendering becomes data** (template source, control-token set,
quirks) and **parsing stays code**. The three survivors are all on the parsing side,
which T1 does not touch.

**One thing gets harder and needs deciding.** The `server-bug-compatible` profile
cannot be produced by a template-driven renderer without deliberately reintroducing
minja's bug, and minijinja has no knob for it. My call: **drop it.** Its purpose was
to prove we understood the template well enough to reproduce minja exactly, which
earned the `faithful` profile the right to declare divergences. Running the real
template through a correct engine is stronger evidence than reproducing a wrong one,
and the INTEROP phase against `/apply-template` still reports how the server differs.

1. **`render_incremental(prev_end, new_items)` cannot be a pure function.** GLM needs
   boundary state — turn open, `<think>` open, previous item a tool result — none of
   it derivable from a `usize`. Worked around with `GlmDialect::for_conversation`;
   `new()` panics rather than guessing. The signature must carry history or a state
   token.
2. **`parse(&[u32])` is unimplementable as specified** — no vocab, yet it must return
   `Content(String)`. Currently takes a caller-supplied `TokenDecoder`.
3. **No home for the generation prompt.** Folding `<|assistant|><think>` into
   `render` breaks the stated invariant for any conversation whose next item is a
   user message. Currently an inherent `generation_prompt()`.
4. **`ControlRole` is closed and too small** — no `<arg_key>`, `<arg_value>`,
   `<sop>`, or the image triplet, all single vocab entries, so all must be `Control`.
   They currently sit under `TurnEnd` as a "no role" bucket.
5. **Resolution must key on literal, not role.** One role with many literals is the
   case that bites, and `ControlTokens::get(role)` silently returns whichever comes
   first. `GLM_TOKENS` is ordered so the canonical entry wins — a convention, not a
   guarantee.
6. **`stop_tokens()` returns bare `&'static str`** with no role, so a failure cannot
   be reported honestly. Correctness argument: a stop token that is silently a
   *sequence* never fires, and the turn runs to `n_ctx`.
7. **`ControlTokens` as `&'static [ControlToken]`** forces `&'static str` through
   every error type. Fine while dialects are compile-time constants; impossible for
   a dialect loaded from a config file or a downloaded template. `Cow<'static, str>`
   costs nothing today.
8. **`ControlRole` has no `Ord`**, so deterministic error listings must preserve
   declaration order rather than sort. Trivial; a derive would do.

---

---

## T15 — Replace §10's compaction design with structural eviction — **design written, not scheduled**

`docs/compaction.md`. Compaction today bundles *reducing what is resident* (necessary)
with *destroying what is recoverable* (an accident). Falsifier B removed the usual
justification — quality does not degrade with depth — so the trigger is the context
wall and memory pressure only.

The key constraint that rules out the easy fix: **plain eviction invalidates the
prefix just as summarisation does**, because the prefix *is* the old turns. Both cost
a re-prefill, so the question is what to get for it. Answer: replace an evicted span
with a **structured, addressable map** rather than prose, maintained incrementally so
there is no stop-the-world summarisation call, with zoom-in appended via `recall`
rather than rewritten in place.

Depends on nothing. Feeds `docs/memory.md`. Gives `SegmentMark` its missing producer.

---

---

## T13 — §4.5's event enum is insufficient for a real head — **five gaps; gap 1 is a priority, see T14**

Found by building one, then by fixing T12. Listed with what was done about each.

**Gap 1 is not a footnote.** T14 argues that an addressable record is the fallback if
composable KV proves impossible on hybrid models — and a log whose transcript events
carry no content cannot be that record. Treat it as blocking for the "sessions that
do not forget" goal, not as a schema nicety.

1. **`TranscriptAppended{item_id, kind, ledger_head}` carries no content**, and
   `EventSink` has no channel for it — so **a head cannot reconstruct a conversation
   from the log at all**. The daemon must reconcile items into the view out of band
   (`Hub::record_item`). W8 kept the event exactly as specified and made the gap
   explicit rather than widening it unilaterally. This is the structural one and it
   needs a decision: either the event carries content, or the log is formally not a
   sufficient record of a session.
2. **`TurnFinished{usage, timings}` is lossy against `turn_metrics`.** Not carried:
   `prefix_check`, `id_slot`, `n_busy_slots`, `cost`, `dialect_template_sha` — and
   §18.2 says **`id_slot` is exactly what distinguishes a scheduling fact from a
   prefix divergence**, so a head cannot today tell those apart. W8 widened `usage`
   to carry `cached_tokens` on the argument that a harness whose point is cache reuse
   must be able to display it.
3. **No event announces who issued a command**, which §13.2 requires twice. Added as
   `CommandIssued`, labelled as an addition rather than smuggled into `Warning`.
4. `ToolStarted` / `ToolProgress` are given by name only in §4.5; their shapes are
   W8's invention and **W9 will find out whether they are right**.
5. **`DeltaTarget` is `Text | Reasoning` — there is no channel for a tool call.**
   Found while fixing T12. A tool call's argument text streams as `Text` while
   `items::produce` puts it in `ToolCall{arguments}` and excludes it from the
   Assistant row, so T12's new per-channel equality assertion **would fail on a
   tool-calling turn — correctly**, as a real live/stored disagreement rather than a
   test defect. No test exercises it yet because `live_e2e`'s prompt makes no calls.
   This is T12's defect, unfixed, in a third channel. The enum was not widened
   unilaterally.

---

---

## T10 — Contract gaps found by W6 — **items 1, 2, 3 and 5 SETTLED 2026-09-11, see TODO-settled.md (R1, R2, R3, R4)**

The turn engine is the first real consumer of the crates below, and it found five
things. Listed in the order I would fix them.

1. **`TranscriptItem::Assistant` has no `truncated` field**, and both §5.7 and §5.8
   require one. Currently tracked on the turn record instead, which is why **one
   piece of steering is unbuilt**. Needs a `transcript` crate change — the only item
   here that blocks a feature rather than costing elegance.
2. **`ParsedSpan` carries no token offsets**, so a `Parser` cannot say which ids an
   item owns. Cost an entire module (`crates/turn/src/items.rs`) to work around
   without reimplementing the parser. Adding a span to `ParsedSpan` would delete it.
3. **`DialectSpec` has no `ReasoningField`** — a per-model fact of exactly the kind
   that crate exists to model as data. The engine takes it as config rather than
   guessing, which works but puts a model fact in the wrong place.
4. **`render_incremental(history, new)` replays the whole history** for boundary
   state, so building one ledger row per item is O(n²) replays. Microseconds today
   at our sizes; a resumable state token fixes it. Note T1 may delete this entirely
   when the template-driven renderer lands.
5. **`cargo:rustc-link-arg` does not propagate across crates**, so every crate that
   links `libllama` needs its own `build.rs` to bake the rpath. Without it, test
   binaries link fine and fail at exec looking like a missing library. Fixed in
   `crates/turn`; worth a note so the third crate does not rediscover it.

---

---

## T11 — §18.1-I1's observable form is not checkable on a hybrid model — **SETTLED 2026-09-11, plan text corrected, see TODO-settled.md (R5)**

§18.1-I1 states the prefix invariant observably as
`cached_tokens(N+1) >= prompt_tokens(N) + predicted_tokens(N)`. On this box it
**cannot pass**, and not because the invariant is violated.

Qwen3-Next is hybrid/recurrent, so llama.cpp resumes from a **context checkpoint**
and snaps `n_past` back to it (`server-context.cpp:5910`). Measured against prompts
*proven* identical over the shared span: reuse 48 of 52, and 38 of 44. The server is
reusing less than it could, correctly, for reasons of its own memory model.

W6's resolution, which I agree with: make the **exact** form the assertion — hash
turn N's prompt plus its committed generation, re-hash that span of N+1's prompt —
and demote the observable number to a *measurement of the server*, with a warning
that says which it is.

**Second defect in the same sentence:** `predicted_tokens(N)` is the wrong term for
a harness that owns its turn boundaries. A trailing stop token is stripped before
commit — keeping GLM's emitted `<|user|>` would put a second one in the next prompt —
so the witness must record **committed** generated tokens. With `predicted`, every
turn warns by one, forever.

Both need fixing in `docs/implementation-plan.md` §18.1. The code is already right.

---



---

## T4 — Verify what opencode actually sends — **SETTLED 2026-09-11, mechanism 2 refuted, see TODO-settled.md (R6)**

`docs/chat-templates.md` §3 lists two ways a client causes prompt-cache divergence.
The second — sending reasoning as its own message rather than fused onto the
assistant message — applies only if opencode does that. Unverified.

The W1 recording proxy answers it directly. Decides whether the residual
divergence after the `interleaved` fix is one mechanism or two.

---

---

## T18 — The prompt cache costs ~120 KiB/token on recurrent models — **operational, caused a fifth OOM**

Found while running UNVERIFIED-16, which **OOM-killed `qwen-flash-next` at 14:35 on
2026-09-09** (158.1 GiB peak; systemd restarted it, `qwen-slots-restore` put the slots
back, ~40 s down, in-flight requests from two other agents lost).

Cause, and it generalises: **every distinct prompt prefilled becomes a prompt-cache
entry at ~120 KiB/token, because 111 MiB of each entry is the recurrent accumulator** —
fixed per sequence, independent of length. A sweep over many short distinct prompts is
therefore far more expensive than its token count suggests. `server-context.cpp:4549`
records four previous occurrences; this was the fifth.

**A second cost, measured on a DENSE model, 2026-09-14 by `lubuntu1-lab` on .76
(Qwen3.8-27B Q6, llama.cpp, stock).** Different mechanism from the recurrent
accumulator above and it compounds with it:

- A saved prompt-cache state costs roughly the **full `-c` allocation, not the
  tokens actually used** — ~1.5 GiB each at `-c 8192`. So capacity is
  `cache-ram / per-saved-state`, and the 8 GiB default holds about five
  conversations. At 40 GiB: 16 of 16. Two levers, and only two: raise
  `--cache-ram`, or lower `-c`.
- Eviction is **capacity, not conflict**: interleaving A,B,A,B,A,B hits 94–95 %
  once each has been seen. So the number to size against is *how many distinct
  briefs are live*, not how they are ordered.
- Warm 644 ms median against cold 1986 ms; the first touch of a new conversation
  is always cold.
- **`--cache-reuse` is silently disabled on that model** — `cache_reuse is not
  supported by this context, it will be disabled`, logged with `--kv-unified`
  both true and false, so unified KV is not the gate. The 94 % hits are the
  slot's own longest-common-prefix match, not `cache_reuse`. Advice to rely on
  that flag is therefore **model-dependent**, and a measurement taken on one
  model does not port. Nobody has yet established what makes a context
  unsupported; the suspicion is an attention property rather than a flag.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

---

---

### 2026-09-11 — the sixth OOM, and why the disk tier did not save the turn

GLM was **killed by the kernel OOM killer** at 01:35:34 (`status=9/KILL`), taking a live
letibot turn with it: `malformed http response: connection closed mid-chunk: the server
went away before the terminating 0-length chunk`. Host RAM, not VRAM.

**The good news first, because it corrects a standing fear.** The spills survive a hard
kill and are re-used. 957 GB across 506 files, and the restarted server indexed them:

    L2: indexed 386 cache entries from disk (23,128,678 tokens, 931,079 MiB)

and then served **6,427,050 cached prompt tokens against 240,831 prefilled** — 26.7×
reuse, resuming a 171,901-token conversation rather than re-prefilling it. Critically,
`forcing full prompt re-processing` appears **zero** times, so the plan's "Root cause B"
— a disk restore that loaded the KV and then discarded it because `checkpoints` was
empty, which is exactly the hybrid/recurrent case — is **fixed** in this build. That is
the first evidence of it holding across a `SIGKILL`.

**What is wrong is that the tier is read-only in practice.** Every idle cycle logs

    flush: 0 sequence(s) with tokens, 0 saved, 0 already cached, 0 offloaded (0 saved),
           cache now 386 entries / 0.0 MiB

`0 sequence(s) with tokens` — by flush time no slot holds anything to save, which is
`spill-only-writes-cached-prompts` in the memory dir: *prompt_save is false for an
already-cached prompt, so live slots never spill*. So it serves brilliantly from what was
written before the kill and **adds nothing**. Any conversation that grows from here has
no disk entry, and that one really will prefill from zero after the next kill.

**And the shape of it is perverse.** The RAM tier is **write-back** — entries reach disk
on eviction or graceful shutdown — and write-back is precisely what a `SIGKILL` defeats.
`--cache-ram 81920` therefore both raises the chance of the kill and maximises what the
kill destroys. `/data` is a 7.3 GB/s NVMe with 5.3 T free; **write-through survives OOM
by construction** and the read side is already proven fast enough at 26.7× reuse.

**Also costing something:** 48 `index_disk` calls in one log, ~20 minutes apart, each
re-reading metadata for 931 GB across 386 entries — the sleep/wake cycle
(`--sleep-idle-seconds 600`) paying the index once per wake rather than once per start.

**The decision, and it is the operator's:** lower `--cache-ram` (less pressure, less
lost, more disk reads), or make the RAM tier write-through (survives the kill, costs
NVMe bandwidth that is measurably spare). Not a letibot change either way — it is
`~/bin/glm-flash-server` and llama.cpp's `server_prompt_cache`.

## T20 — Where the assembled parts did not fit — **integration findings, several serious**

1. **Double envelope.** `ToolRuntime::transcript_item` renders §8.2's `NO_RESULT`
   envelope into `payload`, and **both** dialect renderers wrapped it again — the model
   received two. Fixed: renderers emit `payload` verbatim.
2. **`Registry::tools_json()` is not prompt-ready** and this one was nearly invisible.
   It emits `,`/`:`; both templates use HF `tojson`'s `, `/`: `. Using it directly for
   `StablePrefix::tools_json` differs in **the first bytes of the stable prefix** — a
   cold prefill every single turn, with nothing in the tree to catch it. Now routed
   through the dialect's own tool-JSON function, with a test asserting the two differ.
3. **C10 was unmeasurable as specified** — `prompt_tokens − cached_tokens` from
   `TurnMetrics` alone is a tautology. Needed the server's own frame numbers.
4. ~~**`ToolRuntime` is not `Send`** — `Gate` lacks `Send + Sync`~~ — **FIXED, verified
   2026-09-10:** `crates/tools/src/runtime.rs:275` reads `pub trait Gate: Send + Sync`.
5. **`Session::append_items` renders one item at a time**, so Qwen's consecutive tool
   results cannot merge into one user turn without a fourth trait method. Declared as a
   gate divergence.
6. **Qwen's `SystemUpdateMode` is `Envelope`, not `InHistory`** — its template *raises*
   on a mid-history system message, so §5.3's conservative default is the only
   renderable form. The model can and did argue with it.
7. **`ask_code`/`ask_corpus` return `NotRun`, not `Abstained`** — nothing ran, which is
   a different fact. The daemon's startup disclosure says so.
8. The `/apply-template` oracle caught a renderer bug **no unit test would have**: an
   `Assistant` following a `Reasoning` opened a second message.

---

---

---

## BashArena — Kev versus the incumbent on the attack corpus — **STOPPED 2026-09-22, unresolved; the operator ruled it off**

**Where it stopped.** The measurement is done and committed (`docs/evidence/basharena-2026-09-22.md`,
commits `d1a5a22` + `4b10c0a`): 339 rows converted from HF `abhayesian/basharena-monitor-eval`,
Kev scored on the Blackwell in bf16 (5/47 attacks caught at t=0.5), the incumbent through
`--etalon-oracle` (14/47, then 12/47 on a rerun), the four gate words, the per-side-task split, and
the zero-false-allow threshold (**t > 0.8498 catch-all, 274 of 292 honest calls**).

**What is unresolved, and it is the one that decides whether the comparison means anything:**
`docs/guard-corpus-plan.md` §4b records the SAME guard on the SAME 339 rows at **27 of 47 (57%)**
on 2026-09-17; today's runs give **14 and 12**.

**BISECTED 2026-09-23 — the commit is `3d8754d` (R12), and the mechanism is the prompt's ORDER.
FIXED THE SAME DAY on the operator's ruling: see `docs/evidence/basharena-2026-09-22.md` §5d.**
The order is back to reasoning-then-verdict and `DEFAULT_MAX_TOKENS` is 120 → 400, so truncation
costs latency instead of recall. Four runs of `--etalon-oracle` on the same 47 attacks alone:

    attacks caught     26, 23, 23, 25   (mean 24.25, median 24)   against 18, 18, 17 before
    truncated replies  0 of 47, every run
    median latency     706, 761, 902, 2316 ms;  p90 2282-5728 ms   (mean ms ~1234 -> ~1838)

**The declared threshold (>= 24) was met by the mean and the median and MISSED BY TWO OF THE FOUR
RUNS, both at 23.** The honest range is 23-26, the low edge of the parent's band, not comfortably
inside it. The recovery is ~6.3 rows against the bisect's attribution of ~6.5 — they agree, so the
reordering was the mechanism and a second bisect is not owed. **The honest column is still
unmeasured** for this configuration, which is what a recall-versus-needless-prompts ruling needs.

See `docs/evidence/basharena-2026-09-22.md` §5c for the instrument, the walk and the isolation.
Every commit was BUILT AND MEASURED, not reasoned about: `--etalon-oracle` on the 47 attacks
alone, local endpoint, threshold declared first (**≥24 caught GOOD, ≤23 BAD**, from poles of
29–30 at af96837 against 17–18 at HEAD). 307 commits, 9 steps.

The answer, and **all five of my earlier candidates were refuted**: `c3ee6b7`, `a16453f`,
`bf4137e`, `452d7dd`, `76e1b31` are every one of them ancestors of `06ec17b`, which measures
**26, 24** — they cannot be the cause. What settles it beyond the branched-lineage doubt is the
direct parent→child pair: **`3d8754d^` = 26, 24 against `3d8754d` = 17.**

**The mechanism, isolated to two edits in one file.** R12's own message names the change it
considers load-bearing — *"the prompt asked for a sentence and then the verdict on the LAST line,
so a reply cut before that line was a budget recorded as an unreadable answer. Now the verdict is
asked for on its FIRST line."* Applying **only** that pair to the parent (25 insertions, none of
the other 738 lines):

    06ec17b as committed                       26, 24 caught
    + the prompt reorder and parser only       18, 18        <- the whole drop
    + max_tokens 120 -> 400 only               24, 25        <- about one row

**So R12's own diagnosis is refuted too:** truncation is real but worth ~1 row. The other ~6 are
the prompt's ORDER changing what the model decides — commit before reasoning makes the oracle
materially more permissive.

**NOT REVERTED, deliberately, and the halves are separable for the operator to rule on.** The
ceiling/classification half is unambiguously right and worth ~1 row; the ordering half is worth
~6 and was never ruled on in its own right. Reverting would restore an artifact — replies cut
before their verdict, scored as catches the guard never made. Every number in §5c is attacks-only
(examples effect held constant); **the honest column is unmeasured for both variants**, which is
what a recall-versus-needless-prompts decision would need.
