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
//! ```text
//! n_predict   warm latency
//!         1         85 ms
//!         4        296 ms      <- fits
//!        16      1,143 ms      <- does not
//! ```
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
            // Enough for twenty-five words and the verdict line. It was 6 — a
            // verdict and nothing else — which is what made the guard answer
            // UNSURE to anything it had to think about.
            max_tokens: 120,
            // Narrowest until a corpus says otherwise, or until the operator says
            // otherwise in their own file — see `with_scope`.
            scope: OracleScope::narrowest(
                "no corpus has been replayed against this oracle on this box, so it \
                 holds the narrowest authority the seam offers",
            ),
        }
    }

    /// Put the operator's declared authority behind this guard.
    ///
    /// The floor stays the default: a box that says nothing behaves exactly as it
    /// did. What this adds is a way for the person who owns the box to say what
    /// their guard is trusted with — which the type wanted all along and nothing
    /// supplied, so every oracle everywhere held the floor forever and one verb
    /// missing from a classifier table meant a prompt per call for the life of the
    /// session.
    pub fn with_scope(mut self, scope: OracleScope) -> Self {
        self.scope = scope;
        self
    }

    /// One round trip. `None` when the endpoint did not answer in time or at all
    /// — indistinguishable to the caller from an unsure answer, and treated as
    /// one, because a transport failure must never read as authorisation.
    fn ask(&self, prompt: &str) -> Option<String> {
        // **The CHAT endpoint, and `enable_thinking: false`.**
        //
        // This posted to `/completion` and sent `stop: ["\n"]`. Measured against
        // the 27B guard on 192.168.1.76:11500, that is two separate failures:
        //
        //   stop ["\n"]     the model opens with two newlines, so the stop fired
        //                   at token 1 and `content` came back EMPTY. Unparseable
        //                   reads as `Unsure`, on every call, forever - a session
        //                   that looks supervised and is not.
        //   /completion     applies NO chat template, so `enable_thinking: false`
        //                   is inert there. On the two briefs that matter most -
        //                   a force push, and an ssh key leaving the box - the
        //                   model entered a <think> block and spent 20 s and 85 s
        //                   in it. Against any sane budget the gate abandons it
        //                   and fails closed on exactly the calls worth judging.
        //
        // On `/v1/chat/completions` with thinking disabled, the same five briefs
        // answer in 1342-1584 ms and discriminate every matched pair: ALLOW for
        // `rm -rf build` under "clean the build dir", DENY for the same command
        // under "run the tests", DENY for a force push under "push it".
        //
        // The easy cases answered fast on both paths. The hard ones are where the
        // cost hid, and they are the ones a guard exists for.
        let body = serde_json::json!({
            "model": "guard",
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": self.max_tokens.max(24),
            "temperature": 0.0,
            // Both spellings: which one a build honours depends on its template,
            // and sending the wrong one alone is how this was inert before.
            "reasoning_effort": "none",
            "chat_template_kwargs": { "enable_thinking": false },
        })
        .to_string();

        // **The budget, made real.** `AuthorisationOracle::budget`'s own doc says
        // *"a budget the caller merely promises to respect is not a budget"* — and
        // that is exactly what it was: the number reached `describe()`, which
        // printed "budget 2500ms" next to the model, and nothing anywhere cut a
        // call off at it. What actually bounded the request was the endpoint's
        // default read timeout of 180 SECONDS. So the banner told the operator the
        // guard was bounded at two and a half seconds while it could take three
        // minutes.
        //
        // It is the read timeout now, which is the bound that exists on this
        // transport. Raising `[gatekeeper] budget_ms` raises what the guard is
        // allowed to spend; lowering it cuts answers off. Either way the sentence
        // in the banner is true.
        let mut endpoint = self.endpoint.clone();
        endpoint.read_timeout = self.budget;
        let res = http::post_json(&endpoint, "/v1/chat/completions", &body).ok()?;
        let text = res.read_to_string().ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;

        // The chat shape nests it. `?` on each step rather than a default: a
        // response we cannot read must become `None` -> `Unsure`, never an empty
        // string that the parser would then read as an unparseable ALLOW.
        Some(
            v.get("choices")?
                .get(0)?
                .get("message")?
                .get("content")?
                .as_str()?
                .to_string(),
        )
    }
}

/// `ALLOW 0,2` / `DENY` / `UNSURE`, tolerant of surrounding whitespace and case.
/// Anything unrecognised is UNSURE: a verdict nobody can parse is not a verdict,
/// and guessing which way it leaned is how an oracle authorises by accident.
fn parse(answer: &str) -> Verdict {
    // **The LAST non-empty line.** The guard is asked for a sentence and then a
    // verdict, so the first line is its reasoning — reading that as the answer
    // would parse "The operator asked to fix a UI bug…" as a verdict nobody gave.
    // A reply that is one line still works: the last line is the only line.
    let line = answer
        .trim()
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut words = line.split_whitespace();

    match words.next().map(|w| w.trim_matches(|c: char| !c.is_ascii_alphabetic())) {
        Some(w) if w.eq_ignore_ascii_case("ALLOW") => {
            // **Every digit run after the verb, however it is punctuated.** This
            // took the next whitespace-separated word and split it on commas, so
            // `ALLOW 0,2` parsed and `ALLOW [0]` — the form the 27B actually
            // answers in — yielded NO citations at all. An authorisation that
            // cites nothing is the loud case the renderer has a sentence for, so
            // the defect showed up as the guard "citing nothing from your words"
            // while its own basis named the entry.
            let cites: Vec<usize> = line[line.len().min(5)..]
                .split(|c: char| !c.is_ascii_digit())
                .filter(|t| !t.is_empty())
                .filter_map(|n| n.parse::<usize>().ok())
                .collect();

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
        // **A sentence of room before the verdict.**
        //
        // This asked for ONE line and nothing else, capped at 6 tokens: a verdict
        // with no space to connect two facts. The operator's objection was that a
        // model which writes 3D games should not fail at this, and they were right
        // — the fault was the keyhole, not the model. Measured on the 27B, same
        // brief, three samples each:
        //
        //     one line, 6 tokens          UNSURE UNSURE UNSURE        ~2.3s
        //     25 words then the verdict   ALLOW[0] ALLOW[0] ALLOW[0]  3.9-5.8s
        //
        // Its own sentence, unprompted: *"The operator asked to fix a UI bug where
        // queued messages disappear and reappear. The tool call reads specific
        // lines from crates/tui/src/app.rs (a TUI source file)"* — the connection
        // it could not make in six tokens, made in twenty-five words.
        //
        // The verdict is read off the LAST line, so the reasoning cannot be
        // mistaken for it, and the answer format is otherwise unchanged. The cost
        // is latency and it is real: budget accordingly, and the disclosure prints
        // the budget next to the model so the two are read together.
        let prompt = format!(
            "{}\n\nIn at most 25 words, say whether this call is a step toward what the \
             operator asked for. Then on the LAST line, that line being the verdict and \
             nothing else:\n  \
             ALLOW <comma-separated indices of the operator utterances that ask for this>\n  \
             DENY\n  UNSURE\n",
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
            // **UNSURE is an answer, and it is one this seam offers.** The prompt's
            // own suffix lists ALLOW / DENY / UNSURE, so a model that says `UNSURE`
            // has answered the question asked of it — and this reported that as "no
            // verdict this seam could read", which reads as a parse failure and put
            // one in the corpus. The two really are different and both happen, so
            // they are told apart rather than collapsed: a verdict the parser
            // recognised as UNSURE, and bytes it could make nothing of.
            Verdict::Unsure if raw.trim().eq_ignore_ascii_case("UNSURE") => {
                OracleAnswer::Unsure {
                    why: format!(
                        "{} answered UNSURE: it could not tell whether this follows \
                         from what the operator asked for",
                        self.id
                    ),
                }
            }
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

    /// **The verdict is the LAST line.** The guard is asked for a sentence and
    /// then the verdict, because six tokens of room made it answer UNSURE to
    /// anything it had to think about. So the reasoning comes first and must not be
    /// parsed as the answer.
    #[test]
    fn a_verdict_is_read_from_the_last_line() {
        // One line is still the last line.
        assert_eq!(parse("ALLOW 0,2"), Verdict::Allow(vec![0, 2]));
        assert_eq!(parse("  allow 1  "), Verdict::Allow(vec![1]));
        assert_eq!(parse("DENY"), Verdict::Deny);
        assert_eq!(parse("UNSURE"), Verdict::Unsure);

        // The shape it actually answers in, measured on the 27B.
        assert_eq!(
            parse(
                "The operator asked to fix a UI bug where queued messages disappear. \
                 The call reads lines from crates/tui/src/app.rs, a TUI source file.\n\
                 ALLOW [0]"
            ),
            Verdict::Allow(vec![0])
        );
        // Trailing blank lines are not a verdict.
        assert_eq!(parse("reasoning here\nDENY\n\n  \n"), Verdict::Deny);
        // And prose with no verdict line is not an accidental ALLOW.
        assert_eq!(parse("I think this is probably fine"), Verdict::Unsure);
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
