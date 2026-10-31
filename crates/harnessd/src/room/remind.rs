//! **The room's reminders, as rows on the session's own board** — the pass that closes
//! `docs/room.md` § *Still owed* item 3.
//!
//! The operator's ask, 2026-10-29, is the whole of why this exists:
//!
//! > *"just like with todo top level session get reminds (with ability to mark waits_on) so they
//! > dont occupy it for ever"*
//!
//! The room already answers the question — `Room::reminders` is one row per live claim, with the
//! wait the holder declared and whether its beat has gone quiet — and until this module nothing
//! turned that answer into anything the nag reads. So a holder was reminded only if the model
//! thought to ask, which is a reminder for the sessions that do not need one.
//!
//! # Why the pass lives on the daemon's idle tick, and not on a clock of its own
//!
//! The board is the harness's, and a board may only be written from the thread that drives the
//! harness. The room's connection has threads of its own — a reader and the beat — and neither may
//! touch it. So the pass is a *moment*, not a thread: `Sessions`' per-session idle arm
//! (`OwnWork::TimedOut`, the same place `deliver_due_nags` runs) asks for it on the clock
//! [`ROOM_PASS_EVERY`] names, and that is the only caller in production.
//!
//! The socket call is bounded by the client's own `REPLY_TIMEOUT`, so a coordinator that has
//! stopped answering costs one idle window rather than the worker.
//!
//! # One row per item, and the words do not move
//!
//! A row's identity on this board is its exact `content` — that is the key `todo_write` resolves,
//! the key an edge is written against, and the key the room pass retires by. So the row's words
//! carry **no clock and no state**: `you hold `ram` #3 — 8 GB` is the same sentence at one second
//! and at an hour, and everything that moves (the wait, the stale beat) moves in the row's
//! `needs` edge and its status instead. A pass that wrote `… for 4m` would add a new row every few
//! seconds, which is the nag machine this design exists to avoid.
//!
//! # Quiet while the room works, loud when the holder must act
//!
//! The row carries a [`TodoNeed::Room`] edge **exactly while the room is working on the item**:
//! the holder told the room what it is waiting for (`room waiting`) and its claim's beat is fresh.
//! While the edge is there the idle check is silent about the row (`waits_on_the_room`) — the
//! holder has said what it waits on, and the room is carrying it, so there is nothing the plan
//! could do. Take the edge away and the row is ordinary work: the check names it, and the holder
//! is told what it holds and that it must finish or release it — *"so they dont occupy it for
//! ever"*.
//!
//! Two things make it loud, and they are different facts:
//!
//! * **no declared wait** — nothing outside is going to move this item, so the hold is the
//!   holder's own work;
//! * **a stopped beat** — the room will take the item back at its next sweep, so the holder has
//!   one window in which to `beat` or `abandon`.
//!
//! # Top-level sessions only
//!
//! The room refuses a subagent at its door and [`super::link`] gives one no connection at all, so
//! a subagent has no `SessionRoom` and this pass returns nothing for it. That is the same rule
//! twice, which is what the brief asks for: *"a subagent never gets a row, and never gets a room
//! row it did not hold"*. The second half is [`rows`]' filter — a reminder about somebody else's
//! claim is not this session's row, and `reminders` is one read for the whole room.

use letibot_room::Reminder;
use letibot_tokencore::store::{TODO_TITLE_MAX, TodoBy, TodoItem, TodoNeed, TodoStatus};

/// **How often the daemon asks the room what this session holds.** Five seconds, the room's own
/// sweep interval (`Server`'s clock) and a third of the default beat: a claim, a wait and a
/// `done` all reach the board within one sweep of happening, and the socket cost is one frame per
/// session per tick.
///
/// It is not smaller because the row only changes when the room's answer does — the door is a
/// compare-and-set — and it is not larger because the loud window after a stopped beat is the
/// lease's tail, and a pass that ran once a minute would miss it.
pub const ROOM_PASS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// **The rows this session owes, one per item it holds** — the whole of the pass's decision, with
/// no daemon, no socket and no board in it.
///
/// `me` is this session's id and `reminders` is the room's answer for the whole box: a reminder
/// about another session's claim is not this session's row, and a room whose `reminders` answer
/// names an id this daemon has never met is a row nobody's board should carry.
pub fn rows(me: &str, reminders: &[Reminder]) -> Vec<(u64, TodoItem)> {
    let mut out: Vec<(u64, TodoItem)> = reminders
        .iter()
        .filter(|r| r.session == me)
        .map(|r| {
            let working = the_room_is_working_on(r);
            (
                r.holding.item,
                TodoItem {
                    content: content(&r.holding.tag, r.holding.item, &r.holding.body),
                    // **The session's own row**, which is what makes the nag say *yours* and what
                    // lets the model dispose of it with `todo_write`'s `update`. It is not the
                    // operator's, it is not a parent's, and it is not the merge queue's.
                    by: TodoBy::Model,
                    status: TodoStatus::InProgress,
                    when: None,
                    needs: vec![TodoNeed::Room {
                        item: r.holding.item,
                        working,
                    }],
                },
            )
        })
        .collect();
    // Sorted by item so the board reads in the room's own order and the door's compare-and-set
    // sees the same vector for the same state. `reminders` already sorts by `(session, item)`;
    // this is the guarantee rather than the observation.
    out.sort_by_key(|(item, _)| *item);
    out
}

/// **Whether the room is working on this item** — the one predicate that decides quiet or loud.
///
/// Two facts, and the row is quiet only when BOTH hold:
///
/// * **the holder declared a wait** (`room waiting { item, text }`) — it said what it is waiting
///   for, and clearing the wait (`text: ""`) is how it says it is done waiting;
/// * **the claim's beat is fresh** — `Room::sweep` will not take the item back at its next pass.
///
/// Either one alone is a hold nobody is moving, which is the thing the operator asked to be
/// reminded about.
pub fn the_room_is_working_on(r: &Reminder) -> bool {
    !r.stale && r.holding.waits_on.is_some()
}

/// **The row's words** — `you hold `ram` #3 — 8 GB`.
///
/// Stable by construction: the tag, the number and the poster's body, and nothing that moves. The
/// body is cut to keep the whole inside [`TODO_TITLE_MAX`], which is the length the board's own
/// writers are held to — a row is read in a pane and quoted back to a tool, and neither wants a
/// paragraph.
pub fn content(tag: &str, item: u64, body: &str) -> String {
    let head = format!("you hold `{tag}` #{item} — ");
    let room = TODO_TITLE_MAX.saturating_sub(head.chars().count());
    let body = one_line(body);
    let body = if body.chars().count() > room {
        let mut cut: String = body.chars().take(room.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        body
    };
    format!("{head}{body}")
}

/// The body as one line of single spaces — a body is a model's own sentence and may hold a
/// newline, and a newline in a row's `content` is a row that draws as two and resolves as one.
fn one_line(body: &str) -> String {
    let said: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if said.is_empty() {
        "held in the room".to_string()
    } else {
        said
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_room::{Holding, Wait};

    fn reminder(session: &str, item: u64, wait: Option<&str>, stale: bool) -> Reminder {
        Reminder {
            session: session.to_string(),
            holding: Holding {
                item,
                tag: "ram".into(),
                body: "8 GB, ~10 min".into(),
                claim_ms: 1_000,
                beat_ms: 2_000,
                held_ms: 4_000,
                waits_on: wait.map(|t| Wait {
                    text: t.to_string(),
                    since_ms: 1_500,
                }),
            },
            stale,
        }
    }

    /// **The words carry no clock**, and that is the whole of *one row per item, updated not
    /// appended*: the board's key is the content, so a sentence that moved with `held_ms` would be
    /// a new row every pass — the nag machine.
    #[test]
    fn the_words_do_not_move_with_the_clock() {
        let mut a = reminder("s-me", 3, None, false);
        let b = reminder("s-me", 3, None, false);
        assert_eq!(
            content(&a.holding.tag, a.holding.item, &a.holding.body),
            content(&b.holding.tag, b.holding.item, &b.holding.body)
        );
        assert_eq!(
            content("ram", 3, "8 GB, ~10 min"),
            "you hold `ram` #3 — 8 GB, ~10 min"
        );
        // …and it does not move when the facts around it do.
        a.holding.held_ms = 900_000;
        a.holding.beat_ms = 950_000;
        assert_eq!(
            content(&a.holding.tag, a.holding.item, &a.holding.body),
            "you hold `ram` #3 — 8 GB, ~10 min"
        );
    }

    /// **A long body is cut inside the title cap**, because a row is quoted back to a tool and
    /// drawn in a pane; a body with a newline is one line, because a newline in `content` is a row
    /// that draws as two and resolves as one.
    #[test]
    fn a_body_is_one_line_and_fits_the_title_cap() {
        let long = "x".repeat(400);
        let said = content("ram", 7, &long);
        assert!(
            said.chars().count() <= TODO_TITLE_MAX,
            "{} chars",
            said.chars().count()
        );
        assert!(said.ends_with('…'), "{said}");
        assert_eq!(
            content("ram", 7, "two\nlines  here"),
            "you hold `ram` #7 — two lines here"
        );
        assert_eq!(
            content("ram", 7, "   "),
            "you hold `ram` #7 — held in the room"
        );
    }

    /// **Quiet while the room works, loud when the holder must act** — the three cases, and the two
    /// that make it loud are different facts. The edge is on every row; `working` is the whole of
    /// the difference, and it is what the idle check reads.
    #[test]
    fn the_edge_says_whether_the_room_is_working_on_the_item() {
        let quiet = rows("s-me", &[reminder("s-me", 3, Some("the suite"), false)]);
        assert_eq!(quiet.len(), 1);
        assert_eq!(
            quiet[0].1.needs,
            vec![TodoNeed::Room {
                item: 3,
                working: true
            }],
            "a declared wait on a fresh beat is the room working on it"
        );
        assert!(
            letibot_tools::builtins::todo::waits_on_the_room(&quiet[0].1),
            "and that is the row the idle check stays silent about"
        );

        // No declared wait: nothing outside moves it, so it is the holder's own work.
        let loud = rows("s-me", &[reminder("s-me", 3, None, false)]);
        assert_eq!(
            loud[0].1.needs,
            vec![TodoNeed::Room {
                item: 3,
                working: false
            }],
            "the edge is still there — retirement needs it — and it says nothing is coming"
        );
        assert!(!letibot_tools::builtins::todo::waits_on_the_room(
            &loud[0].1
        ));

        // A stopped beat: the room takes it back at the next sweep, so say so now.
        let stale = rows("s-me", &[reminder("s-me", 3, Some("the suite"), true)]);
        assert!(
            !letibot_tools::builtins::todo::waits_on_the_room(&stale[0].1),
            "a beat that stopped is the holder's to rescue, whatever it was waiting for"
        );
    }

    /// **The row is the session's own** — `by: yours`, in progress, with no `when` — so the nag
    /// says *yours* and the model may dispose of it with `update`. Never the operator's, and never
    /// the merge queue's, whose rows a session cannot move.
    #[test]
    fn the_row_is_the_sessions_own() {
        let r = &rows("s-me", &[reminder("s-me", 3, None, false)])[0].1;
        assert_eq!(r.by, TodoBy::Model);
        assert_eq!(r.status, TodoStatus::InProgress);
        assert!(r.when.is_none());
    }

    /// **A subagent gets no row it did not hold, and a session gets no row about somebody else's
    /// claim.** `reminders` is one read for the whole room, so the filter is the second half of
    /// the top-level rule — the first being that a subagent has no room connection at all.
    #[test]
    fn only_this_sessions_own_claims_become_rows() {
        let all = [
            reminder("s-other", 1, None, false),
            reminder("s-me", 2, None, false),
            reminder("s-other", 3, Some("the suite"), false),
        ];
        let mine = rows("s-me", &all);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].0, 2);
        assert!(rows("s-subagent", &all).is_empty());
        assert!(rows("s-me", &[]).is_empty());
    }

    /// Two items are two rows, in the room's order, so a session holding two tags is reminded of
    /// both and the door's compare-and-set sees one vector per state.
    #[test]
    fn one_row_per_item_in_item_order() {
        let mine = rows(
            "s-me",
            &[
                reminder("s-me", 9, None, false),
                reminder("s-me", 4, None, false),
            ],
        );
        assert_eq!(mine.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![4, 9]);
    }
}
