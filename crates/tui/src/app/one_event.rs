//! One event, one row: a warning's card stands for the session rows that say the same event (a
//! reseat, a restart, a red target, a stale prompt), so those rows are absorbed into it rather than
//! dropped - they keep their place in the transcript, and their text rides the card's popup.

use super::*;
use crate::ui::transcript::{job_card, red_card, stale_card};

/// How far from a warning's seam its session rows may sit, in rows.
pub(crate) const WINDOW: usize = 3;

/// The System row a reseat writes (`harnessd`'s re-seat note).
const RESEAT_SYSTEM: &str = "This conversation was re-seated onto a new prompt";

/// The four events that drew more than one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Reseat,
    Restart,
    Red,
    Stale,
}

/// The event a note announces, when it is one of the four.
pub(crate) fn of_note(n: &Note) -> Option<(Event, &str)> {
    let Note::Warned(w) = n else {
        return None;
    };
    let e = match w.code.as_str() {
        "reseated" => Event::Reseat,
        "restart_restored" | "restart_reminded" => Event::Restart,
        "target_red" => Event::Red,
        "prefix_stale" => Event::Stale,
        _ => return None,
    };
    Some((e, w.detail.as_str()))
}

/// The event a transcript row says, its text, and whether it is a System row - or `None`.
pub(crate) fn of_row(it: &SnapshotItem) -> Option<(Event, String, bool)> {
    if let Some((text, _)) = crate::app::diff_popup::system_row_text(it) {
        return text
            .starts_with(RESEAT_SYSTEM)
            .then_some((Event::Reseat, text, true));
    }
    let text = crate::ui::transcript::user::agent_row_text(it)?;
    // One notice, or a restart and the deliveries it carries. A row holding other notices is never
    // absorbed - hiding it would hide them with it - but a restart's `[task]` groups are the
    // children its warning already counts, and left in they would draw a second line each.
    let groups = job_card::notice_groups(&text);
    let one_event = groups.len() == 1
        || (text.starts_with("[restart] ") && groups[1..].iter().all(|g| g.starts_with("[task] ")));
    if !one_event {
        return None;
    }
    let e = if text.starts_with("[reseat] ") {
        Event::Reseat
    } else if text.starts_with("[restart] ") {
        Event::Restart
    } else if text.starts_with("EMERGENCY — `") {
        Event::Red
    } else if stale_card::stale_of(&text).is_some() {
        Event::Stale
    } else {
        return None;
    };
    Some((e, text, false))
}

/// Whether a row's text and a warning's detail are about the same event - the same target and
/// sha for a red tip, the same session for a stale prompt. A reseat and a restart have nothing
/// to compare beyond their place, which [`WINDOW`] bounds.
fn same_event(e: Event, detail: &str, text: &str) -> bool {
    match e {
        Event::Red => match (red_card::warning_tip(detail), red_card::notice_tip(text)) {
            (Some(w), Some(n)) => w.target == n.target && w.sha.starts_with(&n.sha),
            _ => false,
        },
        Event::Stale => match (stale_card::stale_of(detail), stale_card::stale_of(text)) {
            (Some(w), Some(n)) => w.session == n.session,
            _ => false,
        },
        Event::Reseat | Event::Restart => true,
    }
}

impl App {
    /// Pair every warning of the four with the session rows that say the same event, once, after
    /// every frame. A new pairing re-walks from the earlier of the row and the warning's seam, so
    /// a row drawn before its warning goes and a warning drawn first gets the row's text.
    pub(crate) fn pair_one_event(&mut self) {
        let mut found: Vec<(usize, String, usize)> = Vec::new();
        for (place, n) in &self.notes {
            let Placed::Seam(at) = place else {
                continue;
            };
            let Some((e, detail)) = of_note(n) else {
                continue;
            };
            let key = note_key(n);
            let lo = at.saturating_sub(WINDOW);
            let hi = (at + WINDOW).min(self.items.len());
            for r in lo..hi {
                let id = self.items[r].item_id.as_str();
                if self.absorbed.contains_key(id) || found.iter().any(|(f, _, _)| *f == r) {
                    continue;
                }
                let Some((re, text, system)) = of_row(&self.items[r]) else {
                    continue;
                };
                if re != e || !same_event(e, detail, &text) {
                    continue;
                }
                // One row of each kind per warning: a second `[restart]` notice nearby is a
                // second restart, not this one.
                let taken = self
                    .absorbed
                    .iter()
                    .filter(|(_, v)| *v == &key)
                    .filter_map(|(id, _)| self.items.iter().find(|r| &r.item_id == id))
                    .chain(
                        found
                            .iter()
                            .filter(|(_, v, _)| *v == key)
                            .filter_map(|(k, _, _)| self.items.get(*k)),
                    )
                    .any(|it| of_row(it).is_some_and(|(_, _, s)| s == system));
                if !taken {
                    found.push((r, key.clone(), *at));
                }
            }
        }
        if found.is_empty() {
            return;
        }
        let from = found
            .iter()
            .map(|(r, _, at)| (*r).min(at.saturating_sub(1)))
            .min()
            .unwrap_or(0);
        for (r, key, _) in found {
            let id = self.items[r].item_id.clone();
            self.absorbed.insert(id, key);
        }
        self.invalidate_history_from(from);
    }
}

/// Whether row `item_id` is drawn as nothing because a warning's card stands for it: its card is on
/// the screen ([`Placed::Seam`]) or the reader retired it - never a note merely held, or the row,
/// the only witness left, would go. Asked of the key set first: a note may be evicted, the pairing not.
pub(crate) fn is_absorbed(
    absorbed: &std::collections::HashMap<String, String>,
    notes: &[(Placed, Note)],
    dismissed: &[String],
    marks: &[(String, u64)],
    item_id: &str,
) -> bool {
    absorbed.get(item_id).is_some_and(|key| {
        dismissed.contains(key)
            || notes.iter().any(|(place, n)| {
                note_key(n) == *key
                    && (matches!(place, Placed::Seam(_)) || note_retired(dismissed, marks, n))
            })
    })
}

/// The texts of the rows a note's card stands for, in transcript order - appended to its
/// popup under the warning's own text.
pub(crate) fn absorbed_texts(
    absorbed: &std::collections::HashMap<String, String>,
    items: &[SnapshotItem],
    key: &str,
) -> Vec<String> {
    let mut rows: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| absorbed.get(&it.item_id).is_some_and(|k| k == key))
        .filter_map(|(at, it)| of_row(it).map(|(_, t, _)| (at, t)))
        .collect();
    rows.sort_by_key(|(at, _)| *at);
    rows.into_iter().map(|(_, t)| t).collect()
}

/// A card's popup: its own text and the rows it stands for - the event said once. A restart's
/// warning and the notice it absorbed share a header, so when the row opens with the warning's own
/// first line the warning's text is that row's summary and the popup is the rows alone.
pub(crate) fn card_popup_text(own: &str, absorbed: &[String]) -> String {
    let head = own.lines().next().unwrap_or("");
    let repeated = !head.is_empty() && absorbed.iter().any(|t| t.lines().next() == Some(head));
    let mut text = if repeated {
        String::new()
    } else {
        own.to_string()
    };
    for t in absorbed {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(t);
    }
    text
}
