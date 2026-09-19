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
//! T23, and it is a **defect in llama.cpp**, measured on this box 2026-09-10.
//! `process_token` (`server-context.cpp:4067`) computes
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
//! # Why the guard stays, and what changed instead
//!
//! The id is genuinely gone. Accepting the frame anyway would write a ledger whose
//! ids are not what the model produced, and the hash chain exists to catch exactly
//! that. So the frame is still refused. What changed is the blast radius: the
//! refusal is carried out as [`AbortCause::FrameMismatch`], which keeps every id
//! that *was* accounted for, so the operator gets a turn marked interrupted with a
//! partial answer instead of a turn that recorded nothing.

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
    },
    /// The stream ended without a terminal frame.
    NoFinalChunk,
    Protocol(String),
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
            } => write!(
                f,
                "a frame advanced tokens_predicted {previous} -> {n_decoded} but carried \
                 {ids} id(s)"
            ),
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
                }) => {
                    assert_eq!((previous, n_decoded, ids), (1, 3, 1));
                    // Exactly the operator's shape: two-for-one, never two-for-zero.
                    assert_eq!(n_decoded - previous, 2);
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
