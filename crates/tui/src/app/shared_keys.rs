//! rano's shared key table (`rano::agent::keys::SHARED`) as this head spells it: one source for
//! the editor's hint row and `/help`, so neither can promise a chord the other does not bind.

use rano::agent::keys::{SHARED, Shared};
use rano::keymap::{Notation, parse_seq, seq_in};

/// The hint row is over capacity by construction and keeps its head, so its rows are ranked:
/// the crossing out of the pane, the way out and the interrupt, the asks, help, then the rest in
/// the table's order. A row the head has no rank for keeps its place at the end.
const RANK: &[&str] = &[
    "host-focus",
    "deactivate-mark",
    "host-interrupt",
    "ask",
    "send-position",
    "help",
    "next-diagnostic",
];

/// A row's chord in the `words` spelling every screen here uses: `ctrl-]`, `alt-s`, `esc esc`.
/// The table's own spelling is rano's bind notation, which is a different alphabet.
pub(crate) fn chord(row: &Shared) -> String {
    match parse_seq(row.key) {
        Some(seq) => seq_in(Notation::Words, &seq),
        // A spelling rano's own parser does not take. Shown as the table wrote it, so a bad row
        // reads as odd here instead of vanishing from every screen.
        None => row.key.to_lowercase(),
    }
}

/// Every row as `/help` shows it: the chord, and the table's whole sentence.
pub(crate) fn rows() -> Vec<(String, String)> {
    SHARED
        .iter()
        .map(|r| (chord(r), r.hint.to_string()))
        .collect()
}

/// The hint row's tail: the same rows, ranked, each as much of its hint as a bar can hold.
pub(crate) fn bar_tail() -> String {
    let mut ranked: Vec<(usize, &Shared)> = SHARED
        .iter()
        .map(|r| {
            let at = RANK
                .iter()
                .position(|a| *a == r.action)
                .unwrap_or(usize::MAX);
            (at, r)
        })
        .collect();
    // Stable, so the rows a rank does not name stay in the table's order.
    ranked.sort_by_key(|(at, _)| *at);
    ranked
        .iter()
        .map(|(_, r)| format!("{} {}", chord(r), words(r.hint)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A hint as the bar says it: its first clause. The table's hint is written for a help page,
/// which wraps; the bar is cut to the width and would spend it all on the first row.
fn words(hint: &str) -> &str {
    hint.split([';', ',']).next().unwrap_or(hint).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row the table carries reaches both screens, and its chord is spelled the way a
    /// person's terminal spells it rather than the way rano binds it.
    #[test]
    fn every_shared_row_is_spelled_in_words_and_reaches_both_screens() {
        for row in SHARED {
            let key = chord(row);
            assert!(
                !key.contains("C-") && !key.contains("M-") && !key.starts_with('<'),
                "{} is not the words spelling of {}",
                key,
                row.key
            );
            assert!(
                rows().iter().any(|(k, h)| *k == key && h == row.hint),
                "{key} is missing from the help's rows"
            );
            assert!(
                bar_tail().contains(&format!("{key} ")),
                "{key} is missing from the hint row"
            );
        }
    }

    /// The bar keeps its head, so the crossing out of the pane is the first thing on it: it is
    /// the most frequent act there, and the one a reader in the pane is looking for.
    #[test]
    fn the_bar_leads_with_the_crossing() {
        let tail = bar_tail();
        assert!(
            tail.starts_with("ctrl-] cross focus"),
            "the bar leads with {tail}"
        );
    }
}
