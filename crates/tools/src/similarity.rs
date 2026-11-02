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

use letibot_embeddings::Tier;

use crate::builtins::notes::abstract_of;

/// The shortest term this scores on. Two-letter terms here are overwhelmingly English
/// function words and keeping them would raise the baseline under every score; the cost
/// is that `id`, `vm`, `ui` and `db` are not terms.
pub const MIN_TERM: usize = 3;

/// One text as a weighted bag of terms, against a corpus's `idf`. Not a `HashMap`: iteration order over a
/// score's terms is the difference between a deterministic answer and one that changes with the allocator.
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

/// The one function: how alike two texts are, `0.0` to `1.0`, and `1.0` only for the same terms in the
/// same proportions. A text with no terms scores `0.0` rather than `NaN`, because a score that cannot be
/// compared is a score a floor cannot be applied to.
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

/// The corpus, with the `idf` that makes a common word cheap. Built from the same list the injected
/// section is assembled from, so a score is never about a note a model cannot be told about.
pub struct Corpus {
    notes: Vec<Note>,
    idf: BTreeMap<String, f64>,
    /// What an unseen term weighs: as much as a term exactly one note uses, which is what a term in the
    /// note being written and in no other is. A fallback of zero would make a brand-new note score zero
    /// against everything.
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

    /// Build from texts that are scored whole, without [`index_text`] in front of it. A note is scored on
    /// its title, abstract and headings because that is what a reader is shown, and a text that is not a
    /// note has none of the three.
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
        self.ranked_with(text, most, None)
    }

    /// [`Corpus::ranked`] with the second tier attached. `tier` is filled with this
    /// corpus's paths as keys, which is what makes a note's vector reachable by its path.
    pub fn ranked_with(&self, text: &str, most: usize, tier: Option<&Tier>) -> Vec<(&Path, f64)> {
        let mut all = self.scored_with(text, tier);
        all.retain(|(_, s)| *s > 0.0);
        all.sort_by(|(pa, sa), (pb, sb)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| pa.cmp(pb))
        });
        all.truncate(most);
        all
    }

    /// Every note's score against a text: the words, and the meaning when a tier is
    /// attached. The query is embedded once per call and each note's cosine read off the
    /// in-memory index, so a round costs one embedding and never one per note.
    fn scored_with(&self, text: &str, tier: Option<&Tier>) -> Vec<(&Path, f64)> {
        let q = self.weigh(text);
        let query = tier.and_then(|t| t.query(text, now_ms()).ok());
        self.notes
            .iter()
            .map(|n| {
                let lexical = similarity(&q, &n.weights);
                let semantic = match (&query, tier) {
                    (Some(query), Some(tier)) => tier.cosine(&n.path.to_string_lossy(), query),
                    _ => None,
                };
                (n.path.as_path(), blended(lexical, semantic))
            })
            .collect()
    }

    /// The note this text is most alike, and `None` only when the corpus is empty or nothing shares a
    /// single term. Ties break by path, so the same one is offered every time.
    pub fn best(&self, text: &str) -> Option<(&Path, f64)> {
        self.best_with(text, None)
    }

    /// [`Corpus::best`] with the second tier attached - the notes offer's door, once a
    /// caller has filled the tier with its own corpus.
    pub fn best_with(&self, text: &str, tier: Option<&Tier>) -> Option<(&Path, f64)> {
        self.scored_with(text, tier)
            .into_iter()
            .filter(|(_, s)| *s > 0.0)
            .max_by(|(pa, sa), (pb, sb)| {
                sa.partial_cmp(sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| pb.cmp(pa))
            })
    }

    /// The note a candidate would be a near-copy of. `skip` is the note being edited, excluded from its own
    /// corpus: an `append` must not be refused for being similar to the note it is growing.
    pub fn nearest(&self, candidate: &str, skip: Option<&Path>) -> Option<(&Path, f64)> {
        self.nearest_with(candidate, skip, None)
    }

    /// [`Corpus::nearest`] with the second tier attached - the duplicate gate's door, on
    /// both a note being written and an edit made durable.
    pub fn nearest_with(
        &self,
        candidate: &str,
        skip: Option<&Path>,
        tier: Option<&Tier>,
    ) -> Option<(&Path, f64)> {
        let scored = self.scored_with(candidate, tier);
        scored
            .into_iter()
            .filter(|(p, _)| skip.is_none_or(|s| s != *p))
            .max_by(|(pa, sa), (pb, sb)| {
                sa.partial_cmp(sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| pb.cmp(pa))
            })
    }
}

/// The box's clock, as the cache's `last_used` stamps it: a caller of a corpus never
/// names an instant, so this is the one place the tier is told the time.
fn now_ms() -> i64 {
    letibot_metrics::now_ms() as i64
}

fn weigh(tf: &BTreeMap<String, f64>, idf: &BTreeMap<String, f64>, unseen: f64) -> Weights {
    let mut terms: BTreeMap<String, f64> = BTreeMap::new();
    let mut squared = 0.0;
    for (term, count) in tf {
        // Sublinear, because a word said four times in one abstract is a word said once:
        // without it a note that repeats itself would be scored as a note about less.
        let w = (1.0 + count.ln()) * idf.get(term).copied().unwrap_or(unseen);
        // A term the whole corpus uses weighs nothing and is dropped rather than carried as a zero, since
        // leaving it in would make `terms()` name words that did not count.
        if w <= 0.0 {
            continue;
        }
        terms.insert(term.clone(), w);
        squared += w * w;
    }
    Weights { terms, squared }
}

/// How much of a blended score comes from meaning rather than from words. Half, because
/// a tier that outweighed the terms would offer a note nobody quoted, and one that
/// whispered would move no floor a caller had tuned.
pub const SEMANTIC_WEIGHT: f64 = 0.5;

/// **The one blend, for every caller.** A lexical score and, when the second tier
/// answered, the cosine of the two vectors - `None` when the mode is off, when the text
/// was never embedded, or when the tier could not answer, and then the lexical score
/// stands alone. What a caller adds is its own floor: offering a note and refusing a
/// duplicate want different thresholds and tune them from their own logs.
pub fn blended(lexical: f64, semantic: Option<f32>) -> f64 {
    let lexical = lexical.clamp(0.0, 1.0);
    match semantic {
        None => lexical,
        Some(s) => {
            let semantic = (s as f64).clamp(0.0, 1.0);
            ((1.0 - SEMANTIC_WEIGHT) * lexical + SEMANTIC_WEIGHT * semantic).clamp(0.0, 1.0)
        }
    }
}

/// The text a note is scored on: its title, its abstract and its headings, the same three fields the
/// injected index leads with. A note that mentions a thing once in passing is not a note about it, and the
/// title is the file stem with its separators opened out.
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

/// The terms of a text: lowercased runs of letters and digits, at least [`MIN_TERM`] long, in order. Digits
/// are terms, and so are words with digits in them, because they are exactly the terms nothing else shares.
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TERM)
        .map(|t| t.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_embeddings::testing as fixture;
    use letibot_embeddings::{Settings, Tier};

    /// A directory that removes itself.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "letibot-similarity-{tag}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Dir(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The second tier over the tiny model, filled with the corpus's documents - which is
    /// what a caller does once, when its notes change.
    fn tier(tag: &str, corpus: &[(PathBuf, String)]) -> (Dir, Dir, Tier) {
        let model = Dir::new(&format!("model-{tag}"));
        fixture::tiny_bert(model.path()).expect("fixture model");
        let data = Dir::new(&format!("data-{tag}"));
        let settings = Settings::from_toml(&format!(
            "[embeddings]\nmode = \"internal\"\nmodel = \"{}\"\n",
            model.path().display()
        ))
        .expect("settings");
        let mut tier = Tier::open(&settings, data.path())
            .expect("open")
            .expect("a tier");
        tier.fill(
            corpus
                .iter()
                .map(|(p, t)| (p.to_string_lossy().to_string(), index_text(p, t))),
            letibot_metrics::now_ms() as i64,
        )
        .expect("fill");
        (model, data, tier)
    }

    /// **Off is the lexical score, exactly.** A box that never wrote `[embeddings]` scores
    /// what it scored before the tier existed.
    #[test]
    fn mode_off_hands_back_the_lexical_score() {
        assert_eq!(blended(0.126, None), 0.126);
        let corpus = Corpus::new(&[
            (
                PathBuf::from("a-merge.md"),
                "# Branch\nmerging a branch\n".into(),
            ),
            (
                PathBuf::from("b-docker.md"),
                "# Docker\na container\n".into(),
            ),
        ]);
        assert_eq!(
            corpus.best("merging a branch"),
            corpus.best_with("merging a branch", None)
        );
    }

    /// **The case the entry was filed for.** Lexically a paraphrase scores 0.126 under an
    /// unrelated note's 0.133, so no floor separates them; with the second tier's cosine
    /// the paraphrase leads.
    #[test]
    fn the_tier_separates_what_the_words_cannot() {
        let paraphrase = blended(0.126, Some(0.88));
        let unrelated = blended(0.133, Some(0.16));
        assert!(
            paraphrase > unrelated,
            "the paraphrase scored {paraphrase} and the unrelated note {unrelated}"
        );
    }

    /// One arithmetic and no more: the ends are the ends, and a missing tier cannot move a
    /// score at all.
    #[test]
    fn the_blend_keeps_its_ends_and_ignores_an_absent_tier() {
        assert_eq!(blended(1.0, Some(1.0)), 1.0);
        assert_eq!(blended(0.0, Some(0.0)), 0.0);
        assert_eq!(blended(0.4, None), 0.4);
        assert!((blended(0.4, Some(1.0)) - (0.5 * 0.4 + 0.5)).abs() < 1e-9);
    }

    /// The corpus door and the one function are the same arithmetic: every note's score
    /// is `blended` of its lexical score and the tier's cosine for it, and the best of
    /// those is what the doors answer.
    #[test]
    fn the_corpus_scores_through_the_one_function() {
        let docs = vec![
            (
                PathBuf::from("a-merge.md"),
                "# Branch\nmerging a branch\n".to_string(),
            ),
            (
                PathBuf::from("b-docker.md"),
                "# Docker\na container\n".to_string(),
            ),
        ];
        let (_m, _d, tier) = tier("scores", &docs);
        let corpus = Corpus::new(&docs);
        let query = "alpha beta gamma delta";
        let attached = tier
            .query(query, letibot_metrics::now_ms() as i64)
            .expect("query");
        let q = corpus.weigh(query);
        let ranked = corpus.ranked_with(query, 10, Some(&tier));
        for note in &corpus.notes {
            let lexical = similarity(&q, &note.weights);
            let semantic = tier.cosine(&note.path.to_string_lossy(), &attached);
            let expected = blended(lexical, semantic);
            let got = ranked
                .iter()
                .find(|(p, _)| *p == note.path.as_path())
                .map(|(_, s)| *s);
            match got {
                Some(got) => assert!(
                    (got - expected).abs() < 1e-6,
                    "{} scored {got} and the blend says {expected}",
                    note.path.display()
                ),
                None => assert!(expected == 0.0, "{}", note.path.display()),
            }
        }
        assert!(!ranked.is_empty(), "the tier left nothing to rank");
        let best = corpus.best_with(query, Some(&tier)).expect("a best");
        assert_eq!(best.0, ranked[0].0);
        assert_eq!(best.1, ranked[0].1);
    }

    /// A tier that cannot answer falls back to the lexical score: the door does not fail
    /// and the caller's floor still applies.
    #[test]
    fn a_tier_that_cannot_answer_falls_back_to_the_words() {
        let docs = vec![
            (
                PathBuf::from("a-merge.md"),
                "# Branch\nmerging a branch\n".to_string(),
            ),
            (
                PathBuf::from("b-docker.md"),
                "# Docker\na container\n".to_string(),
            ),
        ];
        let dir = Dir::new("broken");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"http://127.0.0.1:1\"\nmodel = \"m\"\n")
                .expect("settings");
        let mut broken = Tier::open(&settings, dir.path())
            .expect("open")
            .expect("a tier");
        broken
            .fill(
                docs.iter()
                    .map(|(p, t)| (p.to_string_lossy().to_string(), index_text(p, t))),
                letibot_metrics::now_ms() as i64,
            )
            .ok();
        let corpus = Corpus::new(&docs);
        assert_eq!(
            corpus.best("merging a branch"),
            corpus.best_with("merging a branch", Some(&broken))
        );
        assert_eq!(
            corpus.nearest("merging", None),
            corpus.nearest_with("merging", None, Some(&broken))
        );
        // And the reason is available exactly once, for the caller that says it.
        assert!(broken.take_fault().is_some());
        assert_eq!(broken.take_fault(), None);
    }

    /// Each caller keeps its own floor: one blended score answers two questions
    /// differently, and the blend does not pick for them.
    #[test]
    fn one_score_two_floors() {
        let score = blended(0.126, Some(0.6));
        let offer_floor = 0.3;
        let duplicate_floor = 0.5;
        assert!(score > offer_floor, "{score} is not above the offer floor");
        assert!(
            score < duplicate_floor,
            "{score} is not below the duplicate floor"
        );
    }
}
