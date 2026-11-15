//! A shadow's reply, read under the guardrails: schema or nothing, every quote an exact substring of
//! what it was shown, and a closed verdict space per job. None of this is in a prompt, so no prompt
//! override and no injected transcript text can loosen it.

use serde::Deserialize;

use super::Job;
use super::guard::{self, Call};
use super::words::Author;

/// The shortest quote that counts as evidence. A two-letter "quote" is a substring of anything.
pub const MIN_QUOTE: usize = 8;

/// Most offers one `during` verdict may make.
pub const MAX_OFFERS: usize = 2;

/// What the shadow was shown, as the checks need it.
#[derive(Debug, Clone, Default)]
pub struct Shown {
    /// The fenced data: transcript, todos, rules. Quotes are checked against this alone, never
    /// against the instructions around it.
    pub data: String,
    /// The names a `during` offer may pick from.
    pub shortlist: Vec<String>,
    /// The call a `permission` verdict is about.
    pub call: Option<Call>,
    /// Who wrote each stretch of `data`, for the author rules: a note quotes only the operator's
    /// own words, and a promise only the assistant's.
    pub spans: Vec<Span>,
}

/// One authored stretch of [`Shown::data`]: byte range, author, and transcript row.
#[derive(Debug, Clone)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub author: Author,
    pub row: Option<usize>,
}

impl Shown {
    /// Append `text` as `author`'s, under `header`, and record its span.
    pub fn push_said(&mut self, header: &str, author: Author, row: Option<usize>, text: &str) {
        self.data.push_str(header);
        self.data.push('\n');
        let start = self.data.len();
        self.data.push_str(text.trim());
        let end = self.data.len();
        self.data.push_str("\n\n");
        self.spans.push(Span {
            start,
            end,
            author,
            row,
        });
    }

    /// The span holding an occurrence of `quote` whose author passes `ok`.
    fn authored(&self, quote: &str, ok: impl Fn(&Author) -> bool) -> Option<&Span> {
        self.data.match_indices(quote).find_map(|(at, _)| {
            self.spans
                .iter()
                .find(|s| ok(&s.author) && s.start <= at && at + quote.len() <= s.end)
        })
    }
}

/// A note's quote and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quoted {
    pub text: String,
    /// The author's label, `the operator`.
    pub author: String,
    /// The transcript row it was quoted from, when known.
    pub row: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reminder {
    Owed { promise: String, evidence: String },
    None,
    Unsure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    Drifted { plan: String, calls: Vec<String> },
    OnPlan,
    Unsure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub name: String,
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { rule: String },
    Ask,
}

/// A verdict that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    TurnEnd {
        reminder: Reminder,
        /// A proposed note, quoted from the operator's own words. Propose-only: nothing writes it.
        note: Option<Quoted>,
    },
    During {
        drift: Drift,
        offers: Vec<Pick>,
    },
    Permission {
        decision: Decision,
        evidence: String,
    },
}

impl Verdict {
    /// The one word a row's title carries.
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::TurnEnd { reminder, .. } => match reminder {
                Reminder::Owed { .. } => "owed",
                Reminder::None => "none",
                Reminder::Unsure => "unsure",
            },
            Verdict::During { drift, .. } => match drift {
                Drift::Drifted { .. } => "drifted",
                Drift::OnPlan => "on_plan",
                Drift::Unsure => "unsure",
            },
            Verdict::Permission { decision, .. } => match decision {
                Decision::Allow { .. } => "allow",
                Decision::Ask => "ask",
            },
        }
    }
}

/// The reply, read: the verdict or why it was voided, and one line per check either way.
#[derive(Debug, Clone)]
pub struct Checked {
    pub verdict: Result<Verdict, String>,
    pub log: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnEndReply {
    verdict: String,
    #[serde(default)]
    promise: String,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferReply {
    name: String,
    quote: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuringReply {
    verdict: String,
    #[serde(default)]
    plan: String,
    #[serde(default)]
    calls: Vec<String>,
    #[serde(default)]
    offers: Vec<OfferReply>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionReply {
    verdict: String,
    #[serde(default)]
    rule: String,
    #[serde(default)]
    evidence: String,
}

/// The JSON object in a reply: the reply itself, or the one inside a ```json fence. Anything
/// else around it is dropped, since models wrap; what is inside still has to parse whole.
fn object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

struct Log(Vec<String>);

impl Log {
    fn pass(&mut self, what: &str) {
        self.0.push(format!("pass {what}"));
    }
    fn reject(mut self, what: &str, why: String) -> Checked {
        self.0.push(format!("reject {what}: {why}"));
        Checked {
            verdict: Err(format!("{what}: {why}")),
            log: self.0,
        }
    }
    fn done(self, v: Verdict) -> Checked {
        Checked {
            verdict: Ok(v),
            log: self.0,
        }
    }
}

/// Every non-empty quote, verified. One fabricated quote voids the whole verdict.
fn verify<'a>(
    data: &str,
    quotes: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), String> {
    for (field, q) in quotes {
        if q.is_empty() {
            continue;
        }
        if q.trim().len() < MIN_QUOTE {
            return Err(format!(
                "`{field}` quote {q:?} is shorter than {MIN_QUOTE} bytes"
            ));
        }
        if !data.contains(q) {
            return Err(format!(
                "`{field}` quote {q:?} is not in what the shadow was shown"
            ));
        }
    }
    Ok(())
}

/// Read `reply` for `job` against what it was shown.
pub fn check(job: Job, reply: &str, shown: &Shown) -> Checked {
    let mut log = Log(Vec::new());
    let Some(json) = object(reply) else {
        return log.reject("schema", "no JSON object in the reply".into());
    };
    match job {
        Job::TurnEnd => {
            let r: TurnEndReply = match serde_json::from_str(json) {
                Ok(r) => r,
                Err(e) => return log.reject("schema", e.to_string()),
            };
            let reminder = match r.verdict.as_str() {
                "owed" if r.promise.is_empty() => {
                    return log.reject("verdict", "`owed` needs a promise quote".into());
                }
                "owed" => Reminder::Owed {
                    promise: r.promise.clone(),
                    evidence: r.evidence.clone(),
                },
                "none" => Reminder::None,
                "unsure" => Reminder::Unsure,
                other => {
                    return log.reject("verdict", format!("`{other}` is not owed, none or unsure"));
                }
            };
            log.pass("schema");
            if let Err(why) = verify(
                &shown.data,
                [
                    ("promise", r.promise.as_str()),
                    ("evidence", r.evidence.as_str()),
                    ("note", r.note.as_str()),
                ],
            ) {
                return log.reject("quotes", why);
            }
            log.pass("quotes");
            // The author rules, in code because a prompt is not a guardrail. A promise is the
            // assistant's own words; a note quotes only what the human operator wrote.
            if !shown.spans.is_empty() {
                if !r.promise.is_empty()
                    && shown
                        .authored(&r.promise, |a| *a == Author::Assistant)
                        .is_none()
                {
                    return log.reject(
                        "author",
                        format!("promise {:?} is not the assistant's words", r.promise),
                    );
                }
                log.pass("author");
            }
            // A note is memory: the operator's words or the model's own statement of what it found.
            // Tool output, fetched content and another agent's line are not, until the model says it.
            let note = match r.note.is_empty() {
                true => None,
                false => match shown.authored(&r.note, |a| {
                    matches!(a, Author::Operator | Author::Assistant)
                }) {
                    Some(span) => {
                        log.pass("note author");
                        Some(Quoted {
                            text: r.note,
                            author: span.author.label(),
                            row: span.row,
                        })
                    }
                    None => {
                        return log.reject(
                            "note author",
                            format!(
                                "note {:?} is neither the operator's nor the model's own words",
                                r.note
                            ),
                        );
                    }
                },
            };
            log.done(Verdict::TurnEnd { reminder, note })
        }
        Job::During => {
            let r: DuringReply = match serde_json::from_str(json) {
                Ok(r) => r,
                Err(e) => return log.reject("schema", e.to_string()),
            };
            let drift = match r.verdict.as_str() {
                "drifted" if r.plan.is_empty() || r.calls.is_empty() => {
                    return log.reject(
                        "verdict",
                        "`drifted` needs the plan item and the off-plan calls quoted".into(),
                    );
                }
                "drifted" => Drift::Drifted {
                    plan: r.plan.clone(),
                    calls: r.calls.clone(),
                },
                "on_plan" => Drift::OnPlan,
                "unsure" => Drift::Unsure,
                other => {
                    return log.reject(
                        "verdict",
                        format!("`{other}` is not drifted, on_plan or unsure"),
                    );
                }
            };
            log.pass("schema");
            if r.offers.len() > MAX_OFFERS {
                return log.reject(
                    "offers",
                    format!("{} offers, and the most is {MAX_OFFERS}", r.offers.len()),
                );
            }
            for o in &r.offers {
                if !shown.shortlist.iter().any(|n| n == &o.name) {
                    return log.reject("offers", format!("`{}` is not on the shortlist", o.name));
                }
                if o.quote.is_empty() {
                    return log.reject(
                        "offers",
                        format!("`{}` has no quote showing the need", o.name),
                    );
                }
            }
            log.pass("offers");
            let quotes = std::iter::once(("plan", r.plan.as_str()))
                .chain(r.calls.iter().map(|c| ("calls", c.as_str())))
                .chain(r.offers.iter().map(|o| ("offers.quote", o.quote.as_str())));
            if let Err(why) = verify(&shown.data, quotes) {
                return log.reject("quotes", why);
            }
            log.pass("quotes");
            // The off-plan calls must come after the plan they left, or it is no drift.
            if let Drift::Drifted { plan, calls } = &drift {
                let at = shown.data.find(plan.as_str()).unwrap_or(0);
                if let Some(c) = calls
                    .iter()
                    .find(|c| shown.data.rfind(c.as_str()).is_none_or(|i| i <= at))
                {
                    return log.reject("order", format!("call {c:?} is not later than the plan"));
                }
                log.pass("order");
            }
            log.done(Verdict::During {
                drift,
                offers: r
                    .offers
                    .into_iter()
                    .map(|o| Pick {
                        name: o.name,
                        quote: o.quote,
                    })
                    .collect(),
            })
        }
        Job::Permission => {
            let r: PermissionReply = match serde_json::from_str(json) {
                Ok(r) => r,
                Err(e) => return log.reject("schema", e.to_string()),
            };
            let allow = match r.verdict.as_str() {
                "allow" if r.rule.is_empty() => {
                    return log.reject("verdict", "`allow` needs the rule it relied on".into());
                }
                "allow" => true,
                "ask" => false,
                other => return log.reject("verdict", format!("`{other}` is not allow or ask")),
            };
            log.pass("schema");
            if let Err(why) = verify(
                &shown.data,
                [("rule", r.rule.as_str()), ("evidence", r.evidence.as_str())],
            ) {
                return log.reject("quotes", why);
            }
            log.pass("quotes");
            let mut decision = if allow {
                Decision::Allow { rule: r.rule }
            } else {
                Decision::Ask
            };
            // An allow still meets the denylist, whatever the transcript said. Blocked is ask.
            if matches!(decision, Decision::Allow { .. }) {
                match shown.call.as_ref().map(guard::allow_passes) {
                    Some(Ok(())) => log.pass("denylist"),
                    Some(Err(why)) => {
                        log.0.push(format!("narrow denylist: {why}; allow is ask"));
                        decision = Decision::Ask;
                    }
                    None => {
                        log.0
                            .push("narrow denylist: no call to check; allow is ask".into());
                        decision = Decision::Ask;
                    }
                }
            }
            log.done(Verdict::Permission {
                decision,
                evidence: r.evidence,
            })
        }
    }
}
