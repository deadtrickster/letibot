//! The room's reminders, as rows on the session's own board - the pass that closes
//! `docs/room.md` item 3: *"just like with todo top level session get reminds (with
//! ability to mark waits_on) so they dont occupy it for ever"*.

use letibot_room::Reminder;
use letibot_tokencore::store::{TodoBy, TodoItem, TodoNeed, TodoStatus};

/// How often the daemon asks the room what this session holds. Five seconds - the room's own
/// sweep interval and a third of the default beat - so a claim, a wait and a `done` all reach the
/// board within one sweep, and the loud window after a stopped beat is not missed.
pub const ROOM_PASS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// The rows this session owes, one per item it holds - the whole of the pass's decision, with
/// no daemon, no socket and no board in it. `me` is this session's id and `reminders` is the
/// room's answer for the whole box, so a reminder about another session's claim is filtered out.
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
                    // The session's own row, which is what makes the nag say *yours* and what
                    // lets the model dispose of it with `todo_write`'s `update`.
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
    // sees the same vector for the same state.
    out.sort_by_key(|(item, _)| *item);
    out
}

/// Whether the room is working on this item - the one predicate that decides quiet or loud.
/// Both facts must hold: a declared wait, and a fresh beat. `waits_on` is the only field that can
/// make a row quiet; `said` is not - an acknowledgement is *I am on it*, the opposite of waiting.
pub fn the_room_is_working_on(r: &Reminder) -> bool {
    !r.stale && r.holding.waits_on.is_some()
}

/// How much of the poster's body a reminder row carries. A reminder is a nag line, one row of a
/// board drawn in a pane, so a long body is one short line with an ellipsis rather than a row
/// that buries the board.
const BODY_CUT: usize = 135;

/// The row's words - `you hold `ram` #3 - 8 GB`. Stable by construction: the tag, the number
/// and the poster's body, and nothing that moves. The body is cut to keep the whole row inside
/// [`BODY_CUT`].
pub fn content(tag: &str, item: u64, body: &str) -> String {
    let head = format!("you hold `{tag}` #{item} — ");
    let room = BODY_CUT.saturating_sub(head.chars().count());
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

/// The body as one line of single spaces - a body is a model's own sentence and may hold a
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
                said: None,
            },
            stale,
        }
    }

    /// The same claim, having acknowledged itself - `seen #3; working on it` in [`Holding::said`]
    /// which is what a client's claim writes and is not a wait.
    fn acknowledged(mut r: Reminder, words: &str) -> Reminder {
        r.holding.said = Some(Wait {
            text: words.to_string(),
            since_ms: 1_000,
        });
        r
    }

    /// The words carry no clock, and that is the whole of *one row per item, updated not
    /// appended*: the board's key is the content, so a sentence that moved with `held_ms` would be
    /// a new row every pass.
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

    /// A long body is cut to one short line, because a row is quoted back to a tool and drawn in
    /// a pane; a body with a newline is one line, because a newline in `content` is a row that
    /// draws as two and resolves as one.
    #[test]
    fn a_body_is_one_line_and_cut_to_one_short_row() {
        let long = "x".repeat(400);
        let said = content("ram", 7, &long);
        assert!(
            said.chars().count() <= BODY_CUT,
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

    /// Quiet while the room works, loud when the holder must act - the edge is on every row
    /// and `working` is the whole of the difference the idle check reads.
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

    /// An acknowledgement is not a wait - the defect that made `main` red: a reminder that read
    /// *seen #3; working on it* as a declared wait went quiet on every claim there is, so the nag
    /// never fired. The two facts live in two fields and only one of them is a reason to be silent.
    #[test]
    fn a_claim_that_acknowledged_itself_is_still_the_holders_own_work() {
        let acked = acknowledged(reminder("s-me", 3, None, false), "seen #3; working on it");
        assert!(
            !the_room_is_working_on(&acked),
            "the holder said it is doing the work, which is the opposite of waiting on something \
             else: nothing outside moves this item"
        );
        assert_eq!(
            rows("s-me", &[acked])[0].1.needs,
            vec![TodoNeed::Room {
                item: 3,
                working: false
            }],
            "so the edge says so, and the idle check names the row"
        );

        // And the two together: a holder that said what it is doing AND what it waits on is
        // waiting - the acknowledgement does not cancel a wait, it simply is not one.
        let both = acknowledged(
            reminder("s-me", 3, Some("the suite"), false),
            "seen #3: the 40-minute suite",
        );
        assert!(the_room_is_working_on(&both));
    }

    /// The row is the session's own - `by: yours`, in progress, with no `when` - so the nag
    /// says *yours* and the model may dispose of it with `update`.
    #[test]
    fn the_row_is_the_sessions_own() {
        let r = &rows("s-me", &[reminder("s-me", 3, None, false)])[0].1;
        assert_eq!(r.by, TodoBy::Model);
        assert_eq!(r.status, TodoStatus::InProgress);
        assert!(r.when.is_none());
    }

    /// A subagent gets no row it did not hold, and a session gets no row about somebody else's
    /// claim - `reminders` is one read for the whole room, so the filter is the second half of
    /// the top-level rule.
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
