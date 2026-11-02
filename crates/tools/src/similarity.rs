//! ONE similarity score, with four uses - lexical, local, deterministic.
//!
//! ```text
//! similarity(a, b) = Σ_t w_a(t)·w_b(t) / (‖a‖·‖b‖)
//! w_x(t)           = (1 + ln tf_x(t)) · idf(t)
//! idf(t)           = ln(N / df(t))
//! ```
//!
//! The document side is a note's title, abstract and headings ([`index_text`]), the same
//! three fields the injected index leads with, so what is scored is what a reader is shown;
//! the subject is the person's prompt and the turn's tool results, never the whole
//! conversation, which is what decides whether a hint fires on what is happening now.
//!
//! Cosine and not a containment ratio, because dividing by the note's own weight punishes
//! it for having more headings. `ln(N/df)` and not `ln(1 + N/df)`, because a term every note
//! uses must count at exactly nothing - derived from the corpus rather than from a stopword
//! list - which leaves a one-note corpus scoring everything at zero, the honest reading of
//! "similar to what". The same limit runs the other way through the write gate, where a
//! gate that cannot discriminate admits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::builtins::notes::abstract_of;

/// The shortest term this scores on. Two-letter terms here are overwhelmingly English
/// function words and keeping them would raise the baseline under every score; the cost
/// is that `id`, `vm`, `ui` and `db` are not terms.
pub const MIN_TERM: usize = 3;

/// One text as a weighted bag of terms, against a corpus's `idf`.
///
/// Not a `HashMap`: iteration order over a score's terms is the difference between a
/// deterministic answer and one that changes with the allocator, and a gate whose verdict
/// flickers is worse than no gate.
#[derive(Debug, Clone, Default)]
pub struct Weights {
    terms: BTreeMap<String, f64>,
    /// The sum of the squared weights - kept rather than re-added, because it is the
    /// denominator of every score this text takes part in.
    squared: f64,
}

impl Weights {
    /// The weight of one term, `0.0` when the text does not carry it.
    pub fn weight(&self, term: &str) -> f64 {
        self.terms.get(term).copied().unwrap_or(0.0)
    }

    /// The terms this text is scored on, in a stable order - for a report that has to
    /// name why two notes matched.
    pub fn terms(&self) -> impl Iterator<Item = (&str, f64)> {
        self.terms.iter().map(|(t, w)| (t.as_str(), *w))
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The euclidean norm, which is what normalises a score against the length of
    /// the text it came from.
    pub fn norm(&self) -> f64 {
        self.squared.sqrt()
    }
}

/// The one function: how alike two texts are, `0.0` to `1.0`.
///
/// `1.0` only for two texts with the same terms in the same proportions. A text with no
/// terms scores `0.0` rather than `NaN`: nothing is not similar to anything, and a score
/// that cannot be compared is a score a floor cannot be applied to.
pub fn similarity(a: &Weights, b: &Weights) -> f64 {
    let den = a.norm() * b.norm();
    if den <= 0.0 {
        return 0.0;
    }
    let dot: f64 = b.terms().map(|(t, w)| w * a.weight(t)).sum();
    (dot / den).clamp(0.0, 1.0)
}

/// A note, as the corpus holds it: where it is and what it weighs.
struct Note {
    path: PathBuf,
    weights: Weights,
}

/// The corpus, with the `idf` that makes a common word cheap.
///
/// Built from the same list the injected section is assembled from - the reader's
/// `gather` - so the notes a score is computed over are exactly the notes a model can be
/// told about, and a score is never about something unreachable.
pub struct Corpus {
    notes: Vec<Note>,
    idf: BTreeMap<String, f64>,
    /// What an unseen term weighs: as much as a term exactly one note uses, which is
    /// what a term in the note being written and in no other actually is. A fallback of
    /// zero would make a brand-new note score zero against everything, so the gate would
    /// pass everything it should and nothing it should not.
    unseen: f64,
}

impl Corpus {
    /// Build from `(path, text)` pairs, in whatever order they arrive.
    pub fn new(files: &[(PathBuf, String)]) -> Corpus {
        let indexed: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(path, text)| (path.clone(), index_text(path, text)))
            .collect();
        Corpus::of_texts(&indexed)
    }

    /// Build from texts that are scored whole - the same arithmetic as [`Corpus::new`],
    /// without [`index_text`] in front of it.
    ///
    /// A note is scored on its title, abstract and headings because that is what a reader
    /// is shown; a corpus that is not notes has none of the three, and running a sentence
    /// through `index_text` would score it on its first sentence plus whatever the label
    /// happened to be called. The label is still a path, so the callers can say which
    /// document matched.
    pub fn of_texts(files: &[(PathBuf, String)]) -> Corpus {
        let n = files.len().max(1) as f64;
        let mut counts: Vec<(PathBuf, BTreeMap<String, f64>)> = Vec::with_capacity(files.len());
        let mut df: BTreeMap<String, f64> = BTreeMap::new();
        for (path, text) in files {
            let mut tf: BTreeMap<String, f64> = BTreeMap::new();
            for term in terms(text) {
                *tf.entry(term).or_insert(0.0) += 1.0;
            }
            for term in tf.keys() {
                *df.entry(term.clone()).or_insert(0.0) += 1.0;
            }
            counts.push((path.clone(), tf));
        }
        let idf: BTreeMap<String, f64> =
            df.iter().map(|(t, d)| (t.clone(), (n / d).ln())).collect();
        // A term in NO note is as distinctive as a term in exactly one: `df = 1` is
        // the smallest count the corpus can express, and a note being written has
        // terms no other note has.
        let unseen = n.ln();
        let notes = counts
            .into_iter()
            .map(|(path, tf)| Note {
                path,
                weights: weigh(&tf, &idf, unseen),
            })
            .collect();
        Corpus { notes, idf, unseen }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// One text as the corpus would weigh it - the candidate note, or the subject.
    pub fn weigh(&self, text: &str) -> Weights {
        let mut tf: BTreeMap<String, f64> = BTreeMap::new();
        for term in terms(text) {
            *tf.entry(term).or_insert(0.0) += 1.0;
        }
        weigh(&tf, &self.idf, self.unseen)
    }

    /// The documents most like `text`, best first - [`Corpus::best`]'s arithmetic with the
    /// ranking kept instead of the winner: a caller that wants the top match's body and the
    /// next two names needs the order, not the maximum.
    pub fn ranked(&self, text: &str, most: usize) -> Vec<(&Path, f64)> {
        let q = self.weigh(text);
        let mut all: Vec<(&Path, f64)> = self
            .notes
            .iter()
            .map(|n| (n.path.as_path(), similarity(&q, &n.weights)))
            .filter(|(_, s)| *s > 0.0)
            .collect();
        all.sort_by(|(pa, sa), (pb, sb)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| pa.cmp(pb))
        });
        all.truncate(most);
        all
    }

    /// The note this text is most alike - the read hint's question. `None` only when the
    /// corpus is empty, or when nothing shares a single term with it.
    ///
    /// Ties break by path, so two notes that score the same offer the same one every time.
    pub fn best(&self, text: &str) -> Option<(&Path, f64)> {
        let q = self.weigh(text);
        self.notes
            .iter()
            .map(|n| (n.path.as_path(), similarity(&q, &n.weights)))
            .filter(|(_, s)| *s > 0.0)
            .max_by(|(pa, sa), (pb, sb)| {
                sa.partial_cmp(sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| pb.cmp(pa))
            })
    }

    /// The note a candidate would be a near-copy of - the write gate's question, asked of
    /// a note that is not in the corpus yet.
    ///
    /// `skip` is the note being edited, excluded from its own corpus: an `append` must not
    /// be refused for being similar to the note it is growing.
    pub fn nearest(&self, candidate: &str, skip: Option<&Path>) -> Option<(&Path, f64)> {
        let cand = self.weigh(candidate);
        self.notes
            .iter()
            .filter(|n| skip.is_none_or(|s| s != n.path))
            .map(|n| (n.path.as_path(), similarity(&cand, &n.weights)))
            .max_by(|(pa, sa), (pb, sb)| {
                sa.partial_cmp(sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| pb.cmp(pa))
            })
    }
}

fn weigh(tf: &BTreeMap<String, f64>, idf: &BTreeMap<String, f64>, unseen: f64) -> Weights {
    let mut terms: BTreeMap<String, f64> = BTreeMap::new();
    let mut squared = 0.0;
    for (term, count) in tf {
        // Sublinear, because a word said four times in one abstract is a word said once:
        // without it a note that repeats itself would be scored as a note about less.
        let w = (1.0 + count.ln()) * idf.get(term).copied().unwrap_or(unseen);
        // A term the whole corpus uses weighs nothing and is dropped rather than
        // carried as a zero: it adds nothing to a dot product or a norm, and leaving it
        // in would make `terms()` - the report a reader sees - name words that did not
        // count.
        if w <= 0.0 {
            continue;
        }
        terms.insert(term.clone(), w);
        squared += w * w;
    }
    Weights { terms, squared }
}

/// The text a note is scored on: its title, its abstract and its headings.
///
/// The same three fields the injected index leads with, so the score is about what a
/// reader is shown rather than about a whole body - a note that mentions a thing once in
/// passing is not a note about it, and the abstract is where an author says what it is
/// about. The title is the file stem with its separators opened out, because the name an
/// author chose is a summary they had to write.
pub fn index_text(path: &Path, text: &str) -> String {
    let mut out = String::new();
    if let Some(stem) = path.file_stem() {
        let named = stem.to_string_lossy().replace(['-', '_'], " ");
        out.push_str(&named);
        out.push('\n');
    }
    if let Some(abstract_) = abstract_of(text) {
        out.push_str(&abstract_.text);
        out.push('\n');
    }
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(heading) = trimmed.strip_prefix('#') {
            out.push_str(heading.trim_start_matches('#').trim());
            out.push('\n');
        }
    }
    out
}

/// The terms of a text: lowercased runs of letters and digits, at least
/// [`MIN_TERM`] long, in the order they appear.
///
/// Digits are terms, and so are words with digits in them (`gpt4`, `2k`), because they
/// are exactly the terms nothing else shares.
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TERM)
        .map(|t| t.to_lowercase())
        .collect()
}
