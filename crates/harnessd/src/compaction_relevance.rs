//! Which old turns a compaction keeps in full because they are about the work in hand.
//!
//! A compaction decides by age and size: the newest exchanges are carried verbatim and the rest is
//! summarised. With the second tier on, the old half is scored against the plan once more, and an
//! exchange that answers a row on the board is kept whole - a summary of the turn that decided
//! something is worth less than the turn itself. Mode off scores nothing, so the plan stays the one
//! age and size computed, which is the order this tree had before the tier existed.

use std::path::PathBuf;

use letibot_embeddings::Tier;
use letibot_tokencore::store::TodoItem;
use letibot_tools::similarity::Corpus;
use letibot_transcript::{TranscriptItem, UserPart};

/// The score an old exchange has to reach before it is kept whole. Higher than the offers' floors:
/// a wrong offer costs a line, and a wrong keep costs the room a summary would have used.
pub const FLOOR: f64 = 0.35;

/// How many characters of one item are scored. A turn's own text is what it decided; the 200 KB a
/// tool printed is the same 200 KB in every round, and embedding it again is the cost without the
/// signal.
pub const ITEM_CHARS: usize = 2_000;

/// The plan as one text: the rows the session is working from, which is what an old turn is scored
/// against. Empty when there is no plan, and then nothing is kept for being relevant.
pub fn query(plan: &[TodoItem]) -> String {
    plan.iter()
        .map(|row| row.content.trim())
        .filter(|row| !row.is_empty())
        .collect::<Vec<&str>>()
        .join("\n")
}

/// One score per item, blended, from the second tier - and `None` when there is no tier or no plan,
/// which is every case in which the age-based plan stands.
pub fn scores(
    items: &[TranscriptItem],
    plan: &[TodoItem],
    tier: Option<&Tier>,
) -> Option<Vec<f64>> {
    let query = query(plan);
    if query.trim().is_empty() || items.is_empty() {
        return None;
    }
    let tier = tier?;
    let docs = keys(items);
    let corpus = Corpus::of_texts(&docs);
    // A fill that cannot answer empties the tier, so what is left is the words: the plan is not
    // moved by a meaning the box could not make.
    let _ = corpus.fill(Some(tier));
    let mut out = vec![0.0; items.len()];
    for (path, score) in corpus.ranked_with(&query, docs.len(), Some(tier)) {
        if let Some(i) = path.to_str().and_then(|s| s.parse::<usize>().ok())
            && i < out.len()
        {
            out[i] = score;
        }
    }
    Some(out)
}

/// The start of the verbatim region, moved earlier over the old exchanges that are about the plan
/// while `room` (the tokens the region may spend) allows. Never zero: a compaction that summarised
/// nothing would be a session with no record of its own history.
pub fn keep(
    items: &[TranscriptItem],
    tokens: &[u64],
    from: usize,
    room: u64,
    scores: &[f64],
) -> usize {
    let n = items.len().min(tokens.len()).min(scores.len());
    if n == 0 || from > n {
        return from;
    }
    // What the region already spends on the newest turns; an old exchange is paid for out of what
    // is left, so a keep cannot grow the prompt past the budget the plan was built for.
    let kept: u64 = tokens[from.min(n)..n].iter().sum();
    let mut free = room.saturating_sub(kept);
    // Where an exchange can begin: the first item, and every user item after it.
    let mut starts: Vec<usize> = vec![0];
    for (i, item) in items.iter().enumerate().take(from).skip(1) {
        if matches!(item, TranscriptItem::User { .. }) {
            starts.push(i);
        }
    }
    // Each old exchange by the best score inside it, best first, and only the ones about the plan.
    let mut ranked: Vec<(usize, f64)> = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(from);
        let best = scores[start..end]
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        // The first exchange is never pulled: something has to be summarised, or the fold is a
        // copy of the conversation.
        if start > 0 && best >= FLOOR {
            ranked.push((start, best));
        }
    }
    ranked.sort_by(|(pa, sa), (pb, sb)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| pa.cmp(pb))
    });
    let mut at = from;
    for (start, _) in ranked {
        if start >= at {
            continue;
        }
        let cost: u64 = tokens[start..at].iter().sum();
        if cost > free {
            continue;
        }
        free -= cost;
        at = start;
    }
    at
}

/// What an item says, for the one thing this module asks of it.
pub fn text_of(item: &TranscriptItem) -> String {
    let whole = match item {
        TranscriptItem::System { text, .. }
        | TranscriptItem::Reasoning { text, .. }
        | TranscriptItem::Assistant { text, .. } => text.clone(),
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<&str>>()
            .join("\n"),
        TranscriptItem::ToolResult { payload, .. } => payload.clone(),
        TranscriptItem::SegmentMark { .. } => String::new(),
    };
    // The cut lands on a character boundary, never through one: an item is prose in any script.
    let cut = ITEM_CHARS.min(whole.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| whole.is_char_boundary(i))
        .unwrap_or(0);
    whole[..cut].to_string()
}

/// The corpus keys, which are the item indices: a score comes back as the item it belongs to.
pub fn keys(items: &[TranscriptItem]) -> Vec<(PathBuf, String)> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| (PathBuf::from(format!("{i}")), text_of(item)))
        .collect()
}

/// The census, in its own file.
#[cfg(test)]
mod tests;
