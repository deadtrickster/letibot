//! Whether a session has said a document's own words. A cosine finds neighbours of whatever the
//! session is doing, so an offer also needs the document's identity - its name and keywords - to
//! appear in what the operator wrote or the files the session touched.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

use super::terms;

/// The 20,000 most frequent English words (wordfreq 3.1.1, CC BY-SA 4.0; see the file's header).
/// A small registry ranks plain English as rare, so a description word on this list is no evidence.
const ENGLISH: &str = include_str!("english-20k.txt");

/// A description word that can name a document: not a number and not common English.
fn is_distinctive(word: &str) -> bool {
    word.chars().any(char::is_alphabetic) && !is_common(word)
}

/// Whether `word`, or its stem, is a common English word.
pub fn is_common(word: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let set = SET.get_or_init(|| {
        ENGLISH
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    });
    let word = word.to_lowercase();
    set.contains(word.as_str()) || set.contains(stem(&word).as_str())
}

/// The words a session has used, as [`terms`] cuts them.
#[derive(Debug, Clone, Default)]
pub struct Seen(BTreeSet<String>);

impl Seen {
    pub fn of_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Seen {
        Seen(
            texts
                .into_iter()
                .flat_map(terms)
                .map(|t| stem(&t))
                .collect(),
        )
    }

    /// The seen word that is `term` or one of its inflections: `fuzz` for `fuzzer`, `pane` for
    /// `panes`. A shared stem of four letters, so `fix` matches only `fix`.
    fn says(&self, term: &str) -> bool {
        self.0.contains(&stem(term))
    }
}

/// A word without its inflection: `fuzzing` and `fuzzer` are `fuzz`, `panes` is `pane`. Light on
/// purpose - a stem shorter than four letters is not cut, so `fixes` never becomes `fix`.
fn stem(word: &str) -> String {
    for suffix in ["ings", "ing", "ers", "er", "ed", "es", "s"] {
        if let Some(root) = word.strip_suffix(suffix)
            && root.chars().count() >= 4
        {
            return root.to_string();
        }
    }
    word.to_string()
}

/// Each document's identity terms, weighted by how rare they are across the whole corpus.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    docs: BTreeMap<String, Vec<(String, f64)>>,
    floor: f64,
}

impl Evidence {
    /// `docs` is `(key, identity, full text)`. Rarity is counted over the full texts, so a name word
    /// every description uses weighs little. `ln(1 + N/df)` keeps a one-document corpus nonzero.
    pub fn new<'a>(docs: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Evidence {
        Evidence::described(docs.into_iter().map(|(k, i, f)| (k, i, "", f)))
    }

    /// As [`Evidence::new`], with `(key, identity, description, full text)`: the description's
    /// words that are not common English also name the document.
    pub fn described<'a>(
        docs: impl IntoIterator<Item = (&'a str, &'a str, &'a str, &'a str)>,
    ) -> Evidence {
        let docs: Vec<(&str, &str, &str, &str)> = docs.into_iter().collect();
        let n = docs.len().max(1) as f64;
        let mut df: BTreeMap<String, usize> = BTreeMap::new();
        for (_, _, _, full) in &docs {
            let mine: BTreeSet<String> = terms(full).into_iter().collect();
            for t in mine {
                *df.entry(t).or_default() += 1;
            }
        }
        let weight = |t: &str| (1.0 + n / df.get(t).copied().unwrap_or(1).max(1) as f64).ln();
        let docs = docs
            .iter()
            .map(|(key, identity, description, _)| {
                let mut mine: BTreeSet<String> = terms(identity).into_iter().collect();
                mine.extend(terms(description).into_iter().filter(|t| is_distinctive(t)));
                let weighted = mine.into_iter().map(|t| (t.clone(), weight(&t))).collect();
                (key.to_string(), weighted)
            })
            .collect();
        // One word as rare as a word four documents share is enough; a common one is not.
        let floor = (1.0 + n / 4.0).ln();
        Evidence { docs, floor }
    }

    /// The identity terms the session said, and their summed weight.
    pub fn hits(&self, key: &str, seen: &Seen) -> (f64, Vec<String>) {
        let Some(terms) = self.docs.get(key) else {
            return (0.0, Vec::new());
        };
        let mut sum = 0.0;
        let mut hit = Vec::new();
        for (t, w) in terms {
            if seen.says(t) {
                sum += w;
                hit.push(t.clone());
            }
        }
        (sum, hit)
    }

    /// The hits, when they weigh enough to count as the session having named this document.
    pub fn found(&self, key: &str, seen: &Seen) -> Option<Vec<String>> {
        let (sum, hit) = self.hits(key, seen);
        (sum >= self.floor).then_some(hit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Evidence {
        Evidence::new([
            (
                "fuzz-harness-writing",
                "fuzz-harness-writing",
                "fuzz-harness-writing how to write an in-process fuzz harness, and fix it",
            ),
            (
                "fuzzing-method",
                "fuzzing-method",
                "fuzzing-method how to fuzz properly and prove a fix, harness bugs versus the system",
            ),
            (
                "pane-screenshot",
                "pane-screenshot tmux tui",
                "pane-screenshot render a tmux pane to a png to see colours and fix them",
            ),
            (
                "fix-report",
                "fix-report",
                "fix-report write a fix report for a bug fix and compile them",
            ),
            (
                "plugin-pane",
                "plugin-pane",
                "plugin-pane a pane beside the chat, a fix for a missing view",
            ),
        ])
    }

    /// The rano session: the operator asked about scrolling and colours in a terminal pane and
    /// the session edited its editor; no fuzzing word anywhere.
    #[test]
    fn a_session_that_never_named_fuzzing_has_no_evidence_for_it() {
        let seen = Seen::of_texts([
            "no scroll and no colors, please add",
            "well my touchpad still doesnt scroll",
            "please fix things",
            "/home/op/Projects/rano/rano/src/editor.rs",
        ]);
        let ev = registry();
        assert_eq!(ev.found("fuzz-harness-writing", &seen), None);
        assert_eq!(ev.found("fuzzing-method", &seen), None);
        // `fix` is in every document: a common word does not name `fix-report`.
        assert_eq!(ev.found("fix-report", &seen), None);
    }

    /// An inflection is the same word: `fuzzer` names the fuzzing skills, `panes` the pane one.
    #[test]
    fn a_stem_is_evidence() {
        let ev = registry();
        let seen = Seen::of_texts(["the fuzzer found a crash in two panes"]);
        assert!(ev.found("fuzzing-method", &seen).is_some());
        assert!(ev.found("fuzz-harness-writing", &seen).is_some());
        assert_eq!(
            ev.found(
                "pane-screenshot",
                &Seen::of_texts(["capture the tmux panes"])
            ),
            Some(vec!["pane".to_string(), "tmux".to_string()])
        );
    }

    /// The measured failure: a 26-skill registry made `work`, `caller` and `protocol` rare, so
    /// every unrelated skill whose description used one was found. A distinctive word still is.
    #[test]
    fn a_description_word_is_evidence_only_when_it_is_not_common_english() {
        let mut docs = vec![
            (
                "conformity-method".to_string(),
                "conformity-method".to_string(),
                "how a caller measures whether a protocol works like its reference".to_string(),
            ),
            (
                "rust-builds".to_string(),
                "rust-builds".to_string(),
                "the shared sccache for agents' work, and target dirs on one disk".to_string(),
            ),
        ];
        for i in 0..24 {
            docs.push((
                format!("other-{i}"),
                format!("other-{i}"),
                format!("filler {i}"),
            ));
        }
        let ev = Evidence::described(
            docs.iter()
                .map(|(k, i, d)| (k.as_str(), i.as_str(), d.as_str(), d.as_str())),
        );
        let plain = Seen::of_texts(["the caller does the work over the protocol"]);
        assert_eq!(ev.found("conformity-method", &plain), None);
        assert_eq!(ev.found("rust-builds", &plain), None);
        assert_eq!(
            ev.found("rust-builds", &Seen::of_texts(["is sccache warm"])),
            Some(vec!["sccache".to_string()])
        );
        for word in ["work", "caller", "protocol", "callers", "works"] {
            assert!(is_common(word), "{word}");
        }
        assert!(!is_common("sccache"));
        assert!(!is_distinctive("2026"));
    }

    #[test]
    fn a_stem_keeps_four_letters() {
        assert_eq!(stem("fuzzer"), stem("fuzzing"));
        assert_eq!(stem("fuzz"), stem("fuzzer"));
        assert_ne!(stem("fix"), stem("fixture"));
        assert_ne!(stem("report"), stem("repository"));
    }
}
