//! Layer B over HTTP: the seam `ModelAdjudicator` has been refusing on.
//!
//! # Why it lives here and not in `letibot-tools`
//!
//! That crate says so itself: *"this crate has no HTTP client, no async runtime
//! and no backend, and a seam that could only be exercised by standing up
//! inference is a seam nobody tests."* `ScriptedOracle` is the fake it ships so
//! the seam has tests. This is the real one, and it replaces that function and
//! nothing else.
//!
//! # The budget is the design constraint, not a detail
//!
//! [`AuthorisationOracle::budget`] defaults to 400ms and `ModelAdjudicator`
//! ENFORCES it: an oracle that overruns is abandoned and the request becomes
//! `Timeout`, which fails closed. That rules out an oracle that explains itself.
//! Measured on the guard model here (Qwen3-4B-Instruct Q6_K, CPU only):
//!
//!     n_predict   warm latency
//!             1         85 ms
//!             4        296 ms      <- fits
//!            16      1,143 ms      <- does not
//!
//! So the answer is a VERDICT, not prose: a word and, when it authorises, the
//! trail indices it relies on. The explanation a human reads is generated later,
//! off the tool path, from the corpus row — the budget is for the decision.
//!
//! Prompt caching is the other half: 736ms cold against 85ms warm. The brief's
//! prefix is stable by construction (`ModelBrief::render` opens with the same
//! instruction every time), so `cache_prompt` keeps the common prefix resident.

use std::time::Duration;

use letibot_tools::{
    AuthorisationOracle, ModelBrief, OracleAnswer, OracleScope, Widening,
};
use letibot_turn::http::{self, Endpoint};

pub struct HttpOracle {
    endpoint: Endpoint,
    /// For `describe`, so an operator can see WHICH model holds this authority.
    id: String,
    budget: Duration,
    max_tokens: usize,
    scope: OracleScope,
}

impl HttpOracle {
    /// `endpoint` speaks llama.cpp's `/completion`.
    pub fn new(endpoint: Endpoint, id: impl Into<String>, budget: Duration) -> Self {
        HttpOracle {
            endpoint,
            id: id.into(),
            budget,
            // Enough for `ALLOW 0,2` and no more. Raising this buys prose and
            // spends the budget; see the module header for the measurements.
            max_tokens: 6,
            // Narrowest until a corpus says otherwise. Widening this is a
            // configuration change with a number behind it, per OracleScope.
            scope: OracleScope::narrowest(
                "no corpus has been replayed against this oracle on this box, so it \
                 holds the narrowest authority the seam offers",
            ),
        }
    }

    /// One round trip. `None` when the endpoint did not answer in time or at all
    /// — indistinguishable to the caller from an unsure answer, and treated as
    /// one, because a transport failure must never read as authorisation.
    fn ask(&self, prompt: &str) -> Option<String> {
        let body = serde_json::json!({
            "prompt": prompt,
            "n_predict": self.max_tokens,
            "temperature": 0.0,
            "top_k": 1,
            "cache_prompt": true,
            // Stop as soon as the verdict line ends: the model will happily
            // start explaining, and every token after the verdict is budget
            // spent on text nobody reads.
            "stop": ["\n"],
        })
        .to_string();

        let res = http::post_json(&self.endpoint, "/completion", &body).ok()?;
        let text = res.read_to_string().ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;

        Some(v.get("content")?.as_str()?.to_string())
    }
}

/// `ALLOW 0,2` / `DENY` / `UNSURE`, tolerant of surrounding whitespace and case.
/// Anything unrecognised is UNSURE: a verdict nobody can parse is not a verdict,
/// and guessing which way it leaned is how an oracle authorises by accident.
fn parse(answer: &str) -> Verdict {
    let line = answer.trim().lines().next().unwrap_or("").trim();
    let mut words = line.split_whitespace();

    match words.next().map(|w| w.trim_matches(|c: char| !c.is_ascii_alphabetic())) {
        Some(w) if w.eq_ignore_ascii_case("ALLOW") => {
            let cites = words
                .next()
                .map(|rest| {
                    rest.split(',')
                        .filter_map(|n| n.trim().parse::<usize>().ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            Verdict::Allow(cites)
        }
        Some(w) if w.eq_ignore_ascii_case("DENY") => Verdict::Deny,
        _ => Verdict::Unsure,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Allow(Vec<usize>),
    Deny,
    Unsure,
}

impl AuthorisationOracle for HttpOracle {
    fn authorised(&self, brief: &mut ModelBrief) -> OracleAnswer {
        let request_id = brief.request_id.clone();
        let prompt = format!(
            "{}\n\nAnswer with ONE line and nothing else:\n  \
             ALLOW <comma-separated indices of the operator utterances that ask for this>\n  \
             DENY\n  UNSURE\nAnswer:",
            brief.render()
        );

        let Some(raw) = self.ask(&prompt) else {
            return OracleAnswer::Unsure {
                why: format!("{} did not answer", self.id),
            };
        };

        match parse(&raw) {
            Verdict::Allow(cites) => {
                // The witness is layer A's, taken once. Without it there is
                // nothing to widen and ALLOW is not an available answer --
                // which is the rule that keeps layer B from promoting out of
                // AlwaysAsk or Inexpressible.
                let Some(witness) = brief.adjudicable() else {
                    return OracleAnswer::NotAuthorised {
                        why: format!(
                            "{} answered ALLOW on an action the baseline did not mark \
                             adjudicable; the baseline stands",
                            self.id
                        ),
                    };
                };

                // An authorisation that cites nothing is not one.
                if cites.is_empty() {
                    return OracleAnswer::Unsure {
                        why: format!(
                            "{} answered ALLOW without citing any operator utterance",
                            self.id
                        ),
                    };
                }

                OracleAnswer::Authorised(Widening::new(
                    witness,
                    request_id,
                    cites,
                    format!("{} read the trail as asking for this", self.id),
                ))
            }
            Verdict::Deny => OracleAnswer::NotAuthorised {
                why: format!("{} found nothing in the trail that asks for this", self.id),
            },
            Verdict::Unsure => OracleAnswer::Unsure {
                why: format!("{} gave no verdict this seam could read: {:?}", self.id, raw.trim()),
            },
        }
    }

    fn describe(&self) -> String {
        format!(
            "model oracle `{}` at {} (budget {}ms, verdict only)",
            self.id,
            self.endpoint.authority(),
            self.budget.as_millis()
        )
    }

    fn scope(&self) -> OracleScope {
        self.scope.clone()
    }

    fn budget(&self) -> Duration {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_read_from_the_first_line_only() {
        assert_eq!(parse("ALLOW 0,2"), Verdict::Allow(vec![0, 2]));
        assert_eq!(parse("  allow 1  "), Verdict::Allow(vec![1]));
        assert_eq!(parse("DENY"), Verdict::Deny);
        assert_eq!(parse("deny\nExplanation: ..."), Verdict::Deny);
        assert_eq!(parse("UNSURE"), Verdict::Unsure);
    }

    /// Anything unparseable is UNSURE rather than a guess. An oracle that leans
    /// toward ALLOW when its output is malformed authorises by accident.
    #[test]
    fn an_unreadable_answer_is_unsure_not_a_guess() {
        for junk in ["", "\n", "maybe?", "I think that", "42", "ALLOWANCE"] {
            assert_eq!(parse(junk), Verdict::Unsure, "input {junk:?}");
        }
    }

    /// ALLOW without citations is not an authorisation. The Widening type
    /// requires cites for the same reason.
    #[test]
    fn allow_with_no_citation_parses_but_carries_nothing() {
        assert_eq!(parse("ALLOW"), Verdict::Allow(vec![]));
    }
}
