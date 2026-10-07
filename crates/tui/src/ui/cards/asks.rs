//! **The cards that ask the person for something typed**: a provider key, a terminal's
//! answer, a prompt, a secret (masked).

use crate::app::*;
use crate::render::{sgr, trim_to, wrap};

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
        let mut out = vec![colour(
            &self.cfg,
            sgr::YELLOW,
            &trim_to(
                &format!("{} needs a key this box does not hold", ask.provider),
                w,
            ),
        )];
        for l in wrap(
            &format!(
                "paste the {} key below — shown as dots, sent once to the daemon, stored at \
                 mode 600; it never enters the conversation or the transcript. Enter stores it \
                 and takes the row; Esc cancels",
                ask.provider
            ),
            w,
        ) {
            out.push(dim(&self.cfg, &l));
        }
        out
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
        let mut out = Vec::new();
        // The headline is the question, and it carries the `?` every other card in this file
        // carries. Yellow for the reason the gate card is: it exists to interrupt.
        out.push(colour(
            &self.cfg,
            sgr::YELLOW,
            &trim_to(&format!("? end the pane — {} is running", ask.line), w),
        ));
        out.push(dim(
            &self.cfg,
            &trim_to("  ending it kills the program and everything it started", w),
        ));
        // **The keys, both of them, and which one is the default.** Not Enter: that key is the
        // prompt card's and the composer's, and a stray one must not be able to kill a program.
        out.push(dim(
            &self.cfg,
            &trim_to(
                "  y ends it  ·  any other key (esc) leaves it running — that is the default",
                w,
            ),
        ));
        out
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
        let mut out = Vec::new();
        // The headline is the fact, and it carries the marker the decision card carries —
        // this is a card and not three lines of prose above the composer. Yellow, for the
        // reason that card is: it exists to interrupt.
        out.push(colour(
            &self.cfg,
            sgr::YELLOW,
            &trim_to("? your command is asking", w),
        ));
        // The operator's own words, which is the one thing the daemon is certain of.
        for l in wrap(&format!("run: {}", ask.command), w.saturating_sub(2)) {
            out.push(dim(&self.cfg, &format!("  {l}")));
        }
        // **The program's own last line, quoted.** `None` is a real case — a `read` that
        // asks nothing, blocked before its first byte — and it says so rather than showing
        // an empty line as if that were the question.
        match ask.question.as_deref().map(str::trim) {
            Some(q) if !q.is_empty() => {
                for l in wrap(q, w.saturating_sub(4)) {
                    out.push(dim(&self.cfg, &format!("  │ {l}")));
                }
            }
            _ => out.push(dim(
                &self.cfg,
                &trim_to("  │ (it has not written anything yet)", w),
            )),
        }
        // The job handle, so the row can be found in `/jobs` and killed if it must be — and
        // the two keys, keys first, for the reason the password card puts them first.
        out.push(dim(
            &self.cfg,
            &trim_to(
                &format!(
                    "  enter sends it · esc puts the card away · `!send LINE` also works · {}",
                    ask.job
                ),
                w,
            ),
        ));
        out
    }

    /// The password card: what is asking, for which command, and the two keys.
    pub(crate) fn secret_lines(&self, ask: &SecretAsk, w: usize) -> Vec<String> {
        // **The same countdown the gate card draws** (§1.6), which is the other half of
        // this commit's finding: the instrument was here all along and nothing had
        // pointed it at the gate. One function, so the two cards cannot disagree about
        // how long there is — and `saturating_sub` is why an expired one reads `0s left`
        // rather than counting backwards, which is a number that means nothing.
        let left = letibot_ui::progress::countdown(ask.deadline.saturating_sub(self.now_ms));
        let mut out = Vec::new();
        // **The headline is the question, and it carries the `?` the decision card
        // carries** — this is a card, not three lines of prose above the composer, and
        // the marker is what says so at a glance. Yellow for the reason that card is:
        // it exists to interrupt.
        //
        // **And it says the thing ONCE.** The operator, having just answered one: *"the
        // ask is ugly as hell"*. The worst of it was the old first line, which read
        // `sudo wants a password — [sudo] password for dead:` — the same fact twice, in
        // the same weight, trailing off a colon.
        // **Two askers share this card**: sudo, through `letibot-askpass`, and the daemon
        // asking for a provider's API key (`Harness::obtain_key`), which sends no command.
        // The headline says which; the masking, the keys and the countdown are the same.
        let key = ask.command.is_empty();
        out.push(colour(
            &self.cfg,
            sgr::YELLOW,
            &trim_to(
                if key {
                    "? an API key is needed"
                } else {
                    "? sudo wants a password"
                },
                w,
            ),
        ));
        // **sudo's own words**, which name the account — faint and indented, because the
        // headline has already said what this is.
        for l in wrap(ask.prompt.trim(), w.saturating_sub(2)) {
            out.push(dim(&self.cfg, &format!("  {l}")));
        }
        // The command keeps its own rows — it is the one thing here worth the rows, and
        // the thing being authorised — and it is faint, because it is a fact about the
        // ask rather than the ask itself. `run:` names it, where the old `for:` named
        // nothing. A key ask has no command, so no row.
        if !key {
            for l in wrap(&format!("run: {}", ask.command), w.saturating_sub(2)) {
                out.push(dim(&self.cfg, &format!("  {l}")));
            }
        }
        // **Short enough to survive a narrow terminal whole, keys first.** The old
        // sentence ran past a hundred columns and `trim_to` cuts from the end — which is
        // where the countdown lives, so on a narrow screen the one field that is moving
        // was the first thing sacrificed. The keys come first for the same reason the
        // gate card's refusal names its remedy first: what is cut must not be the way
        // out. Forty-two columns, so the countdown survives even a 44-column frame —
        // and `to sudo` is not in it because the headline has already named sudo.
        out.push(dim(
            &self.cfg,
            &trim_to(&format!("  enter sends it · esc refuses · {left}"), w),
        ));
        out
    }
}
