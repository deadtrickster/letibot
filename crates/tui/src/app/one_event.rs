//! **One event, one row** (R89's last finding) — a reseat, a restart, a red target and a stale
//! prompt each reached the screen twice or three times: the hub's warning as a card, and the
//! session's own notice (and, for a reseat, the System row) as more lines saying the same thing.
//!
//! The head keeps the warning's card — it is the one every head hears, live or from a snapshot
//! — and draws nothing for the session rows that say the same event: they are *absorbed* into
//! it, and the card's popup carries their text under its own. Nothing is dropped: the rows are
//! still in the transcript (the model read them), still in `/notes`' source, and one click away.
//!
//! **What pairs**, and only within [`WINDOW`] rows of the warning's seam, because the daemon
//! writes the two halves of one event together:
//!
//! | warning | session rows |
//! |---|---|
//! | `reseated` | the `[reseat] …` notice and the System row `This conversation was re-seated…` |
//! | `restart_restored`, `restart_reminded` | the `[restart] …` notice |
//! | `target_red` | the `EMERGENCY — …` notice for the same target and sha |
//! | `prefix_stale` | the stale-prompt sentence for the same session |
//!
//! A row holding several notices (a todo check with the stale paragraph under it) is never
//! absorbed — hiding it would hide the others with it. A note the reader retired gives its rows
//! back.

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

/// The event a transcript row says, its text, and whether it is a System row — or `None`.
pub(crate) fn of_row(it: &SnapshotItem) -> Option<(Event, String, bool)> {
    if let Some((text, _)) = crate::app::diff_popup::system_row_text(it) {
        return text
            .starts_with(RESEAT_SYSTEM)
            .then_some((Event::Reseat, text, true));
    }
    let text = crate::ui::transcript::user::agent_row_text(it)?;
    if job_card::notice_groups(&text).len() != 1 {
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

/// Whether a row's text and a warning's detail are about the same event — the same target and
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
    /// **Pair every warning of the four with the session rows that say the same event**, once —
    /// called after every frame, cheap (at most 64 notes, [`WINDOW`] rows each). A new pairing
    /// re-walks the history from the earlier of the row and the warning's seam, so a row drawn
    /// before its warning arrived goes, and a warning drawn before its row arrived gets the
    /// row's text in its popup.
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
            if self.dismissed.contains(&key) {
                continue;
            }
            let lo = at.saturating_sub(WINDOW);
            let hi = (at + WINDOW).min(self.items.len());
            for r in lo..hi {
                if self.absorbed.contains_key(&r) || found.iter().any(|(f, _, _)| *f == r) {
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
                    .map(|(k, v)| (*k, v))
                    .chain(found.iter().map(|(k, v, _)| (*k, v)))
                    .any(|(k, v)| {
                        *v == key
                            && self
                                .items
                                .get(k)
                                .and_then(of_row)
                                .is_some_and(|(_, _, s)| s == system)
                    });
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
            self.absorbed.insert(r, key);
        }
        self.invalidate_history_from(from);
        self.redraw = true;
    }
}

/// **Whether row `k` is drawn as nothing because a warning's card stands for it** — its note
/// still held and not retired.
pub(crate) fn is_absorbed(
    absorbed: &std::collections::HashMap<usize, String>,
    notes: &[(Placed, Note)],
    dismissed: &[String],
    k: usize,
) -> bool {
    absorbed.get(&k).is_some_and(|key| {
        !dismissed.contains(key) && notes.iter().any(|(_, n)| note_key(n) == *key)
    })
}

/// **The texts of the rows a note's card stands for**, in transcript order — appended to its
/// popup under the warning's own text.
pub(crate) fn absorbed_texts(
    absorbed: &std::collections::HashMap<usize, String>,
    items: &[SnapshotItem],
    key: &str,
) -> Vec<String> {
    let mut rows: Vec<usize> = absorbed
        .iter()
        .filter(|(_, v)| v.as_str() == key)
        .map(|(k, _)| *k)
        .collect();
    rows.sort_unstable();
    rows.into_iter()
        .filter_map(|r| items.get(r).and_then(of_row).map(|(_, t, _)| t))
        .collect()
}
