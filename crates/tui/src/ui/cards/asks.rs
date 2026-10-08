//! **The cards that ask the person for something typed**: a provider key, a terminal's
//! answer, a prompt, a secret (masked) — drawn by `rano::agent::asks` from the ask this head
//! holds.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::asks;

impl App {
    /// **The key-ask card**: what is asking, and where the key goes.
    ///
    /// The dots are not drawn here — the COMPOSER's box draws them (`composer_rows`), which is
    /// the same arrangement the sudo password card uses: the one text surface this head has is
    /// the thing being typed into, and the card above it is the question it is being typed for.
    pub(crate) fn key_ask_lines(&self, w: usize) -> Vec<String> {
        let Some(ask) = &self.key_ask else {
            return Vec::new();
        };
        let card = asks::KeyAsk {
            provider: ask.provider.clone(),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }

    /// **The card that asks before a pane is ended** — the daemon's own question about killing
    /// something, and never the program's question about itself. See [`TermAsk`] for the whole
    /// argument, and [`App::begin_close`] for when it is raised.
    ///
    /// # The words are the whole of the separation
    ///
    /// The prompt card and this one can never be on the screen together, and the keys cannot
    /// reach the wrong one (see [`TermAsk`]) — but a person has to know which one they are
    /// looking at *by reading it*, so the registers are as different as two cards can be:
    ///
    /// * **this one names the program** and says what ending it does, in the imperative;
    /// * **it quotes nothing the program wrote.** The prompt card's second line is the program's
    ///   own last line, which is exactly what makes it answerable; a confirmation that showed
    ///   that text would look like the program's question;
    /// * **it spells both keys and says which is the default**, because a destructive
    ///   confirmation that leaves the reader to guess is a trap.
    pub(crate) fn term_ask_lines(&self, ask: &TermAsk, w: usize) -> Vec<String> {
        let card = asks::TermAsk {
            line: ask.line.clone(),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }

    /// **The prompt card: a command of the operator's own is waiting for a line.**
    ///
    /// # What it shows, and what it deliberately does not
    ///
    /// The headline names the thing that is certain — **the command the operator typed** —
    /// because the daemon cannot know which process in a pipeline asked: `sudo apt install
    /// mc` is three programs and the question is the third one's, so a card that claimed a
    /// program name would be guessing at exactly the moment a person is deciding what to
    /// type. The program's own last line goes under it, faint and indented, exactly as
    /// sudo's prompt sits under the password card's headline: it is what the person is
    /// answering, quoted and never parsed.
    ///
    /// **No deadline, and that is not an omission.** The password card counts down because
    /// `sudo` gives up and the helper's request expires with it; a command blocked on its
    /// stdin is blocked until somebody answers it or its own timeout ends it, and this head
    /// was not told which. A countdown invented here would be a number nobody measured.
    ///
    /// **Not masked, and not a secret.** The text being typed is drawn by the composer
    /// exactly as the operator types it — see `composer_rows` — because what this card
    /// carries is a line for a program's **stdin** and it is drawn in the open. The password
    /// card's dots are the other channel's, and the two must not be one.
    ///
    /// **And it says the manual way in**, which is the honest half of the feature: the card
    /// is raised when the daemon can see the run blocked on the pipe it holds, and that
    /// reading has misses it names. `!send` needs no reading at all, and a person who would
    /// rather type there than here should be told so on the card rather than in a document.
    pub(crate) fn prompt_lines(&self, ask: &PromptAsk, w: usize) -> Vec<String> {
        // **A card raised WITHOUT a reading says so, and does not borrow the asking one.**
        //
        // The operator's correction is the whole of this branch: *"sudo can get input from
        // here so can others"*, measured 2026-10-09 on `! sudo apt install mc` — the password
        // taken, `apt` at `Continue? [Y/n]` as root, the daemon unable to read
        // `/proc/<pid>/fd/0` (`EACCES`), so no card, and the `y` they typed at the composer
        // became a PROMPT and reached the MODEL while their own command waited and died at
        // its deadline. A card that claimed *your command is asking* would be the guess the
        // whole design refuses — one process of the run belongs to another uid, and a long
        // quiet command that is asking nothing looks exactly the same from here. So the two
        // facts get two cards, and [`PromptReading`] is what tells them apart.
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

    /// The password card: what is asking, for which command, and the two keys.
    pub(crate) fn secret_lines(&self, ask: &SecretAsk, w: usize) -> Vec<String> {
        let card = asks::SecretAsk {
            prompt: ask.prompt.clone(),
            command: ask.command.clone(),
            remaining_ms: ask.deadline.saturating_sub(self.now_ms),
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }
}

/// **The prompt card's other half: a run of the operator's own that this daemon could not
/// read**, drawn as rano lines so it crosses the same edge every other card does
/// ([`row_strings`]).
///
/// # Why it is hand-built and not `rano::agent::asks::PromptAsk`
///
/// That card's whole text is a claim about a READING — its headline is *your command is
/// asking* — and rano has no field to say otherwise. The alternative was a rano release and a
/// second tag bump in one night, for one sentence; this is the sentence, in the same
/// registers (a `Role::Pending` headline, `Role::Faint` for the rest) and the same width
/// discipline, so the two cards are indistinguishable to a reader except in what they say —
/// which is the point. If rano grows the field, this moves there and the hand-built copy
/// goes.
///
/// # What it must say, and what it must not
///
/// * **It must not claim the program asked.** It cannot know: a process of the run belongs to
///   another uid (the `sudo` case), and a long quiet command that is asking nothing is
///   indistinguishable from here. So the headline states the two facts it DOES hold — the run
///   is going, and this daemon could not look at it.
/// * **It must say the line goes in either way**, because that is what makes it answerable:
///   the card is an offer of the same write `!send` performs, not a claim about what is on the
///   other end of it.
/// * **It names `!send` and the job**, exactly as the reading-backed card's footer does — the
///   verb that needs no signal is the floor under both.
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
    // **The honest half.** One sentence for the fact that there is no reading, and one for
    // what the person can do about it — which is the same thing they would have done with a
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
