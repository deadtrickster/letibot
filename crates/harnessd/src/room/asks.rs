//! The room's asks, as rows on this session's board and as the answers that release them - the
//! pass that runs beside the reminders' one, every five seconds.
//!
//! A question is not a message: `room ask` (through `todo_write`'s `wait_on`) makes the room hold an
//! ask, and the room is the only place it ends. So one half of this pass turns the asks OF this
//! session into rows it is nagged for, and the other half turns the asks BY it into the settled rows
//! and the operator cards that its own plan is waiting for. Nothing here opens a socket: it is the
//! decision, over two lists of `AskView`, and the harness does the waiting.

use std::collections::BTreeSet;

use letibot_room::{AskState, AskView};
use letibot_tokencore::store::{TodoBy, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::builtins::todo::AskSettle;

/// The rows this session owes an answer to: one per OPEN ask of it, keyed by the ask's id - the
/// pass's own edge (`Owed`), the way `Room { item }` keys a reminder row. A settled ask has no row:
/// answering is what takes the nag away, and a row that outlived its ask would nag for nothing.
///
/// The row is ready, so the idle check's own nag speaks it on the existing `TodoNagClock`: one nag
/// mechanism, already rate limited, and no second clock.
pub fn owed_rows(me: &str, of_me: &[AskView]) -> Vec<(u64, TodoItem)> {
    let mut rows: Vec<(u64, TodoItem)> = of_me
        .iter()
        .filter(|a| a.open() && a.target == me)
        .map(|a| {
            (
                a.seq,
                TodoItem {
                    content: owed_content(a),
                    // The session's own row, which is what makes the nag say *yours* and what lets
                    // the model dispose of one it can answer.
                    by: TodoBy::Model,
                    status: TodoStatus::InProgress,
                    when: None,
                    needs: vec![TodoNeed::Owed { ask: a.seq }],
                },
            )
        })
        .collect();
    // Sorted by ask so the board reads in the room's own order, and the door's compare-and-set sees
    // the same vector for the same state.
    rows.sort_by_key(|(ask, _)| *ask);
    rows
}

/// How long a question's own words may be in a row: a row is one line of a pane, and the whole
/// question is on the board's other side anyway.
const QUESTION_CUT: usize = 160;

/// The words of the row this session owes: who asked, which ask, the question, and both verbs -
/// because a model that was only told *somebody asked* would answer in chat and leave the asker's
/// plan blocked.
pub fn owed_content(a: &AskView) -> String {
    let question = a.question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut question: String = question.chars().take(QUESTION_CUT).collect();
    if a.question.chars().count() > QUESTION_CUT {
        question.push('…');
    }
    format!(
        "`{}` asked you #{} and waits on your answer: {} — answer with `room` `{{\"verb\": \
         \"answer\", \"ask\": {}, \"text\": …}}` or decline with `decline`",
        a.asker, a.seq, question, a.seq
    )
}

/// What the room settled, as the board applies it: `(ask, settle)` for every ask of MINE that has
/// ended. Open ones are not here, and neither is a settle this pass has already applied - the board
/// is idempotent over it (`TodoBoard::settle_ask` moves nothing twice).
pub fn settles(by_me: &[AskView]) -> Vec<(u64, AskSettle)> {
    by_me
        .iter()
        .filter_map(|a| settle_of(&a.state).map(|s| (a.seq, s)))
        .collect()
}

/// One ask's ending as the board's own words - the sentence the released row keeps.
fn settle_of(state: &AskState) -> Option<AskSettle> {
    Some(match state {
        AskState::Open => return None,
        AskState::Answered { text, .. } => AskSettle::Answered(text.clone()),
        AskState::Declined { text, .. } => AskSettle::Declined(text.clone()),
        AskState::Down { .. } => AskSettle::Down,
        AskState::Withdrawn { by, .. } => AskSettle::Withdrawn(by.clone()),
    })
}

/// The asks of MINE to take back: every one still open that no row of mine carries any more - it was
/// done, cancelled, detached or re-worded, and the target must stop being nagged by a question
/// nobody is waiting for. `carried` is [`letibot_tools::builtins::todo::TodoBoard::asks_on_the_board`].
pub fn withdrawals(by_me: &[AskView], carried: &BTreeSet<u64>) -> Vec<u64> {
    by_me
        .iter()
        .filter(|a| a.open() && !carried.contains(&a.seq))
        .map(|a| a.seq)
        .collect()
}

/// The line the model is told when one of its asks ends: `[answer] on "<row>": you asked
/// `serenedb` "which port?" - 5433.` It names the row, the peer and the words, because a settle
/// arrives while the model may be thinking about five rows at once.
pub fn settle_line(row: &str, ask: &AskView, settle: &AskSettle) -> String {
    let what = match settle {
        AskSettle::Answered(_) => "answer",
        AskSettle::Declined(_) => "declined",
        AskSettle::Down => "down",
        AskSettle::Withdrawn(_) => "withdrawn",
    };
    format!(
        "[{what}] on \"{}\": you asked `{}` \"{}\" - {}.",
        row.trim(),
        ask.target,
        ask.question.trim(),
        settle.reply(&ask.target)
    )
}

/// The warning class a settle is drawn under: an answer and a take-back are routine, and a target
/// whose session was deleted is the one ending a person must stop for.
pub fn settle_class(settle: &AskSettle) -> &'static str {
    match settle {
        AskSettle::Down => "ask_lost",
        _ => "ask_settled",
    }
}

/// The operator's question about an ask nobody can move - and the key the card is withdrawn by, so
/// the words may not carry a clock or a duration: the same state must spell the same sentence.
///
/// The two halves say different things because they are different situations: the asker is told its
/// target stopped, and the target is told the session waiting on it is not running.
pub fn escalation_question(ask: &AskView, me: &str) -> String {
    if ask.asker == me {
        format!(
            "`{}` stopped and `{me}` waits on its answer to \"{}\" - please restart it",
            ask.target,
            ask.question.trim()
        )
    } else {
        format!(
            "`{}` asked you #{} and is not running - it waits on your answer to \"{}\"; please \
             restart it",
            ask.asker,
            ask.seq,
            ask.question.trim()
        )
    }
}

/// What the pass raises and withdraws on the board: `(ask, question, on)` for every OPEN ask on
/// either side. `on` is the room's `escalated` - the other end has been gone past the grace - and
/// false is the card being withdrawn, which is what happens the moment it comes back.
pub fn escalations(by_me: &[AskView], of_me: &[AskView], me: &str) -> Vec<(u64, String, bool)> {
    by_me
        .iter()
        .chain(of_me.iter())
        .filter(|a| a.open())
        .map(|a| (a.seq, escalation_question(a, me), a.escalated))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(seq: u64, asker: &str, target: &str, state: AskState) -> AskView {
        AskView {
            seq,
            room: "host".into(),
            asker: asker.into(),
            target: target.into(),
            question: "which port?".into(),
            asked_ms: 1_000,
            gone_since_ms: None,
            escalated: false,
            state,
        }
    }

    fn open(seq: u64) -> AskView {
        ask(seq, "s-arango", "s-serenedb", AskState::Open)
    }

    /// A row per OPEN ask of mine, keyed by the ask, and none at all once it has ended - the nag is
    /// withdrawn by answering, not by marking the row done.
    #[test]
    fn one_row_per_ask_i_owe_and_none_once_it_has_ended() {
        let rows = owed_rows("s-serenedb", &[open(7), open(9)]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 7);
        assert_eq!(rows[0].1.status, TodoStatus::InProgress);
        assert_eq!(rows[0].1.needs, vec![TodoNeed::Owed { ask: 7 }]);
        let words = &rows[0].1.content;
        assert!(words.contains("s-arango"), "{words}");
        assert!(words.contains("#7"), "{words}");
        assert!(words.contains("which port?"), "{words}");
        // The model is told both doors, because answering in chat settles nothing.
        assert!(words.contains("answer"), "{words}");
        assert!(words.contains("decline"), "{words}");

        // A settled ask has no row: the answer IS the retirement.
        let answered = ask(
            9,
            "s-arango",
            "s-serenedb",
            AskState::Answered {
                text: "5433".into(),
                ms: 2,
            },
        );
        assert!(owed_rows("s-serenedb", &[answered]).is_empty());
        // Nor has somebody else's ask of a third session.
        assert!(owed_rows("s-other", &[open(11)]).is_empty());
    }

    /// The words do not move with the clock, and a long question is cut to one line: the row's key
    /// is its content, so a sentence that moved would be a new row every pass.
    #[test]
    fn the_owed_words_do_not_move_and_a_long_question_is_one_line() {
        let mut a = open(7);
        let first = owed_content(&a);
        a.gone_since_ms = Some(9_999);
        a.escalated = true;
        a.asked_ms = 5_000;
        assert_eq!(first, owed_content(&a), "the words carry no clock");

        let mut long = open(8);
        long.question = format!("two\nlines {}", "x".repeat(400));
        let said = owed_content(&long);
        assert!(
            said.chars().count() < 400,
            "one pane row, cut: {}",
            said.chars().count()
        );
        assert!(
            !said.contains('\n'),
            "one line, however the question was written"
        );
        assert!(said.contains("two lines"), "{said}");
        assert!(
            said.contains("or decline with"),
            "and the doors are still named"
        );
    }

    /// Every ending the room can reach is a settle the board applies, with the words the design
    /// spells - and the row keeps them, so the plan reads what it waited for.
    #[test]
    fn every_ending_becomes_the_settle_the_board_applies() {
        let answered = ask(
            1,
            "s-a",
            "s-b",
            AskState::Answered {
                text: "5433".into(),
                ms: 0,
            },
        );
        let declined = ask(
            2,
            "s-a",
            "s-b",
            AskState::Declined {
                text: "not my area".into(),
                ms: 0,
            },
        );
        let down = ask(3, "s-a", "s-b", AskState::Down { ms: 0 });
        let withdrawn = ask(
            4,
            "s-a",
            "s-b",
            AskState::Withdrawn {
                by: "operator".into(),
                ms: 0,
            },
        );
        let got = settles(&[open(5), answered.clone(), declined, down.clone(), withdrawn]);
        let seqs: Vec<u64> = got.iter().map(|(a, _)| *a).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4], "the open one is not a settle");
        assert_eq!(got[0].1.reply("s-b"), "`s-b` answered: 5433");
        assert_eq!(got[1].1.reply("s-b"), "`s-b` declined: not my area");
        assert!(got[2].1.reply("s-b").contains("shut down for good"));
        assert_eq!(got[3].1.reply("s-b"), "the ask was withdrawn by operator");
        // And the sentence a model reads names the row, the peer and the words.
        let line = settle_line("wire the port", &answered, &got[0].1);
        assert!(line.starts_with("[answer] on \"wire the port\":"), "{line}");
        assert!(line.contains("`s-b`"), "{line}");
        assert!(line.contains("which port?"), "{line}");
        assert!(line.contains("5433"), "{line}");
        assert!(settle_line("r", &down, &got[2].1).starts_with("[down]"));
        assert_eq!(settle_class(&got[2].1), "ask_lost");
        assert_eq!(settle_class(&got[0].1), "ask_settled");
    }

    /// An ask I made that no row carries any more is taken back: the target must not stay nagged by
    /// a question nobody waits on. A settled ask is not withdrawn again, and one a row still carries
    /// stays.
    #[test]
    fn an_ask_no_row_carries_is_taken_back() {
        let carried: BTreeSet<u64> = [7u64].into_iter().collect();
        let settled = ask(
            8,
            "s-a",
            "s-b",
            AskState::Answered {
                text: "x".into(),
                ms: 1,
            },
        );
        assert_eq!(withdrawals(&[open(7), open(9), settled], &carried), vec![9]);
        assert!(withdrawals(&[open(7)], &carried).is_empty());
        // A row detached with `detach` takes the wait off, so the ask goes back with it.
        assert_eq!(withdrawals(&[open(7)], &BTreeSet::new()), vec![7]);
    }

    /// The card is raised on `escalated` and withdrawn when it clears - and its words are the key,
    /// so the same state spells the sentence twice or the card would never be found to withdraw.
    #[test]
    fn the_operator_card_is_keyed_by_its_own_words_and_says_which_side_stopped() {
        let mut gone = open(7);
        let quiet = escalations(&[gone.clone()], &[], "s-arango");
        assert_eq!(quiet.len(), 1);
        assert!(!quiet[0].2, "nothing is owed while the target is up");

        gone.escalated = true;
        let loud = escalations(&[gone.clone()], &[], "s-arango");
        assert_eq!(
            loud,
            vec![(7, escalation_question(&gone, "s-arango"), true)]
        );
        let words = &loud[0].1;
        assert!(words.contains("`s-serenedb` stopped"), "{words}");
        assert!(words.contains("`s-arango` waits"), "{words}");
        assert!(words.contains("which port?"), "{words}");
        assert!(
            words == &escalation_question(&gone, "s-arango"),
            "the key is stable"
        );

        // The target's side: the asker is the one that stopped, and its owed row says so.
        let owed = ask(7, "s-arango", "s-serenedb", AskState::Open);
        let words = escalation_question(&owed, "s-serenedb");
        assert!(words.contains("s-arango"), "{words}");
        assert!(words.contains("not running"), "{words}");
        // A settled ask is never escalated: there is nothing left to restart for.
        let done = ask(
            9,
            "s-arango",
            "s-serenedb",
            AskState::Answered {
                text: "x".into(),
                ms: 1,
            },
        );
        assert!(escalations(&[done], &[], "s-arango").is_empty());
    }
}
