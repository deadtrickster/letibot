//! The cards that ask the person for something typed: a provider key, a terminal's
//! answer, a prompt, a secret (masked) - drawn by `rano::agent::asks` from the ask this head
//! holds.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::asks;

impl App {
    /// The key-ask card: what is asking, and where the key goes.
    /// The dots are drawn by the COMPOSER's box (`composer_rows`), not here: the one text
    /// surface this head has is the thing being typed into.
    pub(crate) fn key_ask_lines(&self, w: usize) -> Vec<String> {
        let Some(ask) = &self.key_ask else {
            return Vec::new();
        };
        let card = asks::KeyAsk {
            provider: ask.provider.clone(),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }

    /// The card that asks before a pane is ended - the daemon's question about killing a
    /// program, never the program's question about itself. It names the program and quotes
    /// nothing the program wrote, so it cannot be read as the prompt card.
    pub(crate) fn term_ask_lines(&self, ask: &TermAsk, w: usize) -> Vec<String> {
        let card = asks::TermAsk {
            line: ask.line.clone(),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }

    /// The prompt card: a command of the operator's own is waiting for a line.
    /// The headline names the command and not a program, because the daemon cannot know which
    /// process of a pipeline asked; and there is no countdown, because none was measured.
    pub(crate) fn prompt_lines(&self, ask: &PromptAsk, w: usize) -> Vec<String> {
        // A card raised WITHOUT a reading says so, and does not borrow the asking one:
        // claiming "your command is asking" would be the guess the whole design refuses,
        // since a process of the run may belong to another uid and be unreadable from here.
        if ask.reading == letibot_sessionlog::PromptReading::Unreadable {
            return row_strings(&unreadable_prompt_lines(ask, w), self.cfg.palette());
        }
        let card = asks::PromptAsk {
            command: ask.command.clone(),
            question: ask.question.clone(),
            job: ask.job.clone(),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }

    /// The password card: what is asking, for which command, and its keys. rano's own hint
    /// row names Esc as the refuser, so the key sentence is this head's: Esc puts the card
    /// away and sends nothing, and ctrl-c is the refusal.
    pub(crate) fn secret_lines(&self, ask: &SecretAsk, w: usize) -> Vec<String> {
        let card = asks::SecretAsk {
            prompt: ask.prompt.clone(),
            command: ask.command.clone(),
            remaining_ms: ask.deadline.saturating_sub(self.now_ms),
        };
        let mut lines = card.lines(w);
        for l in lines.iter_mut() {
            let plain = l.plain();
            if let Some(rest) = plain.trim_start().strip_prefix("enter sends it") {
                let said = format!("  enter sends it{rest}");
                *l = rano::agent::pane::faint(rano::agent::text::trim_to(&said, w));
            }
        }
        lines.push(rano::agent::pane::faint(rano::agent::text::trim_to(
            "  ctrl-c refuses · esc puts it away — alt-a brings it back",
            w,
        )));
        row_strings(&lines, self.cfg.palette())
    }
}

/// The prompt card's other half: a run of the operator's own that this daemon could not read,
/// drawn as rano lines so it crosses the same edge every other card does. Hand-built because
/// rano's `PromptAsk` has no field to say "not a reading", and it must never claim one.
fn unreadable_prompt_lines(ask: &PromptAsk, w: usize) -> Vec<rano::render::Line> {
    use rano::agent::text::{one, wrapped};
    use rano::style::Role;
    let mut out = wrapped(
        "? your command is running, and this daemon could not read it",
        w,
        Role::Pending,
    );
    for l in wrapped(
        &format!("  run: {}", ask.command.replace(['\n', '\r'], " ")),
        w,
        Role::Faint,
    ) {
        out.push(l);
    }
    // The honest half. One sentence for the fact that there is no reading, and one for
    // what the person can do about it - which is the same thing they would have done with a
    // card that had read the process.
    for l in wrapped(
        "  I cannot tell whether it is waiting for a line — part of it belongs to another user, \
         so I may not look at what it is doing. A line you type here goes into its input either \
         way.",
        w,
        Role::Faint,
    ) {
        out.push(l);
    }
    out.push(one(
        format!(
            "  enter sends it · esc puts the card away · `!send LINE` also works · {}",
            ask.job
        ),
        Role::Faint,
    ));
    out
}
