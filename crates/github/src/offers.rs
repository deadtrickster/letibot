//! R114's offers: a pull request whose words are like an open todo row, a running session's brief
//! or a review this seat made before is pointed at it.
//! The scoring is the tree's lexical one (`letibot_tools::similarity`), injected as [`Scorer`]: this
//! crate keeps serde and nothing else, and a test says what is similar without a model.

/// What an offer is for: a row to watch, or a review to read before this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A pull request that may be the same work as an open todo or a running session: watch it.
    Watch,
    /// An earlier review of a change like this one: context, not work.
    Context,
}

impl Kind {
    /// The words an offer's line opens with.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Watch => "watch this",
            Kind::Context => "similar to an earlier review",
        }
    }
}

/// One text an offer can point at: an open todo, a running session's brief, a past review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    /// The one line the offer draws: a todo row's words, a session's title, `review of #12`.
    pub label: String,
    /// What is scored.
    pub text: String,
}

/// Where the candidates come from - the daemon's open todos and its running sessions' briefs.
pub trait Candidates: Send + Sync {
    fn candidates(&self) -> Vec<Candidate>;
}

/// Two texts' likeness, in `0..=1`. The lexical score is `letibot_tools::similarity`; a test hands
/// in its own, which is why nothing here needs a corpus or a model.
pub trait Scorer: Send + Sync {
    fn score(&self, a: &str, b: &str) -> f64;
}

/// One offer a pull request's row carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub kind: Kind,
    pub label: String,
    pub score: f64,
}

impl Offer {
    /// The line the pane draws under the row: what it is, and what it points at.
    pub fn line(&self) -> String {
        format!("{}: {}", self.kind.word(), self.label)
    }
}

/// The score an offer has to reach to be made at all. Higher than the note hint's `0.18`: a pull
/// request and an open todo that are the same work share the nouns, and an offer a person learns to
/// ignore costs more than the one it would have caught.
pub const OFFER_FLOOR: f64 = 0.25;

/// How many offers one row carries, best first. Three is a screen's worth of attention, and a row
/// with ten offers is a row that has told the operator nothing.
pub const MOST: usize = 3;

/// The offers for one subject, best first, over the floor - [`MOST`] of them.
pub fn offers_of(subject: &str, candidates: &[Candidate], scorer: &dyn Scorer) -> Vec<Offer> {
    let mut scored: Vec<Offer> = candidates
        .iter()
        .map(|c| Offer {
            kind: c.kind,
            label: c.label.clone(),
            score: scorer.score(subject, &c.text),
        })
        .filter(|o| o.score >= OFFER_FLOOR)
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(MOST);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scorer a test can reason about: the share of the shorter text's words the longer one also
    /// has. That is the lexical score's shape - shared terms, normalised - without a corpus.
    struct Shared;

    impl Scorer for Shared {
        fn score(&self, a: &str, b: &str) -> f64 {
            let words = |t: &str| -> Vec<String> {
                t.split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 2)
                    .map(|w| w.to_lowercase())
                    .collect()
            };
            let (a, b) = (words(a), words(b));
            if a.is_empty() || b.is_empty() {
                return 0.0;
            }
            let shared = a.iter().filter(|w| b.contains(w)).count();
            shared as f64 / a.len().min(b.len()) as f64
        }
    }

    fn a_candidate(kind: Kind, label: &str, text: &str) -> Candidate {
        Candidate {
            kind,
            label: label.into(),
            text: text.into(),
        }
    }

    /// A pull request about the same work as an open todo is offered as one to watch, and an
    /// unrelated candidate is not offered at all.
    #[test]
    fn a_pull_request_like_an_open_todo_is_offered() {
        let candidates = vec![
            a_candidate(
                Kind::Watch,
                "the notes pane loses its scroll position",
                "the notes pane loses its scroll position when a note is edited",
            ),
            a_candidate(
                Kind::Watch,
                "the deploy script waits for a port",
                "the deploy script waits for a port that never opens",
            ),
        ];
        let offers = offers_of(
            "the notes pane loses its scroll position when a note is edited",
            &candidates,
            &Shared,
        );
        assert_eq!(offers.len(), 1, "{offers:#?}");
        assert_eq!(offers[0].kind, Kind::Watch);
        assert!(offers[0].label.starts_with("the notes pane"), "{offers:#?}");
        assert!(offers[0].score >= OFFER_FLOOR);
    }

    /// An earlier review of a change like this one is context, not work: the two kinds are told
    /// apart on the row and one is never drawn as the other.
    #[test]
    fn a_past_review_is_context_and_not_work() {
        let candidates = vec![a_candidate(
            Kind::Context,
            "review of stroppy-io/letibot#12",
            "the widget is loaded once and the handle is released",
        )];
        let offers = offers_of(
            "the widget is loaded once, and the handle",
            &candidates,
            &Shared,
        );
        assert_eq!(offers.len(), 1, "{offers:#?}");
        assert_eq!(offers[0].kind, Kind::Context);
        assert!(
            offers[0].line().starts_with("similar to an earlier review"),
            "{offers:#?}"
        );
    }

    /// An offer is over the floor or it is not made: a weak match is a line a person learns to skip,
    /// and it costs the one that would have been read.
    #[test]
    fn nothing_under_the_floor_is_offered_and_the_order_is_by_score() {
        let candidates = vec![
            a_candidate(
                Kind::Watch,
                "a port nobody opens",
                "deploy waits for a port",
            ),
            a_candidate(
                Kind::Watch,
                "the notes pane",
                "notes pane scroll position is lost",
            ),
            a_candidate(
                Kind::Watch,
                "nothing in common",
                "kubernetes ingress certificates",
            ),
        ];
        let offers = offers_of("the notes pane scroll position", &candidates, &Shared);
        assert_eq!(offers.len(), 1, "{offers:#?}");
        assert!(offers[0].label.contains("notes pane"), "{offers:#?}");
        assert!(
            !offers.iter().any(|o| o.label.contains("kubernetes")),
            "{offers:#?}"
        );
    }

    /// One row carries [`MOST`] offers, best first: an endless list is a list nobody reads.
    #[test]
    fn at_most_most_offers_are_made_best_first() {
        let candidates: Vec<Candidate> = (0..6)
            .map(|i| {
                a_candidate(
                    Kind::Watch,
                    &format!("todo {i}"),
                    &format!("the notes pane scroll position {i}"),
                )
            })
            .collect();
        let offers = offers_of("the notes pane scroll position", &candidates, &Shared);
        assert_eq!(offers.len(), MOST, "{offers:#?}");
        assert!(
            offers.windows(2).all(|w| w[0].score >= w[1].score),
            "{offers:#?}"
        );
    }
}
