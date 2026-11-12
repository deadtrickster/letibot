//! The one mapping from a meaning to the colour a state is drawn in.
//!
//! The words are the pane wire's (`letibot_plugin::pane::Role`, re-exported as
//! `letibot_sessionlog::pane::Role`) and the looks are rano's; this table is the only place the
//! two meet. There were three such tables - the sessions pane's, a plugin pane's and the chat's -
//! and a `working` that was green in one and yellow in another is what that cost.
//!
//! The five severities mean one thing each, and a caller that wants a colour for "is happening"
//! asks for [`Meaning::Pending`] whatever pane it is drawing:
//!
//! ```text
//! Info       something happened and nothing is asked of the reader
//! Pending    work in progress: running, retrying, not finished
//! Warning    something went wrong and recovered, or is worth knowing before it does
//! Attention  something that needs a person, and nothing else
//! Failure    it did not work
//! ```

use letibot_sessionlog::pane::Role as Meaning;
use rano::style::Role;

/// The role a meaning is drawn in. `None` is a row that said nothing, which takes the row's own
/// role, and then the palette's plain.
pub(crate) fn role(m: Option<Meaning>) -> Role {
    match m {
        Some(Meaning::Info) => Role::Info,
        // A muted meaning and a faint one are one here: both are structure the reader skips.
        Some(Meaning::Muted) | Some(Meaning::Faint) => Role::Faint,
        // `working` and `pending` are one arm for the same reason: a pane that says either is
        // saying "this is happening", and that is one colour in every pane.
        Some(Meaning::Working) | Some(Meaning::Pending) => Role::Pending,
        Some(Meaning::Warning) => Role::Warning,
        Some(Meaning::Attention) => Role::Attention,
        Some(Meaning::Success) => Role::Success,
        Some(Meaning::Failure) => Role::Failure,
        Some(Meaning::Strong) => Role::Strong,
        Some(Meaning::Hue(n)) => crate::ui::chat::colour::paint(n),
        Some(Meaning::Plain) | None => Role::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five a severity may be, and the one table that says which. Anything else - a syntax hue,
    /// a plain run - is not a severity and cannot be asked for by one.
    const FIVE: [Role; 5] = [
        Role::Info,
        Role::Pending,
        Role::Warning,
        Role::Attention,
        Role::Failure,
    ];

    /// Every meaning the wire can carry has a role, and the five severities are five different
    /// looks: a severity that shared a colour with its neighbour is the defect this table exists
    /// to end.
    #[test]
    fn every_meaning_has_a_role_and_the_five_are_five() {
        let all = [
            Meaning::Muted,
            Meaning::Working,
            Meaning::Info,
            Meaning::Warning,
            Meaning::Attention,
            Meaning::Success,
            Meaning::Failure,
            Meaning::Plain,
            Meaning::Strong,
            Meaning::Faint,
            Meaning::Pending,
            Meaning::Hue(0),
            Meaning::Hue(1),
        ];
        assert_eq!(role(None), Role::Plain, "a row that said nothing is plain");
        for m in all {
            if !matches!(m, Meaning::Plain) {
                assert_ne!(role(Some(m)), Role::Plain, "{m:?} draws as prose");
            }
        }
        let five = [
            role(Some(Meaning::Info)),
            role(Some(Meaning::Pending)),
            role(Some(Meaning::Warning)),
            role(Some(Meaning::Attention)),
            role(Some(Meaning::Failure)),
        ];
        for (i, a) in five.iter().enumerate() {
            for b in &five[i + 1..] {
                assert_ne!(a, b, "two severities share a role");
            }
        }
        // The one word every pane used to spell differently.
        assert_eq!(role(Some(Meaning::Working)), Role::Pending);
        assert_eq!(role(Some(Meaning::Pending)), Role::Pending);
    }

    /// Every warning code in the log's table draws as one of the five, and so does every reader's
    /// card; an unclassified code and an unknown shape are both `Info`. The role is read through
    /// the head's own path, so a reader added without a severity fails here.
    #[test]
    fn every_code_and_every_reader_is_one_of_the_five() {
        use crate::ui::panes::notes::{class_mark, note_card};
        use crate::ui::transcript::notice_card::session_notice;
        // The codes: `note_card` draws `Note::Warned`'s card in the class's role.
        let mut codes = 0;
        for (code, _class, _action) in letibot_sessionlog::warning::TABLE {
            let note = crate::app::Note::Warned(letibot_sessionlog::view::Warned {
                code: (*code).to_string(),
                detail: "a sentence no card reads".into(),
                ts: 0,
            });
            let role = note_card(&note).expect("a warning is a card").0.role;
            assert!(FIVE.contains(&role), "{code} draws {role:?}");
            let (_, lead) = class_mark(code);
            assert_eq!(
                lead, role,
                "{code}: the card and the fallback line disagree"
            );
            codes += 1;
        }
        assert!(codes > 50, "{codes} codes");
        // An unclassified code is info, through both halves.
        assert_eq!(
            letibot_sessionlog::warning::class("a_code_from_next_week").role(),
            Meaning::Info
        );
        let (mark, role) = class_mark("a_code_from_next_week");
        assert_eq!((mark, role), ("·", Role::Info));
        // The readers: every shape the tree has a fixture for.
        let mut read = 0;
        for (text, _line) in letibot_plugin::card::readers::FIXTURES {
            let card = session_notice(text);
            assert!(FIVE.contains(&card.role), "{text:?} draws {:?}", card.role);
            read += 1;
        }
        assert!(read >= 8, "{read} fixtures");
        // A shape no reader knows is info, and says so as the generic card the host falls back to.
        assert_eq!(
            session_notice("A sentence no reader knows; it goes on.").role,
            Role::Info
        );
    }

    /// "Running" is one role in every pane: the sessions layer's word for a generating turn, the
    /// board's own `[~]`, and the queue's mark for a gate in flight. Three panes, one colour.
    #[test]
    fn running_is_the_same_role_in_sessions_todos_and_queue() {
        // Sessions: the word its layer sends for a turn that is generating.
        assert_eq!(role(Some(Meaning::Pending)), Role::Pending);
        // Todos: the board's mark, drawn by rano - compared as bytes, since its role is not ours.
        let p = rano::style::Palette::Colour;
        let doing =
            rano::render::Line::new(vec![rano::agent::todos::TodoMark::Doing.span()]).to_ansi(p);
        assert_eq!(doing, p.paint(Role::Pending, "[~]"));
        // Queue: the head's mark for a gate or a review in flight.
        assert_eq!(crate::ui::panes::queue::mark_of("gating").1, Role::Pending);
        assert_eq!(
            crate::ui::panes::queue::mark_of("reviewing").1,
            Role::Pending
        );
    }
}
