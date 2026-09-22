//! Accumulating generated **token ids** out of a `/completion` stream.
//!
//! # The trap this module exists for
//!
//! In stream mode the final frame carries an **empty** `tokens` array
//! (`server-task.cpp:1064` populates it from the partial's own `tokens`, and the
//! final result's is never filled). Measured on this box, `qwen-3.8-flash-next`:
//!
//! ```text
//!  17 stop=0 tokens=[248046] predicted=17 content=''
//!  18 stop=1 tokens=[]       predicted=17 content=''
//! ```
//!
//! An implementation that reads only the final frame finds nothing there, decides
//! it must reconstruct the generation from `content`, and re-tokenizes text. That
//! reintroduces exactly the detokenize/retokenize round trip the token ledger
//! exists to detect: a whitespace or special-token difference then rides into the
//! next prompt and the cache diverges silently. So: **ids come from the partial
//! frames.**
//!
//! # The second trap, which the first one hides
//!
//! §5.6 requires `return_progress: true`. llama.cpp builds a progress frame with
//! `send_partial_response(slot, {}, true)` (`server-context.cpp:5955`) — a
//! **default-constructed** `completion_token_output` — and then unconditionally
//! writes `res->tokens = { tkn.tok }`. The frame therefore carries a fabricated
//! **token id 0**:
//!
//! ```text
//!   1 stop=0 prog=True tokens=[0] predicted=0 content=''
//!   2 stop=0 prog=True tokens=[0] predicted=0 content=''
//!   3 stop=0 prog=False tokens=[3833] predicted=1 content='One'
//! ```
//!
//! So the naive fix for the first trap — "accumulate `tokens` from every non-final
//! frame" — prepends three zeros to a 17-token generation and corrupts the ledger
//! on the very first turn. The two fields §5.6 makes mandatory interact.
//!
//! # The rule
//!
//! Accept a frame's ids only when the server's own `tokens_predicted` counter has
//! **advanced**, and take exactly as many ids as it advanced by. `tokens_predicted`
//! is the fact ("how many tokens has this turn generated"); the presence of a
//! `prompt_progress` object is a proxy for it. At the end, the accumulated count
//! must equal the final frame's `tokens_predicted`, or the turn is refused.
//!
//! # The third trap: the server sometimes emits no frame at all for a token
//!
//! T23, and it is a defect in **upstream llama.cpp** — fixed locally in the operator's
//! fork, commit **`d10f94713`** (*"server : emit the frame for a token that ends in a
//! partial UTF-8 character"*, author date 2026-09-10, committed to branch **`glm-all`**
//! in `~/Projects/llama.cpp` on 2026-09-19), which is the tree the `llama-server` on
//! 127.0.0.1:8080 is built from. **Name both dates, because they are different facts and
//! a reader checking with `git log` will see either one**: the 10th is the day it was
//! written and the day this box measured it; the 19th is when a rebase landed it on the
//! branch. `git merge-base --is-ancestor d10f94713 HEAD` on that branch answers *carried?*
//! in one command, which is the check this note is for.
//!
//! Unfixed `process_token` (`server-context.cpp:4067`) computes
//!
//! ```text
//! bool incomplete = validate_utf8(slot.generated_text) < slot.generated_text.size();
//! ```
//!
//! and puts `send_partial_response` inside `if (!incomplete)`. `slot.stats.n_gen`
//! was already incremented, in both the plain (`:6450`) and the speculative
//! (`:6615`) decode paths. So a token whose bytes leave the generated text ending
//! mid-character produces **no frame**, while the counter it advanced is reported
//! by the *next* frame — which carries only its own id:
//!
//! ```text
//!   {"content":"😀",  "tokens":[141334],"tokens_predicted":1}
//!   {"content":" 😂", "tokens":[224],   "tokens_predicted":3}   ← advance 2, ids 1
//! ```
//!
//! `" 😂"` is two tokens, `[26525, 224]`: `26525` spells a space and the **first
//! three bytes** of a four-byte emoji, so it is suppressed, and `224` (the fourth
//! byte) completes the character and carries the accumulated text. **Id `26525` is
//! never transmitted** — in stream mode the terminal frame's `tokens` array is
//! empty (`server-context.cpp:4326`), so there is nowhere else for it to appear.
//!
//! The signature is exactly `advance 2, ids 1`, never `2, 0` — a suppressed token
//! emits nothing, so there is no zero-id frame — and `advance 3, ids 1` when a
//! character is spread over three tokens. It is content-dependent, not periodic:
//! ASCII and common typographic punctuation are single vocabulary entries and never
//! trigger it, which is why 400 tokens of prose reproduce nothing and a line of
//! emoji reproduce it forty times.
//!
//! **This is not speculative decoding**, which was the standing hypothesis. Measured
//! both ways on 2026-09-10: with MTP active (215 drafted, 150 accepted) an ASCII
//! generation gave `{advance 1, ids 1}` for all 190 tokens, and on a server with no
//! draft model at all the emoji case reproduced 67 times.
//!
//! # Why the guard STAYS, and this is the part that must not be skimmed
//!
//! **The fix lives on a patch branch, not in upstream.** `glm-all` is rebased onto
//! upstream master — its own HEAD is *"rebase onto upstream master e613ef2c8"* — so the
//! patch survives exactly as long as somebody keeps rebasing it. Every server but this
//! one is unpatched, including this one the moment it is rebuilt from upstream `master`
//! or from a release. So this accumulator is **not dead code and not a historical
//! curiosity: it is the detector for a server without `d10f94713`, and that is the
//! default state of the world.** Deleting it would turn a detectable server defect into
//! silent ledger corruption on the first non-ASCII generation, which is the failure mode
//! this module exists to prevent.
//!
//! # What it does against an unpatched server, which is what this promises
//!
//! Not a history lesson — the behaviour on a box that never took the patch:
//!
//! 1. **The frame is refused**, and so is every frame after it, because
//!    [`IdAccumulator::push`] stops at the first frame that does not account for its own
//!    advance. `26525` never reaches the ledger: its id does not exist anywhere on the
//!    wire, so a ledger containing it would be a ledger of ids the model did not
//!    produce.
//! 2. **Every id before that frame is kept** — and *how many that is depends on where the
//!    first suppressed token landed*, which is worth stating rather than promising a
//!    partial. The refusal is carried as [`AbortCause::FrameMismatch`], so the turn is
//!    marked interrupted holding what was accounted for instead of failing with `nothing
//!    was recorded`. **A generation that STARTS with a split character keeps nothing**: the
//!    first frame already disagrees, `ids` is empty, and the abort carries an empty head
//!    with `partial_kept: false` — which is the same thing an unpatched server does to the
//!    OAI and Anthropic serialisers, whose opening chunk is gated on `n_decoded == 1`.
//!    Measured on this box, prompt *"repeat exactly"* over twenty emoji, 60 tokens: **31 of
//!    the 60 counter-frames carry an id with empty `content`** — that empty-content-with-an-id
//!    frame IS the patch, since pre-`d10f94713` it was not sent at all — so an unpatched
//!    server would have withheld 31 ids here and the guard would have fired at the first of
//!    them. The patched server, measured the same way: 60 tokens, 60 ids, **0 bad frames**.
//!    That asymmetry is the whole reason the guard is worth its keep, and it is a
//!    measurement rather than a recollection — see the test that pins it.
//! 3. **The frames are written out verbatim**, neighbours included, and the path is
//!    announced as `frame_capture_written` on the session log — a warning in the routine
//!    register (see `letibot_sessionlog::warning`), because the evidence being kept is
//!    the mechanism *working*. Capture switched off is `frame_capture_disabled` and a
//!    capture that could not be written is `frame_capture_failed`, which **is** in the
//!    failure register, because then there is no evidence at all.
//! 4. **The operator reads which of two faults it was** in the interrupt sentence —
//!    [`Mismatch`]. The direction of the mismatch is the evidence, and it is free:
//!    `ids < advance` means the server **withheld** what it counted, which is this gate
//!    and nothing else (nothing withholds forward); `ids > advance` means it sent
//!    **more** than it counted, which no withholding can explain. Same refusal, same
//!    kept answer, different first thing to check — the server's build in the first case,
//!    the captured frames in the second. Before this they were one sentence, which is
//!    R12's defect in a second family: *two facts, one code.*
//!
//! Checking the server, when the sentence says **withheld**:
//!
//! ```text
//! cd ~/Projects/llama.cpp && git log -1 --format='%h %s' d10f94713
//! git merge-base --is-ancestor d10f94713 HEAD && echo patched || echo UNPATCHED
//! ```
//!
//! **Nothing on this side changes with the patch**: the accumulator accepts or refuses
//! the same frames either way, and a patched server simply never produces a refused one.
//! That is the property to keep — the guard is a function of what arrives, not of which
//! server the operator happens to be running.

use letibot_tokencore::TokenId;

use crate::completion::{Chunk, FinalChunk, PromptProgress, Timings};

#[derive(Debug)]
pub enum StreamError {
    /// The ids we accumulated do not match the server's own count. Never repaired
    /// by re-tokenizing text: a turn whose token identity is unknown must not
    /// reach the ledger.
    CountMismatch {
        accumulated: usize,
        reported: u64,
    },
    /// A frame's ids did not match the amount its counter advanced by.
    FrameMismatch {
        n_decoded: u64,
        previous: u64,
        ids: usize,
        /// **Which fault it was** — see [`Mismatch`]. R12's shape in a second family:
        /// two different facts, one refusal, and the difference is free.
        reading: Mismatch,
    },
    /// The stream ended without a terminal frame.
    NoFinalChunk,
    Protocol(String),
}

/// **Which way a frame failed to account for itself** — and the direction is the evidence.
///
/// The refusal is the same either way (the frame is not a record of what the model produced,
/// so it does not reach the ledger). What differs is **the first thing to check**, and before
/// this they were one sentence naming three numbers and no remedy — R12's defect, where a
/// reply that ran out of room was reported as an answer nobody could read.
///
/// The line between them is arithmetic and it costs nothing to compute: a server that
/// **withholds** a token's frame can only ever send FEWER ids than it counted, because
/// withholding removes ids and nothing adds them. So `ids > advance` is *provably not* the
/// UTF-8 gate — it is a different fault, whatever it is — and `ids < advance` is the shape the
/// gate produces and the only shape it produces.
///
/// **`Withheld` is a reading and not a diagnosis.** It says what the frames show, which is
/// that the server counted tokens it did not send; it does not know which server is on the
/// other end. The remedy named in the sentence is therefore *check the build*, and the commit
/// it names is what to check against — see the module header, and note that a server rebuilt
/// from upstream `master` is unpatched again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// The server counted more tokens than it sent. This is what a token whose bytes end
    /// mid-character does: `advance 2, ids 1`. See the module header for the mechanism and
    /// for `d10f94713`, which is the fix — on a patch branch, so its absence is the default.
    Withheld,
    /// The server sent more ids than it counted. **No withheld token explains this**, so it is
    /// a different fault: the counter and the ids disagree in the direction that cannot come
    /// from a suppression, and the captured frames are where to look.
    OverSent,
}

impl Mismatch {
    /// The token a corpus or a log line carries, beside the sentence a person reads — the
    /// same split `verdict` and `model_verdict` make. Stable, because something may count it.
    pub fn as_str(self) -> &'static str {
        match self {
            Mismatch::Withheld => "withheld",
            Mismatch::OverSent => "over_sent",
        }
    }

    /// **Which it is, from the two numbers alone.** One function, because the accumulator and
    /// a reader of the capture must not be able to disagree about the line.
    pub fn of(advance: u64, ids: usize) -> Mismatch {
        if (ids as u64) < advance {
            Mismatch::Withheld
        } else {
            Mismatch::OverSent
        }
    }
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::CountMismatch {
                accumulated,
                reported,
            } => write!(
                f,
                "accumulated {accumulated} generated token ids but the server reports \
                 {reported}. The turn's token identity is unknown, so it is refused \
                 rather than reconstructed from text."
            ),
            StreamError::FrameMismatch {
                n_decoded,
                previous,
                ids,
                // **The reading decides the sentence** — R12's shape. The refusal is the
                // same; what a reader does next is not. Read from the struct rather than
                // recomputed, so the sentence and the abort cannot disagree.
                reading,
            } => match reading {
                Mismatch::Withheld => write!(
                    f,
                    "the server counted {} token(s) and sent {ids} id(s) \
                     (tokens_predicted {previous} -> {n_decoded}), so {} was withheld. That \
                     is the shape of a token whose bytes end mid-character — llama.cpp \
                     before `d10f94713`, which this box carries on branch `glm-all` and \
                     which a server rebuilt from upstream master does NOT have. Check the \
                     build; the frames are in the capture named above.",
                    n_decoded - previous,
                    n_decoded - previous - *ids as u64
                ),
                // The word `withheld` does not appear here, on purpose: see the engine's
                // arm for why, and `a_mismatch_says_which_direction_it_disagreed_in` for
                // the assertion that keeps it out.
                Mismatch::OverSent => write!(
                    f,
                    "a frame advanced tokens_predicted {previous} -> {n_decoded} but \
                     carried {ids} id(s) — MORE than it counted, which suppression cannot \
                     explain (it only ever removes ids). So this is not the UTF-8 gate: read \
                     the frames in the capture named above.",
                ),
            },
            StreamError::NoFinalChunk => write!(
                f,
                "the stream ended with no terminal frame; the turn is incomplete"
            ),
            StreamError::Protocol(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StreamError {}

/// What one generation produced, before any policy is applied to it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamOutcome {
    /// Every generated id, in order, from the partial frames.
    pub ids: Vec<TokenId>,
    /// The concatenated `content` deltas. Kept for the journal and for display
    /// only — it is never tokenized back. §4.1: the Journal cannot change a prompt.
    pub text: String,
    pub final_chunk: FinalChunk,
    /// The last progress frame seen, so `EXPLAIN` can report the prefill even when
    /// the turn was aborted before the final frame.
    pub last_progress: Option<PromptProgress>,
    /// Set when the accumulator, not the server, ended the turn.
    pub aborted: Option<AbortCause>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortCause {
    /// A dialect stop token appeared in the generated ids.
    StopToken(TokenId),
    /// §8.5's repetition-collapse guard fired.
    Guard(String),
    /// An urgent steering message or an explicit ABORT (§5.8).
    Steering(String),
    /// A frame did not account for its own advance, so the stream stopped being
    /// a record of what the model produced. See [`StreamError::FrameMismatch`] —
    /// this is the same refusal, carried as an abort so the ids that *were*
    /// accounted for survive.
    FrameMismatch {
        n_decoded: u64,
        previous: u64,
        ids: usize,
        /// Which of the two faults it was — see [`Mismatch`]. Carried on the abort as well
        /// as on the error, because the abort is what the operator's sentence is built from.
        reading: Mismatch,
    },
}

/// Accumulates ids across frames, refusing anything it cannot account for.
#[derive(Debug, Default)]
pub struct IdAccumulator {
    ids: Vec<TokenId>,
    text: String,
    n_decoded: u64,
    last_progress: Option<PromptProgress>,
    last_timings: Option<Timings>,
}

impl IdAccumulator {
    pub fn new() -> Self {
        IdAccumulator::default()
    }

    /// Feed one classified frame.
    ///
    /// Returns the ids this frame contributed, so a caller can run guards and stop
    /// checks over exactly the new tokens without re-scanning the whole generation.
    pub fn push(&mut self, chunk: &Chunk) -> Result<&[TokenId], StreamError> {
        match chunk {
            Chunk::Progress { progress, timings } => {
                self.last_progress = Some(*progress);
                if timings.is_some() {
                    self.last_timings = *timings;
                }
                Ok(&[])
            }
            Chunk::Token {
                ids,
                text,
                n_decoded,
                timings,
            } => {
                if *n_decoded <= self.n_decoded {
                    // Not an error the server can produce today, but the whole
                    // point of keying on the counter is that a frame which does not
                    // advance it contributes nothing. Silently accepting its ids is
                    // how the progress frames' zeros got in.
                    return Ok(&[]);
                }
                let advance = n_decoded - self.n_decoded;
                if ids.len() as u64 != advance {
                    return Err(StreamError::FrameMismatch {
                        n_decoded: *n_decoded,
                        previous: self.n_decoded,
                        ids: ids.len(),
                        // **The direction is the evidence**, and it is computed here rather
                        // than at each reader so the two cannot disagree about the line.
                        reading: Mismatch::of(advance, ids.len()),
                    });
                }
                let start = self.ids.len();
                self.ids.extend_from_slice(ids);
                self.text.push_str(text);
                self.n_decoded = *n_decoded;
                if timings.is_some() {
                    self.last_timings = *timings;
                }
                Ok(&self.ids[start..])
            }
            Chunk::Final(f) => {
                // The final frame's `content` is empty in stream mode and its ids
                // are empty too; both are already accounted for. Appending either
                // would double the last token.
                if !f.ids.is_empty() {
                    return Err(StreamError::Protocol(format!(
                        "the final frame carried {} id(s); this accumulator assumes \
                         stream mode, where it carries none",
                        f.ids.len()
                    )));
                }
                Ok(&[])
            }
        }
    }

    pub fn ids(&self) -> &[TokenId] {
        &self.ids
    }

    /// The server's own generation counter, as the last accepted frame reported it.
    ///
    /// Equal to [`Self::ids`] while every frame has been accountable — the
    /// accumulator only accepts a frame whose ids match its advance — and the
    /// number a head is shown while the turn runs.
    pub fn n_decoded(&self) -> u64 {
        self.n_decoded
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn last_progress(&self) -> Option<PromptProgress> {
        self.last_progress
    }

    /// The most recent `timings` object seen.
    ///
    /// Needed when *we* end the turn: closing the socket means the server's final
    /// frame never arrives, and a synthesised one carrying `Timings::default()`
    /// would report `cache_n = 0` — a cold prefill — for a turn that reused its
    /// whole history. `timings_per_token` is on precisely so this is available.
    pub fn last_timings(&self) -> Option<Timings> {
        self.last_timings
    }

    /// Close the accumulation against the terminal frame.
    ///
    /// The count check is the one that matters: it is the only thing standing
    /// between a dropped frame and a ledger that silently disagrees with the
    /// server's cache.
    pub fn finish(
        self,
        final_chunk: FinalChunk,
        aborted: Option<AbortCause>,
    ) -> Result<StreamOutcome, StreamError> {
        if aborted.is_none() && self.ids.len() as u64 != final_chunk.n_decoded {
            return Err(StreamError::CountMismatch {
                accumulated: self.ids.len(),
                reported: final_chunk.n_decoded,
            });
        }
        Ok(StreamOutcome {
            ids: self.ids,
            text: self.text,
            final_chunk,
            last_progress: self.last_progress,
            aborted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{FinishReason, classify};

    /// The exact frames this box produced for "Count from one to eight in words."
    /// with `return_progress: true`. Kept verbatim: a regression here is invisible
    /// against paraphrased fixtures.
    const FRAMES: &[&str] = &[
        r#"{"index":0,"content":"","tokens":[0],"stop":false,"tokens_predicted":0,"tokens_evaluated":20,"prompt_progress":{"total":20,"cache":0,"processed":0,"time_ms":0}}"#,
        r#"{"index":0,"content":"","tokens":[0],"stop":false,"tokens_predicted":0,"tokens_evaluated":20,"prompt_progress":{"total":20,"cache":0,"processed":16,"time_ms":71}}"#,
        r#"{"index":0,"content":"One","tokens":[3833],"stop":false,"tokens_predicted":1,"tokens_evaluated":20}"#,
        r#"{"index":0,"content":",","tokens":[11],"stop":false,"tokens_predicted":2,"tokens_evaluated":20}"#,
        r#"{"index":0,"content":" two","tokens":[1330],"stop":false,"tokens_predicted":3,"tokens_evaluated":20}"#,
        r#"{"index":0,"content":"","tokens":[248046],"stop":false,"tokens_predicted":4,"tokens_evaluated":20}"#,
        r#"{"index":0,"content":"","tokens":[],"stop":true,"tokens_predicted":4,"tokens_evaluated":20,"tokens_cached":38,"stop_type":"eos","stopping_word":"","truncated":false,"timings":{"cache_n":0,"prompt_n":20,"prompt_ms":129.4,"predicted_n":4,"predicted_ms":30.0,"draft_n":3,"draft_n_accepted":3}}"#,
    ];

    fn run(frames: &[&str]) -> Result<StreamOutcome, StreamError> {
        let mut acc = IdAccumulator::new();
        let mut fin = None;
        for f in frames {
            let c = classify(f).map_err(StreamError::Protocol)?;
            acc.push(&c)?;
            if let Chunk::Final(b) = c {
                fin = Some(*b);
            }
        }
        acc.finish(fin.ok_or(StreamError::NoFinalChunk)?, None)
    }

    /// The trap, stated as a test: the ids are in the partials and nowhere else.
    #[test]
    fn generated_ids_come_from_the_partial_frames_because_the_final_one_is_empty() {
        let out = run(FRAMES).unwrap();
        assert!(
            out.final_chunk.ids.is_empty(),
            "if this ever becomes non-empty the server changed and this module's \
             assumption must be re-derived, not patched"
        );
        assert_eq!(out.ids, vec![3833, 11, 1330, 248046]);
        assert_eq!(out.ids.len() as u64, out.final_chunk.n_decoded);
    }

    /// The second trap, which only exists because §5.6 requires `return_progress`.
    #[test]
    fn a_progress_frames_fabricated_token_zero_never_reaches_the_ledger() {
        let out = run(FRAMES).unwrap();
        assert!(
            !out.ids.contains(&0),
            "token id 0 came from a default-constructed completion_token_output, \
             not from the model: {:?}",
            out.ids
        );
        assert_eq!(out.ids.len(), 4, "two progress frames contributed no ids");
    }

    /// The regression guard the brief asked for: an implementation that reads the
    /// final frame alone must fail this, loudly.
    #[test]
    fn reading_only_the_final_frame_is_caught_rather_than_silently_falling_back() {
        let Chunk::Final(f) = classify(FRAMES[FRAMES.len() - 1]).unwrap() else {
            panic!()
        };
        let err = IdAccumulator::new().finish(*f, None).unwrap_err();
        match err {
            StreamError::CountMismatch {
                accumulated,
                reported,
            } => {
                assert_eq!(accumulated, 0);
                assert_eq!(reported, 4);
            }
            other => panic!("{other:?}"),
        }
        // And the message must say what it refuses to do, so the next person does
        // not "fix" it by re-tokenizing the text.
        assert!(format!("{err}").contains("reconstructed from text"));
    }

    #[test]
    fn text_is_accumulated_but_is_never_the_source_of_truth() {
        let out = run(FRAMES).unwrap();
        assert_eq!(out.text, "One, two");
        // The last id decoded to nothing: content and ids are different lengths and
        // only one of them is the prompt.
        assert_eq!(out.ids.len(), 4);
    }

    /// T23's frames, captured verbatim from a live `/completion` stream on
    /// 2026-09-10 — the control server, which had **no draft model and no
    /// speculative decoding at all**, so these are not an MTP artifact.
    ///
    /// Kept as bytes rather than paraphrased because the whole of T23 was that a
    /// paraphrase of this frame ("advance 2, ids 1") was all anyone had.
    const T23_FRAMES: &[&str] = &[
        r#"{"index":0,"content":"","tokens":[0],"stop":false,"id_slot":-1,"tokens_predicted":0,"tokens_evaluated":26,"prompt_progress":{"total":26,"cache":0,"processed":26,"time_ms":20}}"#,
        r#"{"index":0,"content":"😀","tokens":[141334],"stop":false,"id_slot":-1,"tokens_predicted":1,"tokens_evaluated":26}"#,
        // `" 😂"` is `[26525, 224]`. `26525` is a space plus the first three bytes
        // of a four-byte emoji, so the server suppressed its frame — and the
        // counter it advanced is reported here, by the token that completed the
        // character.
        r#"{"index":0,"content":" 😂","tokens":[224],"stop":false,"id_slot":-1,"tokens_predicted":3,"tokens_evaluated":26}"#,
        r#"{"index":0,"content":" 😃","tokens":[225],"stop":false,"id_slot":-1,"tokens_predicted":5,"tokens_evaluated":26}"#,
    ];

    #[test]
    fn the_real_t23_frames_are_refused_at_the_frame_that_stopped_adding_up() {
        let mut acc = IdAccumulator::new();
        for (i, f) in T23_FRAMES.iter().enumerate() {
            let c = classify(f).unwrap();
            match acc.push(&c) {
                Ok(_) => assert!(i < 2, "frame {i} should not have been accepted"),
                Err(StreamError::FrameMismatch {
                    n_decoded,
                    previous,
                    ids,
                    reading,
                }) => {
                    assert_eq!((previous, n_decoded, ids), (1, 3, 1));
                    // Exactly the operator's shape: two-for-one, never two-for-zero.
                    assert_eq!(n_decoded - previous, 2);
                    // **And it is read as WITHHELD, not as corruption.** A token whose
                    // bytes ended mid-character is the only thing that can make a server
                    // send fewer ids than it counted, so this frame is the detector firing
                    // on an unpatched server — see `Mismatch` and the module header.
                    assert_eq!(reading, Mismatch::Withheld);
                    // And the ids accounted for so far survive the refusal — this
                    // is what `AbortCause::FrameMismatch` then keeps.
                    assert_eq!(acc.ids(), &[141334]);
                    return;
                }
                Err(other) => panic!("{other:?}"),
            }
        }
        panic!("the frames stopped adding up and nothing said so");
    }

    /// **What the PATCHED server looks like on the wire, so the guard's silence is a
    /// measurement rather than an assumption.**
    ///
    /// Captured from the live `llama-server` on 127.0.0.1:8080 on 2026-09-22, `/completion`
    /// with `stream: true`, sixty tokens of emoji: **60 ids sent for 60 counted, 0 bad
    /// frames** — so nothing here is refused, which is what a patched server means.
    ///
    /// And the patch is *visible* in those frames rather than merely absent from the
    /// failures: **31 of the 60 carry an id with empty `content`**. Pre-`d10f94713` that
    /// frame was not sent at all — the token's bytes ended mid-character and the emit block
    /// was skipped — so every one of those 31 is a token an unpatched server would have
    /// dropped, and the guard would have refused at the first of them (token 1, before any
    /// text, so with an empty head: `partial_kept: false`).
    ///
    /// Two of these frames are in `FRAMES_LIKE_THE_SERVER_SENDS_THEM` below, byte for byte
    /// as they arrived, because the shape that proves a patch is present is worth as much as
    /// the shape that proves it is missing.
    #[test]
    fn the_patched_server_sends_the_frame_the_patch_added() {
        // The first four counter-frames of the live capture, verbatim. Two of them carry an
        // id with an empty `content` and the counter moving by exactly one — that is
        // `send_partial_response(slot, {}, …)` running unconditionally, which is
        // `d10f94713`. **Consecutive, not sampled**: the first version of this fixture took
        // frames 1 and 3 and left out 2, and the accumulator refused it for advancing the
        // counter by two — the guard catching a hand-written "patched server" that was not
        // one. A fixture for this crate has to be a stream, not a set of frames.
        let patched = [
            r#"{"index":0,"content":"","tokens":[25677],"stop":false,"id_slot":-1,"tokens_predicted":1,"tokens_evaluated":48}"#,
            r#"{"index":0,"content":" 😋","tokens":[233],"stop":false,"id_slot":-1,"tokens_predicted":2,"tokens_evaluated":48}"#,
            r#"{"index":0,"content":"","tokens":[25677],"stop":false,"id_slot":-1,"tokens_predicted":3,"tokens_evaluated":48}"#,
            r#"{"index":0,"content":" 😛","tokens":[249],"stop":false,"id_slot":-1,"tokens_predicted":4,"tokens_evaluated":48}"#,
        ];
        let mut acc = IdAccumulator::new();
        for (i, f) in patched.iter().enumerate() {
            let c = classify(f).unwrap();
            acc.push(&c)
                .unwrap_or_else(|e| panic!("frame {i} of a patched server was refused: {e}"));
        }
        // Every id arrived, in order, and the counter agrees with them.
        assert_eq!(acc.ids(), &[25677, 233, 25677, 249]);
        assert_eq!(acc.n_decoded(), 4, "the counter is the server's own");

        // **And the same two frames assembled the way the unpatched server sends them.**
        // The empty-content frame is simply not there: the client never learns about token
        // 1, and the next frame it sees advances the counter by two while carrying one id.
        let unpatched = [
            r#"{"index":0,"content":" 😋","tokens":[233],"stop":false,"id_slot":-1,"tokens_predicted":2,"tokens_evaluated":48}"#,
        ];
        let mut acc = IdAccumulator::new();
        let Err(e) = acc.push(&classify(unpatched[0]).unwrap()) else {
            panic!("an unpatched server's first frame must be refused")
        };
        assert!(
            matches!(
                e,
                StreamError::FrameMismatch {
                    reading: Mismatch::Withheld,
                    n_decoded: 2,
                    previous: 0,
                    ids: 1,
                }
            ),
            "the withheld reading, and the FIRST frame: {e:?}"
        );
        // **Nothing is kept**, because the very first token is the one that was withheld —
        // which is the honest version of "the partial answer it preserves".
        assert!(acc.ids().is_empty());
        assert!(e.to_string().contains("d10f94713"), "{e}");
    }

    /// **The two faults are told apart by the direction, and the direction is arithmetic.**
    ///
    /// R12's shape in a second family: one refusal, two facts, and the first thing to check
    /// differs. `ids < advance` is a server that *withheld* what it counted — the UTF-8 gate
    /// and nothing else, because suppression only ever removes ids. `ids > advance` cannot be
    /// that, whatever else it is.
    ///
    /// The refusal is identical and is asserted here as identical: `Mismatch` classifies the
    /// same fault, it does not excuse it. What changes is the sentence, which is where an
    /// operator reads what to do next — and this test pins both, so a later rewrite of either
    /// has to say why.
    #[test]
    fn a_mismatch_says_which_direction_it_disagreed_in() {
        // The arithmetic, on its own: one function, so the accumulator and a reader cannot
        // disagree about the line.
        assert_eq!(Mismatch::of(2, 1), Mismatch::Withheld);
        assert_eq!(Mismatch::of(3, 1), Mismatch::Withheld);
        assert_eq!(Mismatch::of(1, 2), Mismatch::OverSent);
        assert_eq!(Mismatch::of(1, 3), Mismatch::OverSent);
        assert_eq!(Mismatch::as_str(Mismatch::Withheld), "withheld");
        assert_eq!(Mismatch::as_str(Mismatch::OverSent), "over_sent");

        // **The withheld sentence names the build**, because that is the thing to check and
        // the one fact this box can name: the fix is a patch on a branch, so a server
        // rebuilt from upstream does not have it.
        let withheld = StreamError::FrameMismatch {
            n_decoded: 3,
            previous: 1,
            ids: 1,
            reading: Mismatch::Withheld,
        }
        .to_string();
        assert!(withheld.contains("2 token(s)"), "{withheld}");
        assert!(withheld.contains("1 id(s)"), "{withheld}");
        assert!(withheld.contains("withheld"), "{withheld}");
        assert!(withheld.contains("d10f94713"), "{withheld}");
        assert!(withheld.contains("glm-all"), "{withheld}");
        assert!(withheld.contains("capture"), "it must say where the evidence is: {withheld}");

        // **The over-sent sentence says the opposite thing**, and says it without naming a
        // cause it cannot know: it rules the gate OUT rather than naming another fault.
        let over = StreamError::FrameMismatch {
            n_decoded: 1,
            previous: 0,
            ids: 2,
            reading: Mismatch::OverSent,
        }
        .to_string();
        assert!(over.contains("MORE than it counted"), "{over}");
        assert!(
            over.contains("suppression cannot"),
            "the whole value of this half is that it excludes the gate: {over}"
        );
        assert!(!over.contains("d10f94713"), "the wrong remedy: {over}");
        // **A grep for the gate must not hit the other fault.** `withheld` is the reading's
        // name, so it is what a session log is searched for — and the over-sent sentence
        // says the opposite thing, which is not a place that word belongs.
        assert!(
            !over.contains("withheld"),
            "the opposite reading's name, in the sentence that rules it out: {over}"
        );
        assert_ne!(withheld, over, "two faults, one sentence is the defect");
    }

    /// **The refusal itself is unchanged by the reading** — both directions stop the turn at
    /// that frame and keep what came before, which is the property R12 must not have traded
    /// for a nicer sentence.
    #[test]
    fn both_directions_refuse_the_same_way() {
        // Over-sent: two ids for one counted.
        let over = [
            r#"{"content":"a","tokens":[1,2],"stop":false,"tokens_predicted":1}"#,
            r#"{"content":"","tokens":[],"stop":true,"tokens_predicted":1,"stop_type":"eos"}"#,
        ];
        assert!(matches!(
            run(&over),
            Err(StreamError::FrameMismatch {
                reading: Mismatch::OverSent,
                ..
            })
        ));
        // Withheld: the real T23 frames, two counted and one sent.
        let mut acc = IdAccumulator::new();
        for f in &T23_FRAMES[..3] {
            let c = classify(f).unwrap();
            if let Err(e) = acc.push(&c) {
                assert!(matches!(
                    e,
                    StreamError::FrameMismatch {
                        reading: Mismatch::Withheld,
                        ..
                    }
                ));
                // And the ids accounted for before it are still there — the same guarantee
                // the over-sent case gets.
                assert_eq!(acc.ids(), &[141334]);
                return;
            }
        }
        panic!("the real frames stopped being a mismatch — the note above is now wrong");
    }

    #[test]
    fn a_frame_whose_ids_disagree_with_its_counter_is_refused() {
        let frames = [
            r#"{"content":"a","tokens":[1,2],"stop":false,"tokens_predicted":1}"#,
            r#"{"content":"","tokens":[],"stop":true,"tokens_predicted":1,"stop_type":"eos"}"#,
        ];
        assert!(matches!(
            run(&frames),
            Err(StreamError::FrameMismatch { .. })
        ));
    }

    #[test]
    fn an_aborted_turn_keeps_what_it_had_without_a_count_check() {
        let mut acc = IdAccumulator::new();
        for f in &FRAMES[..4] {
            acc.push(&classify(f).unwrap()).unwrap();
        }
        let Chunk::Final(f) = classify(FRAMES[FRAMES.len() - 1]).unwrap() else {
            panic!()
        };
        let out = acc
            .finish(*f, Some(AbortCause::Guard("repetition_collapse".into())))
            .unwrap();
        assert_eq!(out.ids, vec![3833, 11]);
        assert!(out.aborted.is_some());
        assert_eq!(out.final_chunk.finish_reason, FinishReason::Eos);
    }
}
