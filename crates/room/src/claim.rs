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
            // `port 5433 free`, `port 5433 on lab2x1 is free`: the number and the verdict are a few
            // words apart, and a host name in between is not a different claim.
            "port" if w[i + 1..].iter().take(5).any(|x| x == "free") => {
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

/// One row the store has, as the checker reads it: a merge row and the gate row of its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The merge row's own id, e.g. `m-7`.
    pub id: String,
    pub repository: String,
    pub target: String,
    pub branch: String,
    /// The row's state word: `landed`, `gating`, `failed`, ...
    pub state: String,
    pub approved_sha: String,
    pub landed_sha: String,
    /// Whether the push to origin happened.
    pub pushed: bool,
    /// The target's gate: `None` when no `target_health` row covers it.
    pub gate_green: Option<bool>,
    /// The row's own evidence text, searched for the id a claim names.
    pub evidence: String,
}

impl Fact {
    /// Is this row on the target, however far the push got?
    fn on_the_target(&self) -> bool {
        self.state == "landed" || self.state == "landed_local"
    }

    /// Does this row answer to `token`: its id, a sha it judged, or its own evidence text?
    fn answers_to(&self, token: &str) -> bool {
        self.id.eq_ignore_ascii_case(token)
            || [&self.approved_sha, &self.landed_sha]
                .iter()
                .any(|sha| !sha.is_empty() && sha.to_ascii_lowercase().starts_with(token))
            || self
                .evidence
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '#')
                .any(|w| w.trim_start_matches('#').eq_ignore_ascii_case(token))
    }
}

/// What the store says about a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A row or a gate record says the same thing.
    Supported,
    /// A row or a gate record says otherwise.
    Contradicted,
    /// The store holds nothing that speaks to it either way.
    NotCovered,
}

impl Verdict {
    /// The verdict as a check line names it.
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Supported => "SUPPORTED",
            Verdict::Contradicted => "CONTRADICTED",
            Verdict::NotCovered => "NOT COVERED",
        }
    }
}

/// One message's claim, judged against the store's rows, with the row that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub claim: StateClaim,
    pub verdict: Verdict,
    /// One line saying which row said so, or what the store does not hold.
    pub why: String,
}

/// The tokens a message names as its backing: the evidence ref's words, and every sha or row id in
/// the text (`m-7`, `q-12`, `2b97a092`).
fn tokens(text: &str, evidence: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut take = |raw: &str| {
        let t = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        let t = t.to_ascii_lowercase();
        let sha = (SHA_MIN..=SHA_MAX).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit());
        let row = t
            .split_once('-')
            .is_some_and(|(kind, n)| kind.len() <= 3 && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !t.is_empty() && (sha || row) && !out.contains(&t) {
            out.push(t);
        }
    };
    for t in evidence.unwrap_or("").split_whitespace() {
        take(t);
    }
    for w in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        take(w);
    }
    out
}

/// Judge what a message claims about the world against the rows the store has. `None` when the
/// message claims no state at all - there is nothing to check.
pub fn check(text: &str, evidence: Option<&str>, facts: &[Fact]) -> Option<Checked> {
    let claim = state_claim(text)?;
    if claim == StateClaim::PortFree {
        return Some(Checked {
            claim,
            verdict: Verdict::NotCovered,
            why: "the store holds no port rows".to_string(),
        });
    }
    let named = tokens(text, evidence);
    let asked = named.first().map(String::as_str).unwrap_or("");
    let Some(fact) = named
        .iter()
        .find_map(|t| facts.iter().find(|f| f.answers_to(t)))
    else {
        return Some(Checked {
            claim,
            verdict: Verdict::NotCovered,
            why: if named.is_empty() {
                format!(
                    "`{}` is claimed with no sha, row or evidence to look up",
                    claim.word()
                )
            } else {
                format!("no merge or gate row names `{asked}`")
            },
        });
    };
    let (verdict, why) = match claim {
        StateClaim::Landed | StateClaim::Merged | StateClaim::FixedIn => {
            if fact.on_the_target() {
                (
                    Verdict::Supported,
                    format!("merge {} is `{}`", fact.id, fact.state),
                )
            } else {
                (
                    Verdict::Contradicted,
                    format!("merge {} is `{}`, not on {}", fact.id, fact.state, fact.target),
                )
            }
        }
        StateClaim::Pushed => {
            if fact.pushed {
                (
                    Verdict::Supported,
                    format!("merge {} was pushed", fact.id),
                )
            } else {
                (
                    Verdict::Contradicted,
                    format!("merge {} is `{}` and was never pushed", fact.id, fact.state),
                )
            }
        }
        StateClaim::GateGreen | StateClaim::Passes => match fact.gate_green {
            Some(true) => (
                Verdict::Supported,
                format!("{} gate is green", fact.target),
            ),
            Some(false) => (
                Verdict::Contradicted,
                format!("{} gate is not green", fact.target),
            ),
            None => (
                Verdict::NotCovered,
                format!("no gate record for {}", fact.target),
            ),
        },
        StateClaim::PortFree => (
            Verdict::NotCovered,
            "the store holds no port rows".to_string(),
        ),
    };
    Some(Checked {
        claim,
        verdict,
        why,
    })
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
            ("port 5433 on lab2x1 is free", StateClaim::PortFree),
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

    fn row(id: &str, state: &str, landed: &str, gate: Option<bool>) -> Fact {
        Fact {
            id: id.into(),
            repository: "/box/letibot".into(),
            target: "main".into(),
            branch: format!("topic/{id}"),
            state: state.into(),
            approved_sha: "aaaaaaa1111".into(),
            landed_sha: landed.into(),
            pushed: state == "landed",
            gate_green: gate,
            evidence: format!("landed on main ({id})"),
        }
    }

    /// The four verdicts a claim about a merge can get, and the row each one rests on.
    #[test]
    fn a_claim_is_judged_against_the_rows_the_store_has() {
        let facts = vec![
            row("m-7", "landed", "2b97a092", Some(true)),
            row("m-8", "gating", "", Some(false)),
            row("m-9", "landed", "", None),
        ];
        let got = |text: &str, evidence: Option<&str>| {
            check(text, evidence, &facts).expect("a claim is judged")
        };
        // By the sha the message names, and by an evidence ref that names the row.
        let landed = got("landed in 2b97a092", None);
        assert_eq!(landed.verdict, Verdict::Supported);
        assert!(landed.why.contains("m-7"), "{}", landed.why);
        let by_row = got("the fix merged", Some("m-7"));
        assert_eq!(by_row.verdict, Verdict::Supported);
        // A row that is not on the target contradicts a landing.
        let early = got("landed as m-8", None);
        assert_eq!(early.verdict, Verdict::Contradicted);
        assert!(early.why.contains("gating"), "{}", early.why);
        // The gate, from the target's own health row.
        assert_eq!(got("the gate is green in m-7", None).verdict, Verdict::Supported);
        assert_eq!(
            got("gate green", Some("m-8")).verdict,
            Verdict::Contradicted
        );
        assert_eq!(
            got("all green", Some("m-9")).verdict,
            Verdict::NotCovered,
            "no gate record for the target"
        );
        // Nothing names a row this store holds, and a port is not a row at all.
        assert_eq!(
            got("landed in deadbe12", None).verdict,
            Verdict::NotCovered
        );
        assert_eq!(got("landed", None).verdict, Verdict::NotCovered);
        assert_eq!(
            got("port 5433 is free", Some("q-3")).verdict,
            Verdict::NotCovered
        );
        assert!(got("port 5433 is free", Some("q-3")).why.contains("port"));
        // A message that claims no state is not judged at all.
        assert_eq!(check("who has the disk?", None, &facts), None);
    }
}
