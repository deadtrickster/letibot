//! Quick asks: `alt-k` in the pane. rano owns the prefix, the presets and the target rule - the
//! selection, else the paragraph or line under the cursor - so the head's half is what an ask
//! becomes: a prompt now, the asked range tinted while its answer is on the way, and what the
//! operator decided about a returned hunk on the prompt after.

use letibot_sessionlog::protocol::{
    AnswerMode, ContextAsk, ContextRef, EditorContext, MarkOutcome,
};
use rano::ask::{Answer, AskEvent};
use rano::marks::{Mark, Outcome};
use rano::send::SendEvent;

use super::*;

impl App {
    /// One ask out of the pane: the preset's words and the place, as a prompt at once - no composer
    /// round trip, and the keyboard stays in the editor. The free-text key has no words of its own,
    /// so it takes the `alt-s` road instead: a chip, and the question is the operator's to write.
    pub(crate) fn ask_in(&mut self, e: &AskEvent) {
        if e.prompt.trim().is_empty() {
            self.context_in(&e.send);
            return;
        }
        if self.detached() {
            self.refused_while_detached();
            return;
        }
        let r = self.context_ref(&e.send);
        let place = crate::app::context::place_name(&r, &|p| self.workspace_relative(p));
        self.tint_asked(&r, &e.send);
        let context = EditorContext {
            origin: "rano".into(),
            refs: vec![r],
            open: self.open_buffers(),
            ask: Some(ContextAsk {
                preset: e.preset.clone(),
                answer: answer_mode(e.answer),
            }),
            outcomes: std::mem::take(&mut self.outcomes),
        };
        self.scroll = 0;
        // The words are held on the screen until the transcript owns them, exactly as a typed
        // prompt is: an ask leaves no composer text behind, so without this it would be invisible.
        if self.turn_busy()
            && let Some(last) = self.pending_prompts.last_mut()
        {
            last.push('\n');
            last.push_str(&e.prompt);
        } else {
            self.pending_prompts.push(e.prompt.clone());
        }
        self.queued.push(Action::Prompt(Prompt::with_context(
            e.prompt.clone(),
            Vec::new(),
            Some(context),
        )));
        self.say(&format!("asked {} about {place}", e.preset));
    }

    /// The asked range, tinted in the pane until the turn ends: a rano mark is the only way a host
    /// colours lines, and a source of its own keeps the tint from replacing - or being replaced by -
    /// a mark the conversation set on the same file.
    fn tint_asked(&mut self, r: &ContextRef, e: &SendEvent) {
        // An unnamed buffer has no file to mark, and a bare place names no range.
        let Some(range) = r.range.filter(|_| !r.file.is_empty()) else {
            return;
        };
        let first = range.start.line as usize;
        let last = crate::app::context::last_line(&range);
        // The selection's own lines are the anchor when they are the rows the range names; a target
        // that ends mid-line makes a shorter anchor, and a wrong one reads as stale at once.
        let anchor: Vec<String> = e
            .selection
            .as_ref()
            .map(|s| s.text.split('\n').map(str::to_string).collect())
            .filter(|a: &Vec<String>| a.len() == last.saturating_sub(first) + 1)
            .unwrap_or_default();
        let source = format!("ask:{}:{first}-{last}", r.file);
        let mark = Mark::cite(&format!("tint:{first}-{last}"), first, last, anchor);
        if !self.ask_tints.contains(&source) {
            self.ask_tints.push(source.clone());
        }
        self.give_marks(std::path::Path::new(&r.file), &source, vec![mark]);
    }

    /// The turn an ask was waiting on is over: the tint goes, and with it nothing else - the reply
    /// and the operator's decisions about it arrive or wait on their own.
    pub(crate) fn asks_answered(&mut self) {
        for source in std::mem::take(&mut self.ask_tints) {
            self.file_marks.drop_source(&source);
            if let Some(p) = self.edit_pane.as_mut() {
                p.ed.clear_marks(Some(&source));
                p.dirty = true;
            }
        }
    }

    /// Every ask's tint, dropped without waiting for a turn: the marks of a conversation this head
    /// is no longer reading.
    pub(crate) fn forget_asks(&mut self) {
        self.ask_tints.clear();
    }

    /// A hunk the operator decided, recorded for the next prompt and then forgotten: the daemon
    /// renders the pair as one sentence, so the label is the place a person reads.
    pub(crate) fn record_decided(&mut self, decided: &[(String, Outcome)]) {
        for (id, outcome) in decided {
            let Some((path, mark)) = self.file_marks.find(id) else {
                continue;
            };
            let name = self.workspace_relative(&path);
            self.outcomes.push(MarkOutcome {
                label: format!("{name}:{}", span(&mark)),
                outcome: decided_word(*outcome).to_string(),
            });
        }
    }
}

/// rano's answer mode as the wire's: three words for the same three shapes of reply.
fn answer_mode(a: Answer) -> AnswerMode {
    match a {
        Answer::Chat => AnswerMode::Chat,
        Answer::Notes => AnswerMode::Notes,
        Answer::Suggest => AnswerMode::Suggest,
    }
}

/// What a decided mark is called back to the model as. Named for what it is: rano's `Outcome` is
/// about a hunk, and `crate::ui`'s `outcome_word` is about a tool's result.
fn decided_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Accepted => "accepted",
        Outcome::Rejected => "rejected",
        Outcome::Reverted => "reverted",
    }
}

/// The lines a mark covers, as `12` or `11-14`: one place, spelled as the citation block spells it.
fn span(m: &Mark) -> String {
    match m.end_line > m.line {
        true => format!("{}-{}", m.line, m.end_line),
        false => m.line.to_string(),
    }
}
