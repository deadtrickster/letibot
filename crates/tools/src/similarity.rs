//! **ONE similarity score, with three uses** — lexical, local, deterministic.
//!
//! The operator's framing, across the night: *"since we ride similarity score -
//! worth having it as a gate for new notes"*, *"add this new-note similarity
//! check. add it for 'just before edit made durable' too. we dont want to endup
//! with n almost identical notes by edits anyway"*, and *"we can do a simple
//! similarity match over corpus - like having a vector db over notes"*. This
//! module is that one arithmetic; its callers are
//!
//! 1. **the gate on writing a note** — [`Corpus::nearest`], asked of a note that
//!    does not exist yet, in `crate::builtins::notes`'s write path;
//! 2. **the same gate on an edit made durable** — the identical question, asked
//!    of the text an `append` or a `replace` would leave on disk, with the note
//!    being edited excluded from its own corpus. Same score, two triggers;
//! 3. **the read hint's score** — [`Corpus::best`], over the person's prompt and
//!    the turn's tool results (`letibot_harnessd::notes_offer`), whose floor is
//!    tuned from the take-up log rather than from taste.
//!
//! # What is compared: a note's index fields, and the subject, and nothing else
//!
//! The document side is a note's **title, abstract and headings** — the same
//! three fields the injected index leads with, so the thing that is scored is the
//! thing a reader is shown. [`index_text`] builds it, and it takes the abstract
//! from [`crate::builtins::notes::abstract_of`], the reader's own derivation, so
//! a note cannot be scored on one abstract and indexed under another.
//!
//! The comparison **subject** is the person's prompt and the turn's tool results,
//! never the whole conversation — the design's own rule, and the one that decides
//! whether a hint fires on what is happening now or on everything that ever
//! happened.
//!
//! # The arithmetic, which is one function
//!
//! [`similarity`] is cosine over TF-IDF weights:
//!
//! ```text
//! similarity(a, b) = Σ_t w_a(t)·w_b(t) / (‖a‖·‖b‖)
//! w_x(t)           = (1 + ln tf_x(t)) · idf(t)
//! idf(t)           = ln(N / df(t))
//! ```
//!
//! **Cosine and not a containment ratio, and that was measured rather than
//! chosen.** The first version of this module scored `Σ min(w_q, w_d) / Σ w_d` —
//! *how much of the note the subject accounts for* — which reads well and ranked
//! badly: dividing by the note's own weight punishes a note for having more
//! headings, so on this box's corpus a 60-word prompt about the offer design
//! ranked `promote-flag-ownership.md` above the note that is actually about it,
//! and the same happened for the notes budget. Cosine normalises both sides, and
//! with it the right note is first in every one of the cases measured (see the
//! floors below). The `min` survives only in the intuition it was reaching for:
//! sublinear `tf` is what stops a subject that repeats a note's word fifty times
//! from counting fifty times.
//!
//! **`ln(N/df)` and not `ln(1 + N/df)`, because the difference is whether a word
//! every note uses counts at all.** The additive form prices a term in every
//! document at `ln 2` — small, but never nothing — and that is enough to carry a
//! hint on its own once the corpus is thin: MEASURED, a workspace holding a
//! single note scored an unrelated prompt (sourdough, flour, water) at **0.257**
//! against that note, over any floor low enough to be useful, on the strength of
//! `the`, `and` and `for`. `ln(N/df)` is exactly zero for a term in every note, so
//! a word that distinguishes nothing contributes nothing, and it is derived from
//! the corpus rather than from a hand-kept stopword list. Two consequences, both
//! of them the safe direction:
//!
//! * a term in every note of the corpus is priced at nothing — which is what *in
//!   every note* means;
//! * **a corpus of ONE note has no `idf` at all**, so every score is zero and
//!   nothing is ever offered. That is the honest reading of *similar to what?* —
//!   there is nothing to be similar to — and it is *silent when unsure* taken at
//!   its word. A workspace gains offers as it gains notes: measured, a two-note
//!   corpus already scores 0.526 for a prompt about one of them and 0.000 for a
//!   prompt about sourdough.
//!
//! **The same limit runs the other way through the write gate**, and it is worth
//! naming because it is the unsafe direction there: with one note in the corpus
//! the gate scores everything at zero and refuses nothing. A gate that cannot
//! discriminate admits. It is the same arithmetic and the same reason, and the
//! alternative — a floor that fires on no evidence — would refuse the second note
//! anybody ever writes.
//!
//! **Symmetric, which is what makes it one score rather than two.** The duplicate
//! gate asks `similarity(a, b)` of the two notes and the hint asks
//! `similarity(subject, note)` — the same function, no second direction, and no
//! dependence on which note was written first.
//!
//! **The score is over a CURATED corpus**, which is what makes a lexical tier
//! respectable rather than a compromise: a note exists because a model decided
//! the thing was worth keeping, so similarity over notes is similarity over value
//! already judged.
//!
//! # The measured floors, and the honest gap between them
//!
//! MEASURED 2026-10-12 against this box's own corpus — the operator's 23
//! standing notes under `/home/dead/Projects/letibot/.letibot/notes/`, read once
//! by a scratch test that was deleted before the commit. Both numbers below are
//! `similarity`, and both are restated in the tests that hold the gate and the
//! hint, so a change to this arithmetic fails a test rather than moving a floor
//! quietly.
//!
//! * **The duplicate gate** (`crate::builtins::notes::DUPLICATE_FLOOR`, 0.30):
//!   the closest HONEST pair in the corpus is **0.183**
//!   (`a-worktree-note-breaks-the-reingest-test` ~
//!   `gate-target-dir-and-a-resume-fixture`; the next two are 0.139 and 0.136).
//!   The weakest near-copy — a note's whole body under a new name with its
//!   abstract reworded — is **0.363**; the same trick on the compaction note is
//!   0.563, half a note's body under a new name is 0.810, and a note copied
//!   verbatim under a new name is 0.957. 0.30 sits in the gap, and it errs toward
//!   refusing: a refusal names the note that was too close and costs one round,
//!   while a corpus of near-duplicates costs every reader of the index.
//! * **The hint** (`letibot_harnessd::notes_offer::OFFER_FLOOR`, 0.14): across ten
//!   prompts drawn from work that has nothing to do with this corpus the best
//!   score any note reached was **0.133**, and across four prompts that are
//!   squarely about a note the scores were 0.181, 0.126, 0.419 and 0.366. 0.14 is
//!   above every negative measured and below three of the four positives.
//!
//! **And the gap is the finding, not a footnote.** The fourth positive (0.126, a
//! paraphrase of the review-queue note) is *below* the worst negative (0.133, a
//! docker build prompt reaching `resume-the-child-nag-clock`). At this tier a
//! floor cannot separate those two, and no choice of floor does. That is the
//! measurement the second tier — embeddings, better recall on a paraphrase — is
//! owed, and it is written down here rather than discovered later.
//!
//! **A thin corpus, measured because it is where the first version was wrong.**
//! With `ln(N/df)` a workspace holding one note scores 0.000 for everything and
//! offers nothing; with two notes, a prompt about the note scores **0.526** and a
//! prompt about sourdough scores **0.000**. The additive `ln(1 + N/df)` this
//! replaced gave 0.257 for the sourdough prompt against a one-note corpus — over
//! any useful floor — which is the misfire that made the change.
//!
//! # What this deliberately does not do
//!
//! * **No stopword list.** A hand-maintained list is a list that drifts from the
//!   corpus it describes, and `ln(N/df)` already prices a word every note uses at
//!   exactly nothing. What is left is the floor above.
//! * **No stemming and no synonyms.** `budget` and `budgets` are two terms. A
//!   stemmer is a model of English this corpus does not need yet; the same
//!   sentence as the tiers above applies.
//! * **Terms shorter than [`MIN_TERM`] are dropped.** Three characters, and the
//!   trade is stated rather than hidden: two-letter terms in this corpus are
//!   overwhelmingly English function words, and keeping them would raise the
//!   baseline under every score; the cost is that `id`, `vm`, `ui` and `db` are
//!   not terms. They are rare in an abstract and a heading, which is where this
//!   reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::builtins::notes::abstract_of;

/// The shortest term this scores on. See the module doc for the trade.
pub const MIN_TERM: usize = 3;

/// One text as a weighted bag of terms, against a corpus's `idf`.
///
/// Not a `HashMap`: iteration order over a score's terms is the difference
/// between a deterministic answer and one that changes with the allocator, and a
/// gate whose verdict flickers is worse than no gate.
#[derive(Debug, Clone, Default)]
pub struct Weights {
    terms: BTreeMap<String, f64>,
    /// `Σ w(t)²` — kept rather than re-added, because it is the denominator of
    /// every score this text takes part in.
    squared: f64,
}

impl Weights {
    /// The weight of one term, `0.0` when the text does not carry it.
    pub fn weight(&self, term: &str) -> f64 {
        self.terms.get(term).copied().unwrap_or(0.0)
    }

    /// The terms this text is scored on, in a stable order — for a report that
    /// has to name why two notes matched.
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

/// **The one function: how alike two texts are**, `0.0` to `1.0`.
///
/// `1.0` only for two texts with the same terms in the same proportions. A text
/// with no terms scores `0.0` rather than `NaN`: nothing is not similar to
/// anything, and a score that cannot be compared is a score a floor cannot be
/// applied to.
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

/// **The corpus, with the `idf` that makes a common word cheap.**
///
/// Built from the same list the injected section is assembled from — the reader's
/// `gather` — so the notes a score is computed over are exactly the notes a model
/// can be told about. A note the reader would not inject is not in the corpus,
/// and a score is never about something unreachable.
pub struct Corpus {
    notes: Vec<Note>,
    idf: BTreeMap<String, f64>,
    /// What an unseen term weighs: as much as a term exactly one note uses, which
    /// is what a term in the note being written and in no other actually is. A
    /// fallback of zero would make a brand-new note score zero against everything
    /// — the gate would pass everything it should and nothing it should not.
    unseen: f64,
}

impl Corpus {
    /// Build from `(path, text)` pairs, in whatever order they arrive.
    pub fn new(files: &[(PathBuf, String)]) -> Corpus {
        let n = files.len().max(1) as f64;
        let mut counts: Vec<(PathBuf, BTreeMap<String, f64>)> = Vec::with_capacity(files.len());
        let mut df: BTreeMap<String, f64> = BTreeMap::new();
        for (path, text) in files {
            let mut tf: BTreeMap<String, f64> = BTreeMap::new();
            for term in terms(&index_text(path, text)) {
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

    /// One text as the corpus would weigh it — the candidate note, or the subject.
    pub fn weigh(&self, text: &str) -> Weights {
        let mut tf: BTreeMap<String, f64> = BTreeMap::new();
        for term in terms(text) {
            *tf.entry(term).or_insert(0.0) += 1.0;
        }
        weigh(&tf, &self.idf, self.unseen)
    }

    /// The note this text is most alike — the read hint's question. `None` only
    /// when the corpus is empty, or when nothing shares a single term with it.
    ///
    /// Ties break by path, so two notes that score the same offer the same one
    /// every time.
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

    /// **The note a candidate would be a near-copy of** — the write gate's
    /// question, asked of a note that is not in the corpus yet.
    ///
    /// `skip` is the note being edited, excluded from its own corpus: an `append`
    /// must not be refused for being similar to the note it is growing.
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
        // Sublinear, because a word said four times in one abstract is a word
        // said once: without it a note that repeats itself would be scored as a
        // note about less.
        let w = (1.0 + count.ln()) * idf.get(term).copied().unwrap_or(unseen);
        // A term the whole corpus uses weighs nothing and is dropped rather than
        // carried as a zero: a weight of zero adds nothing to a dot product and
        // nothing to a norm, and leaving it in would make `terms()` — the report a
        // reader sees — name words that did not count.
        if w <= 0.0 {
            continue;
        }
        terms.insert(term.clone(), w);
        squared += w * w;
    }
    Weights { terms, squared }
}

/// **The text a note is scored on: its title, its abstract and its headings.**
///
/// The same three fields the injected index leads with, so the score is about
/// what a reader is shown rather than about a note's whole body — a note that
/// mentions a thing once in passing is not a note about it, and the abstract is
/// where an author says what it is about.
///
/// The title is the file stem with its separators opened out, so
/// `notes-that-offer-themselves.md` contributes `notes`, `that`, `offer` and
/// `themselves` — the name an author chose is a summary they had to write.
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
/// Digits are terms, and so are words with digits in them (`gpt4`, `2k`), because
/// this corpus's notes are full of them and they are exactly the terms nothing
/// else shares.
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TERM)
        .map(|t| t.to_lowercase())
        .collect()
}
