//! **The harness's tests**, one file per theme, and the fixtures they share. Each theme file
//! reaches the fixtures, the harness and the imports below through `use super::*`.

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
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

mod card_fixtures;
mod children;
mod context;
mod delivery;
mod intent;
mod jobs;
mod misc;
mod operator_ask;
mod questions;
