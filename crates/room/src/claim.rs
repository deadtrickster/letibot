//! What a message claims about the world, and whether the store's own rows back it. Pure: no clock,
//! no store, no model. A claim is a thing a reader can check, never a thing the room obeys.

/// One thing a text asserts about the state of the world: the phrases a person acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateClaim {
    /// "landed", "landed the fix".
    Landed,
    /// "merged".
    Merged,
    /// "pushed".
    Pushed,
    /// "gate green", "the gate is green", "all green".
    GateGreen,
    /// "passes", "the suite passes".
    Passes,
    /// "fixed in <sha>".
    FixedIn,
    /// "port 5433 free".
    PortFree,
}

impl StateClaim {
    /// The claim as a check line names it.
    pub fn word(self) -> &'static str {
        match self {
            StateClaim::Landed => "landed",
            StateClaim::Merged => "merged",
            StateClaim::Pushed => "pushed",
            StateClaim::GateGreen => "gate green",
            StateClaim::Passes => "passes",
            StateClaim::FixedIn => "fixed in",
            StateClaim::PortFree => "port free",
        }
    }
}

/// The shortest and longest token that may stand for a commit sha, in hex characters.
const SHA_MIN: usize = 7;
const SHA_MAX: usize = 40;

/// The text as lowercase words: punctuation, backticks and `#` are separators, so `gate-green`,
/// `landed.` and ``#abc1234`` are the words a person reads them as.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Is the word at `i` followed by these words?
fn next_is(w: &[String], i: usize, want: &[&str]) -> bool {
    want.iter().enumerate().all(|(n, word)| {
        w.get(i + 1 + n)
            .is_some_and(|got| got.as_str() == *word)
    })
}

/// The state this text claims, if it claims one. Cheap and literal: no model, no negation
/// handling - a line that says "the gate is green" is read as saying it.
pub fn state_claim(text: &str) -> Option<StateClaim> {
    let w = words(text);
    for i in 0..w.len() {
        match w[i].as_str() {
            "landed" => return Some(StateClaim::Landed),
            "merged" => return Some(StateClaim::Merged),
            "pushed" => return Some(StateClaim::Pushed),
            "passes" => return Some(StateClaim::Passes),
            "fixed" if next_is(&w, i, &["in"]) => return Some(StateClaim::FixedIn),
            "all" if next_is(&w, i, &["green"]) => return Some(StateClaim::GateGreen),
            "gate" if next_is(&w, i, &["green"]) || next_is(&w, i, &["is", "green"]) => {
                return Some(StateClaim::GateGreen);
            }
            // `port 5433 free`, `port 5433 is free`: the number and the verdict are two words apart.
            "port" if w[i + 1..].iter().take(3).any(|x| x == "free") => {
                return Some(StateClaim::PortFree);
            }
            _ => {}
        }
    }
    None
}

/// The first token in a text that reads as a commit sha: 7 to 40 hex characters at a word
/// boundary, with at least one digit - `defaced` is a word, and a flag on it would be noise.
pub fn sha_in(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| {
            (SHA_MIN..=SHA_MAX).contains(&w.len())
                && w.bytes().all(|b| b.is_ascii_hexdigit())
                && w.bytes().any(|b| b.is_ascii_digit())
        })
        .map(str::to_ascii_lowercase)
}

/// The claim a text makes that nothing backs: it asserts a state and carries neither an evidence
/// ref nor a sha-looking token. A flag for a reader, never a refusal for the sender.
pub fn unbacked(text: &str, evidence: Option<&str>) -> Option<StateClaim> {
    let claim = state_claim(text)?;
    if evidence.is_some_and(|e| !e.trim().is_empty()) {
        return None;
    }
    sha_in(text).is_none().then_some(claim)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each phrase the brief names is a claim, in the spellings a peer actually writes.
    #[test]
    fn a_state_claim_is_the_phrases_a_person_acts_on() {
        let cases = [
            ("landed on main", StateClaim::Landed),
            ("I merged it", StateClaim::Merged),
            ("pushed, so pull", StateClaim::Pushed),
            ("the gate is green", StateClaim::GateGreen),
            ("gate green now", StateClaim::GateGreen),
            ("all green here", StateClaim::GateGreen),
            ("the suite passes", StateClaim::Passes),
            ("fixed in abc1234", StateClaim::FixedIn),
            ("port 5433 free", StateClaim::PortFree),
            ("port 5433 is free now", StateClaim::PortFree),
            ("Gate-Green for the merge queue", StateClaim::GateGreen),
        ];
        for (text, want) in cases {
            assert_eq!(state_claim(text), Some(want), "{text}");
        }
        for text in [
            "who has the disk?",
            "landing gear",
            "the green room",
            "I will land it when the gate says so",
        ] {
            assert_eq!(state_claim(text), None, "{text}");
        }
    }

    /// A sha is hex at a word boundary, and a word that merely looks like hex is not one.
    #[test]
    fn a_sha_is_a_hex_token_and_not_a_word_that_looks_like_one() {
        assert_eq!(sha_in("landed in 2b97a092"), Some("2b97a092".to_string()));
        assert_eq!(
            sha_in("see `ABC1234` and #0123abc"),
            Some("abc1234".to_string())
        );
        assert_eq!(sha_in("defaced"), None, "a word, not a sha");
        assert_eq!(sha_in("abc123"), None, "six characters is too short");
        assert_eq!(sha_in("no sha here"), None);
        assert_eq!(sha_in(&"a".repeat(41)), None, "40 is the longest");
    }

    /// An unbacked claim is a state claim with nothing behind it: an evidence ref or a sha in the
    /// text is enough to stop the flag.
    #[test]
    fn a_claim_with_no_evidence_and_no_sha_is_unbacked() {
        assert_eq!(unbacked("landed", None), Some(StateClaim::Landed));
        assert_eq!(unbacked("landed", Some("  ")), Some(StateClaim::Landed));
        assert_eq!(unbacked("landed", Some("abc1234")), None);
        assert_eq!(unbacked("landed in 2b97a092", None), None);
        assert_eq!(unbacked("who has the disk?", None), None);
    }
}
