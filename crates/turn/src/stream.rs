//! Accumulating generated token ids out of a `/completion` stream.
//!
//! The final frame carries an empty `tokens` array in stream mode, so ids come from the partial
//! frames. Re-tokenizing `content` instead reintroduces the detokenize/retokenize round trip the
//! ledger exists to detect and silently diverges the cache:
//!
//! ```text
//!  17 stop=0 tokens=[248046] predicted=17 content=''
//!  18 stop=1 tokens=[]       predicted=17 content=''
//! ```
//!
//! `return_progress: true` makes llama.cpp send progress frames carrying a fabricated token id 0,
//! so accumulating `tokens` from every non-final frame corrupts the ledger on the first turn:
//!
//! ```text
//!   1 stop=0 prog=True tokens=[0] predicted=0 content=''
//!   2 stop=0 prog=True tokens=[0] predicted=0 content=''
//!   3 stop=0 prog=False tokens=[3833] predicted=1 content='One'
//! ```
//!
//! The rule: accept a frame's ids only when the server's `tokens_predicted` advanced, and take
//! exactly that many. At the end the count must equal the final frame's, or the turn is refused.
//!
//! Unpatched llama.cpp emits no frame for a token ending in a partial UTF-8 character, and the
//! counter advance shows up on the next frame, which carries only its own id:
//!
//! ```text
//! bool incomplete = validate_utf8(slot.generated_text) < slot.generated_text.size();
//! ```
//!
//! ```text
//!   {"content":"😀",  "tokens":[141334],"tokens_predicted":1}
//!   {"content":" 😂", "tokens":[224],   "tokens_predicted":3}   ← advance 2, ids 1
//! ```
//!
//! Signature: `advance 2, ids 1` (or `3, 1`), content-dependent (emoji, not ASCII prose),
//! not speculative decoding. The fix lives on a local patch branch of the server, so most
//! servers lack it and this accumulator is the detector - do not delete it.
//!
//! Against an unpatched server [`IdAccumulator::push`] stops at the first frame that does not
//! account for its advance, keeps the ids before it, and the turn aborts as
//! [`AbortCause::FrameMismatch`] holding that partial (empty if the first frame disagrees).
//! The frames are written out verbatim and announced as `frame_capture_written` (routine); a
//! failed capture is `frame_capture_failed`. [`Mismatch`] gives the direction: `ids < advance`
//! means the server withheld ids, `ids > advance` means it sent more than it counted.
//! To check the server when the sentence says withheld:
//!
//! ```text
//! cd ~/Projects/llama.cpp && git log -1 --format='%h %s' d10f94713
//! git merge-base --is-ancestor d10f94713 HEAD && echo patched || echo UNPATCHED
//! ```
//!
//! The guard is a function of what arrives, not of which server is running.

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
        /// Which fault it was - see [`Mismatch`]. Two different facts, one refusal,
        /// and the difference is free.
        reading: Mismatch,
    },
    /// The stream ended without a terminal frame.
    NoFinalChunk,
    Protocol(String),
}

/// Which way a frame failed to account for itself - the direction is the evidence, and what
/// differs is the first thing to check. A server that withholds can only send FEWER ids than
/// it counted, so `ids > advance` is provably not the UTF-8 gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// The server counted more tokens than it sent. This is what a token whose bytes end
    /// mid-character does: `advance 2, ids 1`. See the module header for the mechanism and
    /// for `d10f94713`, which is the fix - on a patch branch, so its absence is the default.
    Withheld,
    /// The server sent more ids than it counted. No withheld token explains this, so it is
    /// a different fault: the counter and the ids disagree in the direction that cannot come
    /// from a suppression, and the captured frames are where to look.
    OverSent,
}

impl Mismatch {
    /// The token a corpus or a log line carries, beside the sentence a person reads - the
    /// same split `verdict` and `model_verdict` make. Stable, because something may count it.
    pub fn as_str(self) -> &'static str {
        match self {
            Mismatch::Withheld => "withheld",
            Mismatch::OverSent => "over_sent",
        }
    }

    /// Which it is, from the two numbers alone. One function, because the accumulator and
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
                // The reading decides the sentence. The refusal is the
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
    /// only - it is never tokenized back. §4.1: the Journal cannot change a prompt.
    pub text: String,
    pub final_chunk: FinalChunk,
    /// The last progress frame seen, so `EXPLAIN` can report the prefill even when
    /// the turn was aborted before the final frame.
    pub last_progress: Option<PromptProgress>,
    /// Set when the accumulator, not the server, ended the turn.
    pub aborted: Option<AbortCause>,
    /// Ms from the request to the first frame that carried an id - set by the engine, which
    /// holds the clock.
    pub first_byte_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortCause {
    /// A dialect stop token appeared in the generated ids.
    StopToken(TokenId),
    /// §8.5's repetition-collapse guard fired.
    Guard(String),
    /// An urgent steering message or an explicit ABORT (§5.8).
    Steering(String),
    /// A frame did not account for its own advance, so the stream stopped being a record of
    /// what the model produced. Carried as an abort so the ids that were accounted for
    /// survive.
    FrameMismatch {
        n_decoded: u64,
        previous: u64,
        ids: usize,
        /// Which of the two faults it was - see [`Mismatch`]. Carried on the abort as well
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

    /// Feed one classified frame. Returns the ids this frame contributed, so a caller can run
    /// guards and stop checks over exactly the new tokens without re-scanning the generation.
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
                    // Not an error the server produces today, but the whole point of keying on
                    // the counter is that a frame which does not advance it contributes nothing;
                    // silently accepting its ids is how the progress frames' zeros got in.
                    return Ok(&[]);
                }
                let advance = n_decoded - self.n_decoded;
                if ids.len() as u64 != advance {
                    return Err(StreamError::FrameMismatch {
                        n_decoded: *n_decoded,
                        previous: self.n_decoded,
                        ids: ids.len(),
                        // The direction is the evidence, and it is computed here rather
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

    /// The server's own generation counter, as the last accepted frame reported it: equal to
    /// [`Self::ids`] while every frame has been accountable, and the number a head is shown
    /// while the turn runs.
    pub fn n_decoded(&self) -> u64 {
        self.n_decoded
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn last_progress(&self) -> Option<PromptProgress> {
        self.last_progress
    }

    /// The most recent `timings` object seen. Needed when we end the turn: the server's final
    /// frame never arrives, and a synthesised one with default timings would report a cold
    /// prefill for a turn that reused its whole history.
    pub fn last_timings(&self) -> Option<Timings> {
        self.last_timings
    }

    /// Close the accumulation against the terminal frame. The count check is the only thing
    /// between a dropped frame and a ledger that silently disagrees with the server's cache.
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
            first_byte_ms: None,
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

    /// Frames captured verbatim from a live `/completion` stream on a control server
    /// with no draft model, so these are not a speculative-decoding artifact. Bytes rather
    /// than a paraphrase, because a paraphrase was all anyone had.
    const T23_FRAMES: &[&str] = &[
        r#"{"index":0,"content":"","tokens":[0],"stop":false,"id_slot":-1,"tokens_predicted":0,"tokens_evaluated":26,"prompt_progress":{"total":26,"cache":0,"processed":26,"time_ms":20}}"#,
        r#"{"index":0,"content":"😀","tokens":[141334],"stop":false,"id_slot":-1,"tokens_predicted":1,"tokens_evaluated":26}"#,
        // `" 😂"` is `[26525, 224]`. `26525` is a space plus the first three bytes of a
        // four-byte emoji, so the server suppressed its frame and the counter advanced by the
        // token that completed the character.
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
                    // Read as WITHHELD, not as corruption: a token whose bytes ended
                    // mid-character is the only thing that can make a server send fewer ids
                    // than it counted, so this is the detector firing on an unpatched server.
                    assert_eq!(reading, Mismatch::Withheld);
                    // And the ids accounted for so far survive the refusal - this
                    // is what `AbortCause::FrameMismatch` then keeps.
                    assert_eq!(acc.ids(), &[141334]);
                    return;
                }
                Err(other) => panic!("{other:?}"),
            }
        }
        panic!("the frames stopped adding up and nothing said so");
    }

    /// What the PATCHED server looks like on the wire, so the guard's silence is a
    /// measurement rather than an assumption: a live capture where 60 ids were sent for 60
    /// counted, 31 of them carrying an id with empty `content`.
    #[test]
    fn the_patched_server_sends_the_frame_the_patch_added() {
        // The first four counter-frames of the live capture, verbatim, and consecutive: a
        // fixture for this crate has to be a stream, not a set of frames, or the accumulator
        // refuses it for advancing the counter by two.
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

        // And the same two frames assembled the way the unpatched server sends them.
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
        // Nothing is kept, because the very first token is the one that was withheld  -
        // which is the honest version of "the partial answer it preserves".
        assert!(acc.ids().is_empty());
        assert!(e.to_string().contains("d10f94713"), "{e}");
    }

    /// The two faults are told apart by the direction, and the direction is arithmetic: `ids <
    /// advance` is a server that withheld what it counted, `ids > advance` cannot be that. The
    /// refusal is identical; what changes is the sentence an operator reads.
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

        // The withheld sentence names the build, because that is the thing to check and
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
        assert!(
            withheld.contains("capture"),
            "it must say where the evidence is: {withheld}"
        );

        // The over-sent sentence says the opposite thing, and says it without naming a
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
        // A grep for the gate must not hit the other fault. `withheld` is the reading's
        // name, so it is what a session log is searched for - and the over-sent sentence
        // says the opposite thing, which is not a place that word belongs.
        assert!(
            !over.contains("withheld"),
            "the opposite reading's name, in the sentence that rules it out: {over}"
        );
        assert_ne!(withheld, over, "two faults, one sentence is the defect");
    }

    /// The refusal itself is unchanged by the reading - both directions stop the turn at
    /// that frame and keep what came before, which the nicer sentence must not trade away.
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
        // Withheld: the real frames, two counted and one sent.
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
                // And the ids accounted for before it are still there - the same guarantee
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
