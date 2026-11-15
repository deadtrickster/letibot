//! The canned turn a replayed provider answers with, spelled in Qwen's own ids: an id chosen for
//! convenience decodes to a token the harness never reads as the end of a thought, and the round
//! loop then runs on to the end of the script.

use letibot_tokencore::Vocab;

use super::canned::Frame;

/// The end-of-turn token, the one thing that stops the round loop.
pub const IM_END: u32 = 248046;

/// The token that closes the reasoning block, so the answer after it is a reply and not more
/// thinking.
pub const THINK_CLOSE: u32 = 248069;

pub fn ids(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

pub fn spoken(vocab: &Vocab, ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn that answers in plain text: the model thinks, closes the block, answers, ends.
/// No tool calls, so the round loop ends after one round.
pub fn a_plain_answer_turn(
    vocab: &Vocab,
    thought: &str,
    answer: &str,
    n_prompt: u64,
) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The tokens Qwen spells a tool call with.
pub const TOOL_CALL_OPEN: u32 = 248058;
pub const TOOL_CALL_CLOSE: u32 = 248059;

/// A round that thinks, says `text` and makes one call, spelled in Qwen's call syntax
/// (`<function=name>…</function>`). The turn goes on to the next round.
pub fn a_call_turn(
    vocab: &Vocab,
    thought: &str,
    text: &str,
    call: &str,
    n_prompt: u64,
) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let text_ids = ids(vocab, text);
    let call_ids = ids(vocab, call);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &text_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + text_ids.len() + call_ids.len() + 3) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}
