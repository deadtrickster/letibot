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
        let lines = match &ask.kind {
            KeyKind::Provider => asks::KeyAsk {
                provider: ask.provider.clone(),
            }
            .lines(w),
            // rano's `KeyAsk` card is a provider key's - "needs a key this box does not hold" - and
            // a fabric token is not one. The same card, the same masked field, this head's words.
            KeyKind::Flowy { rest } => flowy_key_lines(rest, w),
        };
        row_strings(&lines, self.cfg.palette())
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

    /// The secret card: what will use the value, what it came from, where it is going, and its
    /// keys. rano's own `SecretAsk` is the masked field and the countdown; the facts above it and
    /// the sentence about the two answers are this head's, because rano's card has no field for
    /// them.
    ///
    /// Esc puts the card away and sends nothing; ctrl-c is the refusal. `alt-k` is the second
    /// answer - keep the value - and appears only when the asker allowed a window.
    pub(crate) fn secret_lines(&self, ask: &SecretAsk, w: usize) -> Vec<String> {
        use rano::agent::text::{one, trim_to, wrapped};
        use rano::style::Role;
        let card = asks::SecretAsk {
            prompt: ask.prompt.clone(),
            command: ask.command.clone(),
            remaining_ms: ask.deadline.saturating_sub(self.now_ms),
        };
        let mut lines = Vec::new();
        // A key ask (a provider key on its way to a file) has no command and no `--for`: its
        // prompt is the whole sentence, and rano's card already draws it.
        if !ask.for_what.is_empty() {
            lines.extend(wrapped(
                &format!("  for: {}", ask.for_what.replace(['\n', '\r'], " ")),
                w,
                Role::Faint,
            ));
        }
        if !ask.host.is_empty() {
            lines.extend(wrapped(
                &format!("  on: {}", ask.host.replace(['\n', '\r'], " ")),
                w,
                Role::Faint,
            ));
        }
        if ask.waiting > 1 {
            lines.push(one(
                trim_to(
                    &format!("  {} commands are waiting on this one answer", ask.waiting),
                    w,
                ),
                Role::Pending,
            ));
        }
        lines.extend(card.lines(w));
        for l in lines.iter_mut() {
            let plain = l.plain();
            if let Some(rest) = plain.trim_start().strip_prefix("enter sends it") {
                let said = format!("  enter sends it{rest}");
                *l = rano::agent::pane::faint(rano::agent::text::trim_to(&said, w));
            }
        }
        let keep = match ask.keep_ms {
            Some(ms) => match &ask.key {
                Some(key) => format!(" · alt-k keeps it {} min as `{key}`", (ms / 60_000).max(1)),
                None => format!(" · alt-k keeps it {} min", (ms / 60_000).max(1)),
            },
            None => String::new(),
        };
        lines.push(rano::agent::pane::faint(rano::agent::text::trim_to(
            &format!("  ctrl-c refuses · esc puts it away — alt-a brings it back{keep}"),
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

/// The masked card's words for the fabric's token. The same card a provider's key is typed into -
/// the composer draws the dots and one line of this sits above it - because a token is exactly the
/// kind of thing the masked field exists for, and rano's `KeyAsk` card is a provider's: it says
/// "needs a key this box does not hold", which is not what a seat has not got.
///
/// `rest` is what the operator typed between `login` and the flag, echoed so they can see the seat
/// and address the token will be sent with.
fn flowy_key_lines(rest: &str, w: usize) -> Vec<rano::render::Line> {
    use rano::agent::text::{one, wrapped};
    use rano::style::Role;
    let mut out = wrapped(
        "? the flowy seat needs a token this box does not have",
        w,
        Role::Pending,
    );
    for l in wrapped(
        "  paste the token below — shown as dots, sent once to the daemon, and it never enters the \
         conversation or the transcript. enter attaches the seat; esc cancels",
        w,
        Role::Faint,
    ) {
        out.push(l);
    }
    out.push(one(format!("  /flowy login {rest}--token …"), Role::Faint));
    out
}
