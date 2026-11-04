//! The authorisation trail, a mirror of what was said rather than a walk over the transcript: every `User` row looks alike, so the speaker is recorded where known.
//! Also the two rows a question and its answer become, the same provenance question for the head's own door.

use super::*;

struct Said {
    speaker: Speaker,
    /// **At most [`UTTERANCE_CHARS`] characters** - see [`TrailMirror::say`].
    text: String,
    /// It was longer than that when it was said, and says so - the flag is what
    /// `Utterance::clipped` reports, and it is recorded here rather than recomputed
    /// because the text this struct holds is already the clipped one.
    clipped: bool,
    /// Which turn of this session it belongs to.
    turn: u32,
    /// When it was said. `None` for a row rebuilt from the store - see
    /// [`TrailMirror`].
    at: Option<Instant>,
}

/// The authorisation trail's source, and why it is a mirror rather than a walk over `session.items`:
/// every `User` row looks identical in the transcript, so calling them all the operator's would let
/// the harness authorise itself. The provenance is known only at the append, so it is recorded there.
#[derive(Default)]
pub struct TrailMirror {
    inner: Mutex<TrailInner>,
}

#[derive(Default)]
struct TrailInner {
    said: Vec<Said>,
    /// Every transcript item this session has, of any kind. The denominator.
    items: usize,
    /// The turn currently being served. 0 before the first one.
    turn: u32,
    /// The agent's most recent statement of what it is doing, for the guard to connect the operator's
    /// words to a call with. Never an authorisation and never citable. One line, replaced rather than
    /// accumulated, because the question is what it is doing NOW.
    claim: Option<String>,
}

impl TrailMirror {
    fn lock(&self) -> std::sync::MutexGuard<'_, TrailInner> {
        // Session state, not a safety boundary: recovering a poisoned guard is
        // right, because losing the trail would turn a panic somewhere else into a
        // gate that suddenly cannot see what authorised anything.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A turn is starting. `turns_ago` is measured from here.
    pub(crate) fn begin_turn(&self) {
        self.lock().turn += 1;
    }

    /// The denominator: every item this session appended, of any kind. Kept apart from
    /// [`TrailMirror::say`] because collapsing the two is the defect `TrailProvenance` exists to stop:
    /// `0 of 41 messages` is a measurement and `0` is not.
    pub(crate) fn note_items(&self, n: usize) {
        self.lock().items += n;
    }

    /// Record one thing that was said, with the provenance only the caller knows. Two caps, and both
    /// were already the only values anything read (`trail()` takes the last sixty and clips each to six
    /// hundred), so this is a memory bound and not a behaviour change.
    pub(crate) fn say(&self, speaker: Speaker, text: &str, at: Option<Instant>) {
        if text.trim().is_empty() {
            return;
        }
        let clipped = text.chars().count() > UTTERANCE_CHARS;
        let stored: String = if clipped {
            text.chars().take(UTTERANCE_CHARS).collect()
        } else {
            text.to_string()
        };
        let mut g = self.lock();
        let turn = g.turn;
        g.said.push(Said {
            speaker,
            text: stored,
            clipped,
            turn,
            at,
        });
        Self::keep_the_last_scan(&mut g.said);
    }

    /// Drop everything past the [`TRAIL_SCAN`] the walk can reach: one statement in one place, because
    /// `say` and `seed` must not disagree about how long the trail is.
    fn keep_the_last_scan(said: &mut Vec<Said>) {
        if said.len() > TRAIL_SCAN {
            let drop = said.len() - TRAIL_SCAN;
            said.drain(..drop);
        }
    }

    /// **What the trail holds**, in bytes of text and in utterances - the third term of a
    /// hub's [`Footprint`](letibot_sessionlog::hub::Footprint), read live because this is
    /// already shared behind its own lock.
    pub(crate) fn held(&self) -> (usize, usize) {
        let g = self.lock();
        (g.said.iter().map(|s| s.text.len()).sum(), g.said.len())
    }

    /// Seed from a transcript rebuilt out of the store. The speaker comes off the row (R42): calling
    /// every `User` row the operator's put the session's own settlements and notices into the
    /// authorisation trail as citable utterances. A row older than the field reads as `Operator`.
    pub(crate) fn seed(&self, items: &[TranscriptItem]) {
        let mut g = self.lock();
        g.items = items.len();
        g.said.clear();
        for (i, item) in items.iter().enumerate() {
            if let TranscriptItem::User { parts, speaker } = item {
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    continue;
                }
                // The same two caps a live utterance gets - see `say`. A resumed session's
                // rows are the same text by the same rule, so a seeded trail and a live one
                // are one bound.
                let clipped = text.chars().count() > UTTERANCE_CHARS;
                let text: String = if clipped {
                    text.chars().take(UTTERANCE_CHARS).collect()
                } else {
                    text
                };
                g.said.push(Said {
                    // The two `Speaker` enums meet here and nowhere else, matched inline rather than
                    // behind a `From`, so the one place they touch is the one place a reader looks for
                    // the mapping.
                    speaker: match speaker {
                        letibot_transcript::Speaker::Operator => Speaker::Operator,
                        letibot_transcript::Speaker::Agent => Speaker::Agent,
                    },
                    text,
                    clipped,
                    // Item index as a stand-in turn: the ordering is right and the
                    // distances are monotone, which is what `turns_ago` is read for.
                    turn: i as u32,
                    at: None,
                });
                Self::keep_the_last_scan(&mut g.said);
            }
        }
        g.turn = items.len() as u32;
    }

    /// What the agent last said it was doing, bounded the way an utterance is.
    pub(crate) fn claim(&self) -> Option<String> {
        self.lock().claim.clone()
    }

    /// Record it. Called with the assistant's own prose for a turn - the sentence
    /// before the tool calls, which is where a model says what it is about to do.
    pub(crate) fn claims(&self, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        let mut g = self.lock();
        g.claim = Some(t.chars().take(UTTERANCE_CHARS).collect());
    }

    /// **The trail, walked backwards.** What
    /// [`AdjudicatedGate::with_trail_source`] installs.
    pub fn trail(&self) -> AuthorisationTrail {
        let g = self.lock();
        let now = Instant::now();
        let scanned = g.said.len().min(TRAIL_SCAN);
        let mut utterances: Vec<Utterance> = Vec::new();
        for said in g.said.iter().rev().take(TRAIL_SCAN) {
            if utterances.len() >= TRAIL_UTTERANCES {
                break;
            }
            // The text is stored already clipped, so `clipped` comes off the record and the
            // comparison is kept as the belt to that pair of braces: a row this struct did not
            // write (there is none today) would still read correctly.
            let clipped = said.clipped || said.text.chars().count() > UTTERANCE_CHARS;
            utterances.push(Utterance {
                speaker: said.speaker,
                text: if clipped {
                    said.text.chars().take(UTTERANCE_CHARS).collect()
                } else {
                    said.text.clone()
                },
                clipped,
                turns_ago: g.turn.saturating_sub(said.turn),
                seconds_ago: said.at.map(|t| now.duration_since(t).as_secs()),
            });
        }
        // The denominator is what was LOOKED AT, not what came back: deriving it from the result
        // collapses "I looked at 41 messages and 0 were the operator's" into "I looked at 0".
        AuthorisationTrail::from_messages(utterances, scanned.max(g.items.min(TRAIL_SCAN)))
    }
}

// ---------------------------------------------------------------------------
// A question the model asked, and the answer a person gave it
// ---------------------------------------------------------------------------

/// The tool whose call is an exchange with a person rather than a piece of work. Spelled here and
/// not imported, because at this layer the name means *the one call whose result carries a person's
/// own utterance*, a fact about the transcript and not about the schema.
pub(crate) const ASK_USER_QUESTION: &str = "ask_user_question";

/// The person's own words, exactly: what their row carries, or `None` when there is no honest way to
/// state them. The parts are read off the ANSWER and nothing of ours is prepended, because a citable
/// utterance with a harness sentence glued to it is a citable utterance of the wrong thing.
pub(crate) fn operator_answer_text(asked: &crate::answers::Asked) -> Option<String> {
    let a = &asked.answer;
    if a.abstain {
        return Some("abstain".to_string());
    }
    let mut said: Vec<String> = Vec::new();
    if let Some(i) = a.option {
        said.push(asked.question.options.get(i)?.clone());
    }
    if let Some(n) = a.note.as_deref().filter(|n| !n.trim().is_empty()) {
        said.push(n.trim().to_string());
    }
    if let Some(f) = a.free.as_deref().filter(|f| !f.trim().is_empty()) {
        said.push(f.trim().to_string());
    }
    if said.is_empty() {
        return None;
    }
    Some(said.join("\n"))
}

/// The question, as the SESSION's own row and never the operator's: the trail would otherwise quote
/// it back as authorisation for the answer to it. The options are a list, one per line, so an option
/// holding ` | ` is not read as two.
pub(crate) fn question_row_text(asked: &crate::answers::Asked) -> String {
    let q = &asked.question;
    let mut said = format!("asked: {}", q.text.trim());
    if !q.options.is_empty() {
        said.push_str("\noptions offered:");
        for o in &q.options {
            said.push_str("\n- ");
            said.push_str(&o.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    said
}

/// The two rows the person's half of a question makes, as a free function so the shape is testable
/// without a daemon, a model or a socket. The order is the question and then the answer; where they
/// land beside the call's own result is `Harness::settle_asked`'s business.
pub(crate) fn asked_rows(asked: &crate::answers::Asked, said: &str) -> [TranscriptItem; 2] {
    [
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: question_row_text(asked),
            }],
        },
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: said.to_string(),
            }],
        },
    ]
}
