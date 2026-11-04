//! Cosine, and the corpus as a list of vectors: brute force on purpose.
//! The corpus fits in one array, so a vector store would be a dependency and a second source of truth.

/// How alike two vectors are, `0.0`..`1.0`, and `0.0` when they cannot be compared: two
/// lengths that differ are two models' answers, and a score across them is not a score.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let den = na.sqrt() * nb.sqrt();
    // A zero or a NaN denominator is two vectors with no direction between them, and a
    // NaN score is a floor nothing can be applied to.
    if den <= 0.0 || den.is_nan() {
        return 0.0;
    }
    (dot / den).clamp(0.0, 1.0)
}

/// One document's vector, under the key the caller knows it by - a note's path, a
/// skill's name.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    vector: Vec<f32>,
}

/// Every document's vector, in memory.
#[derive(Debug, Clone, Default)]
pub struct Index {
    rows: Vec<Entry>,
}

impl Index {
    /// Build from `(key, vector)` pairs. A key that appears twice keeps its last vector,
    /// so a caller that re-embeds a document does not score it twice.
    pub fn new(rows: impl IntoIterator<Item = (String, Vec<f32>)>) -> Index {
        let mut out: Vec<Entry> = Vec::new();
        for (key, vector) in rows {
            match out.iter_mut().find(|e| e.key == key) {
                Some(had) => had.vector = vector,
                None => out.push(Entry { key, vector }),
            }
        }
        Index { rows: out }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The keys in the order they were built.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().map(|e| e.key.as_str())
    }

    /// One document's vector.
    pub fn vector(&self, key: &str) -> Option<&[f32]> {
        self.rows
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.vector.as_slice())
    }

    /// How alike a document and a query are. `None` when the document is not in the
    /// index at all, which a caller must keep distinct from `0.0`: not embedded is not
    /// the same as not alike.
    pub fn cosine_to(&self, key: &str, query: &[f32]) -> Option<f32> {
        self.vector(key).map(|v| cosine(v, query))
    }

    /// Every document's score against a query, best first, ties broken by key so the
    /// same answer comes out every round.
    pub fn nearest(&self, query: &[f32], most: usize) -> Vec<(&str, f32)> {
        let mut all: Vec<(&str, f32)> = self
            .rows
            .iter()
            .filter(|e| e.vector.len() == query.len())
            .map(|e| (e.key.as_str(), cosine(&e.vector, query)))
            .collect();
        all.sort_by(|(ka, sa), (kb, sb)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| ka.cmp(kb))
        });
        all.truncate(most);
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_is_one_for_one_direction_and_zero_across_axes() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    /// A score across two model widths is not a score, and a NaN is a floor that can
    /// never be applied. Both answer zero.
    #[test]
    fn a_mismatch_is_zero_rather_than_a_panic_or_a_nan() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    /// The ranking the second tier exists for: a neighbour that shares a direction comes
    /// before one that shares none, and the order is stable.
    #[test]
    fn nearest_ranks_the_closest_vector_first() {
        let index = Index::new([
            ("paraphrase".to_string(), vec![0.9, 0.4, 0.0, 0.0]),
            ("unrelated".to_string(), vec![0.0, 0.0, 1.0, 0.0]),
            ("neighbour".to_string(), vec![0.8, 0.5, 0.1, 0.0]),
        ]);
        let ranked = index.nearest(&[1.0, 0.5, 0.0, 0.0], 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, "paraphrase");
        assert_eq!(ranked[1].0, "neighbour");
    }

    /// `None` is "not in the index" and `0.0` is "in the index and unlike": a caller
    /// that could not tell them apart would treat a missing document as a distant one.
    #[test]
    fn a_document_that_was_never_embedded_is_none_and_not_zero() {
        let index = Index::new([("a".to_string(), vec![1.0, 0.0])]);
        assert_eq!(index.cosine_to("b", &[1.0, 0.0]), None);
        assert_eq!(index.cosine_to("a", &[0.0, 1.0]), Some(0.0));
    }

    /// A document embedded twice is scored once, and a vector of another width is not
    /// compared at all.
    #[test]
    fn a_re_embedded_key_keeps_one_vector_and_a_wrong_width_is_skipped() {
        let index = Index::new([
            ("a".to_string(), vec![1.0, 0.0]),
            ("a".to_string(), vec![0.0, 1.0]),
            ("short".to_string(), vec![1.0]),
        ]);
        assert_eq!(index.len(), 2);
        assert_eq!(index.vector("a"), Some([0.0, 1.0].as_slice()));
        assert_eq!(index.nearest(&[0.0, 1.0], 5).len(), 1);
    }
}
