//! `/note accept N` and `/note decline N`: the operator's answer to a shadow note proposal. An
//! accept writes the note through the `notes` tool, with the quote's provenance in its text; a
//! decline writes nothing.

use letibot_transcript::{OfferKind, ToolCall, TranscriptItem};

use crate::shadow::turn_end::SESSION_LINE;

const HEAD: &str = "shadow suggests saving a note:";
const FROM: &str = "\n\nquoted from ";

/// One proposal, read back from the row the shadow wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub quote: String,
    pub author: String,
    /// The quoted row in the watched session, when the shadow knew it.
    pub row: Option<usize>,
    pub shadow_session: String,
}

/// The proposal a row holds, `None` for any other row.
pub fn proposal_of(item: &TranscriptItem) -> Option<Proposal> {
    let TranscriptItem::Offer {
        kind: OfferKind::Shadow,
        body,
        ..
    } = item
    else {
        return None;
    };
    let rest = body.strip_prefix(HEAD)?.trim_start_matches('\n');
    // The quote may hold blank lines, so the provenance is found from the end.
    let at = rest.rfind(FROM)?;
    let quote = rest[..at].trim().to_string();
    let mut tail = rest[at + FROM.len()..].lines();
    let from = tail.next()?.trim();
    let shadow_session = tail
        .find_map(|l| l.trim().strip_prefix(SESSION_LINE))?
        .trim()
        .to_string();
    let (from, row) = match from.rsplit_once(" (row ") {
        Some((head, n)) => (head, n.strip_suffix(')').and_then(|n| n.parse().ok())),
        None => (from, None),
    };
    let author = match from.rsplit_once(" at ") {
        Some((who, t)) if t.ends_with(" UTC") => who,
        _ => from,
    };
    (!quote.is_empty()).then(|| Proposal {
        quote,
        author: author.to_string(),
        row,
        shadow_session,
    })
}

/// The row a bare `/note accept` means: the newest proposal.
pub fn latest(items: &[TranscriptItem]) -> Option<usize> {
    items.iter().rposition(|i| proposal_of(i).is_some())
}

/// The line a proposal row ends with, naming its own row so the verb can find it.
pub fn hint(offer_row: usize) -> String {
    format!("`/note accept {offer_row}` saves it · `/note decline {offer_row}` drops it")
}

/// The note's name: the quote's first words, so two proposals of one quote land on one name and
/// the second accept is refused as `add` over an existing note.
pub fn name_of(quote: &str) -> String {
    let mut out = String::new();
    for c in quote.chars() {
        if out.len() >= 48 {
            break;
        }
        match c {
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ if !out.is_empty() && !out.ends_with('-') => out.push('-'),
            _ => {}
        }
    }
    let out = out.trim_end_matches('-');
    match out.is_empty() {
        true => "shadow-note".into(),
        false => format!("shadow-{out}"),
    }
}

/// The `notes add` call an accept makes. `quoted_at` is the quoted row's time, `session` the
/// watched session the row is in.
pub fn note_call(
    p: &Proposal,
    offer_row: usize,
    session: &str,
    quoted_at: Option<&str>,
) -> ToolCall {
    let row = p.row.map(|r| format!(", row {r}")).unwrap_or_default();
    let at = quoted_at.map(|t| format!(" at {t}")).unwrap_or_default();
    let text = format!(
        "{}\n\nSource: quoted from {}{at}, session {session}{row}. Proposed by shadow {} \
         (row {offer_row}), accepted by the operator.",
        p.quote, p.author, p.shadow_session,
    );
    let first = p.quote.lines().next().unwrap_or("").trim();
    let args = serde_json::json!({
        "action": "add",
        "name": name_of(&p.quote),
        "abstract": first,
        "text": text,
    });
    ToolCall {
        id: format!("note-accept-{offer_row}"),
        name: "notes".into(),
        arguments: args.to_string(),
    }
}

/// `YYYY-MM-DD HH:MM UTC` for a unix millisecond stamp.
pub fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{} {:02}:{:02} UTC",
        crate::recall_offer::date_of(ms),
        secs / 3600,
        (secs / 60) % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::{turn_end, verdict::Quoted};

    fn row(text: &str, at: Option<&str>, n: Option<usize>) -> TranscriptItem {
        let q = Quoted {
            text: text.into(),
            author: "the operator".into(),
            row: n,
        };
        turn_end::note_row(&q, at, "s-w-shadow")
    }

    #[test]
    fn a_proposal_reads_back_from_its_row() {
        let p = proposal_of(&row(
            "the gate runs\n\non lab2x1",
            Some("12:08 UTC"),
            Some(41),
        ))
        .expect("a proposal");
        assert_eq!(p.quote, "the gate runs\n\non lab2x1");
        assert_eq!(p.author, "the operator");
        assert_eq!(p.row, Some(41));
        assert_eq!(p.shadow_session, "s-w-shadow");
        let bare = proposal_of(&row("x", None, None)).expect("a proposal");
        assert_eq!((bare.author.as_str(), bare.row), ("the operator", None));
    }

    #[test]
    fn other_rows_are_no_proposal() {
        let other = TranscriptItem::Offer {
            kind: OfferKind::Shadow,
            title: "during · offer".into(),
            body: "skill offered".into(),
        };
        assert_eq!(proposal_of(&other), None);
        let items = vec![row("a", None, None), other];
        assert_eq!(latest(&items), Some(0));
    }

    #[test]
    fn the_call_carries_the_provenance() {
        let p = proposal_of(&row("Gate runs on lab2x1.", None, Some(41))).unwrap();
        let call = note_call(&p, 57, "s-w", Some("2026-11-15 12:08 UTC"));
        let args: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
        assert_eq!(args["action"], "add");
        assert_eq!(args["name"], "shadow-gate-runs-on-lab2x1");
        let text = args["text"].as_str().unwrap();
        assert!(
            text.contains(
                "quoted from the operator at 2026-11-15 12:08 UTC, session s-w, row 41. \
                 Proposed by shadow s-w-shadow (row 57)"
            ),
            "{text}"
        );
        assert_eq!(stamp(1_763_208_480_000), "2025-11-15 12:08 UTC");
    }
}
