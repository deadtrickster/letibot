//! The harness's tests, one file per theme, and the fixtures they share. Each theme file reaches the
//! fixtures, the harness and the imports below through `use super::*`.

use letibot_tools::builtins::intent::{Question, QuestionAnswer};

fn a_question() -> Question {
    Question {
        text: "which database should the migration target?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
        because: "the two need different migration files".into(),
        ..Default::default()
    }
}

fn asked(answer: QuestionAnswer) -> crate::answers::Asked {
    crate::answers::Asked {
        question: a_question(),
        answer,
        by: "dead".into(),
    }
}

use super::*;

use letibot_tools::authorise::TrailProvenance;

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        // Named, not defaulted: this fixture is the person typing, and an unlabelled row reads as
        // the session's own.
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// The point a fixture opens at when the test is not about the point. A project nobody configured
/// is `automode` since 2026-11-18, and opening a session there needs an authorisation oracle:
/// each test below is about something else, so it stands where it always did.
fn at_always_ask(cfg: &mut Config) {
    cfg.mode = letibot_tools::mode::Mode::ALWAYS_ASK;
}

mod auto_reseat;
mod card_fixtures;
mod cheap_wake;
mod child_mode;
mod childcap;
mod children;
mod citations;
mod clear;
mod cloud_sampling;
mod context;
mod delivery;
mod fallback;
mod firings;
mod fork_carry;
mod import_rows;
mod intent;
mod jobs;
mod kept_jobs;
mod late_notice;
mod limit_child;
mod marks;
mod misc;
mod model_switch_cancel;
mod note_accept;
mod notice_door;
mod offer_row;
mod operator_ask;
mod package_shadow;
mod parallel_reads;
mod parentage;
mod prefix_on_open;
mod prompt_lanes;
mod prune;
mod questions;
mod recall;
mod restart_notice;
mod reviewer_mode;
mod roles;
mod seam_store;
mod seat_waits;
mod shadow_during;
mod shadow_turn_end;
mod steer_door;
mod store_writes;
mod unbounded;
mod usage_notice;
mod workspace;
