//! The index the recall offer scores against: this project's past conversation as units, kept in
//! the project's own store file with the tier's vector for each.
//!
//! Nothing here runs at open. A turn asks once, the scan skips the transcript the session is
//! carrying, and what it finds is cached by unit id - so a warm turn reads no rows and embeds
//! nothing.

use std::collections::BTreeMap;
use std::path::Path;

use letibot_embeddings::Tier;
use letibot_tokencore::store::{Archived, RecallUnit, Store};

use crate::recall_offer::{EMBED_MAX, ROWS_MAX, SESSIONS_MAX, TRANSCRIPTS_MAX, UNITS_MAX, units};

/// What one read of the index found, and what it cost.
#[derive(Debug, Default)]
pub struct Built {
    /// Every unit the index holds, newest first - not only what this read added.
    pub units: Vec<RecallUnit>,
    /// The vectors this model already made, by unit id.
    pub vectors: BTreeMap<String, Vec<f32>>,
    /// How many vectors this read added, for the report and the test that says a warm index costs
    /// no embedding at all.
    pub embedded: usize,
    /// How many transcripts this read had to read. A warm index reads none, which is the whole of
    /// what "cached by item id" buys.
    pub scanned: usize,
}

/// Read the index for `workspace` and warm what it still owes, or `None` when this session has no
/// store - a session that keeps its conversation in memory has no past beyond its own prompt.
///
/// `live` is the transcript the session is carrying: its rows ARE the prompt, and a hint about what
/// the prompt already says is a line spent saying nothing. `model` keys the cached vectors, so a box
/// that switches embedders re-earns them rather than comparing two widths.
pub fn build(
    store: Option<&Store>,
    workspace: &Path,
    live: Option<&str>,
    model: &str,
    tier: Option<&Tier>,
) -> Built {
    let Some(store) = store else {
        return Built::default();
    };
    // What the index holds already decides what NOT to read: a transcript's rows never change once
    // the next generation is written, so a transcript the index has any unit of is done with. A
    // warm turn therefore reads no rows at all, and only a new compaction costs a scan.
    let indexed = store.recall_units(UNITS_MAX).unwrap_or_default();
    let known: std::collections::HashSet<&str> = indexed
        .iter()
        .filter_map(|u| u.unit.rsplit_once('#').map(|(transcript, _)| transcript))
        .collect();
    let (found, scanned) = scan(store, workspace, live, &known);
    if !found.is_empty()
        && let Err(e) = store.put_recall_units(&found)
    {
        // A cache that cannot be written is a colder cache and never a failed turn: the offer
        // scores what is already indexed.
        eprintln!("  recall index: {e}");
    }
    let units = store.recall_units(UNITS_MAX).unwrap_or_default();
    let mut vectors = store.recall_vectors(model).unwrap_or_default();
    let embedded = warm(store, model, tier, &mut vectors);
    Built {
        units,
        vectors,
        embedded,
        scanned,
    }
}

/// Embed what the index owes, bounded to [`EMBED_MAX`] units a turn. A unit whose vector cannot be
/// made is left without one: it scores on its words, and the next turn owes it again.
fn warm(
    store: &Store,
    model: &str,
    tier: Option<&Tier>,
    vectors: &mut BTreeMap<String, Vec<f32>>,
) -> usize {
    let Some(tier) = tier else {
        return 0;
    };
    let Ok(pending) = store.recall_pending(model, EMBED_MAX) else {
        return 0;
    };
    let mut added: Vec<(String, Vec<f32>)> = Vec::new();
    for (unit, text) in pending {
        if let Ok(vector) = tier.query(&text, now_ms()) {
            added.push((unit, vector));
        }
    }
    if added.is_empty() {
        return 0;
    }
    if let Err(e) = store.put_recall_vectors(model, &added, now_ms() as u64) {
        eprintln!("  recall index: {e}");
        return 0;
    }
    // The new vectors are this turn's own, so they score now rather than next turn: a cache built
    // behind the caller's back would make the first turn after it always cold.
    for (unit, vector) in &added {
        vectors.insert(unit.clone(), vector.clone());
    }
    added.len()
}

/// The units the index does not have yet: the project's transcripts that are not in the window,
/// newest first, read down to the bounds the offer names.
fn scan(
    store: &Store,
    workspace: &Path,
    live: Option<&str>,
    known: &std::collections::HashSet<&str>,
) -> (Vec<RecallUnit>, usize) {
    let want = workspace.display().to_string();
    let Ok(mut sessions) = store.list_sessions_archived(Archived::Any) else {
        return (Vec::new(), 0);
    };
    // The project's own sessions, the archived ones included (R184): an archived session is out of
    // sight, and its work is exactly what "remember when we did that?" is about. A subagent and a
    // package are skipped by default - a child's turns re-tell its parent's work and a package's
    // are the same brief from another seat, so they crowd the index without answering anything.
    sessions.retain(|s| {
        s.workspace_root == want
            && s.parent_session_id.is_none()
            && s.origin.as_deref() != Some("package")
    });
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
    sessions.truncate(SESSIONS_MAX);

    let mut out: Vec<RecallUnit> = Vec::new();
    let mut transcripts = 0usize;
    let mut rows = 0usize;
    let mut scanned = 0usize;
    for session in &sessions {
        let title = session.title.clone().unwrap_or_default();
        for (transcript, _) in store
            .recall_transcripts(&session.id, TRANSCRIPTS_MAX)
            .unwrap_or_default()
        {
            if Some(transcript.as_str()) == live
                || known.contains(transcript.as_str())
                || transcripts >= TRANSCRIPTS_MAX
                || rows >= ROWS_MAX
            {
                continue;
            }
            transcripts += 1;
            scanned += 1;
            let taken = store
                .recall_rows(&transcript, ROWS_MAX - rows)
                .unwrap_or_default();
            rows += taken.len();
            out.extend(units(&taken, &session.id, &transcript, &title));
        }
    }
    // The newest units are what a round can afford to score, and the scan walks newest first, so
    // the cut keeps the half that a question about the recent past is about.
    out.truncate(UNITS_MAX);
    (out, scanned)
}

/// The tier's clock, as the embeddings cache stamps it.
fn now_ms() -> i64 {
    letibot_metrics::now_ms() as i64
}
