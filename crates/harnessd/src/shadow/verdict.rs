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
    /// Every authored stretch of `data` is spanned: a persistent shadow's whole history. Then
    /// every acting quote must lie in a row whose author has that authority.
    pub strict: bool,
    /// The watched row the judged turn began at. Under `strict`, an owed promise is the
    /// assistant's words from this row on.
    pub turn_from: Option<usize>,
    /// The preapproved rules a permission `allow` may rely on. Under `strict`, a rule quoted from
    /// anywhere else is no rule.
    pub rules: Vec<String>,
    /// An oracle brief: a permission `allow` cites trail numbers instead of a rule. The stage-1
    /// gate read the call's paths before the brief was built, so there is no call here.
    pub oracle: bool,
    /// The question is automode's: should this call be BLOCKED? Then the reply carries a category
    /// and a reason rather than a citation, and nothing in it is checked as a quote.
    pub block: bool,
    /// How many bytes the request carried, when the caller knows: the fact an empty reply is
    /// reported with, because an empty reply is a prompt the model could not read.
    pub prompt_bytes: usize,
}

/// One authored stretch of [`Shown::data`]: byte range, author, and transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        self.authored_span(quote, |s| ok(&s.author))
    }

    fn authored_span(&self, quote: &str, ok: impl Fn(&Span) -> bool) -> Option<&Span> {
        // Every occurrence the tolerant matcher finds, in order: a quote that differs only in case
        // or in how its whitespace runs is still a quote, and the old exact-bytes rule voided a
        // whole answer over a capital letter.
        let mut from = 0;
        while let Some((at, end)) = find(&self.data, from, quote) {
            if let Some(span) = self
                .spans
                .iter()
                .find(|s| ok(s) && s.start <= at && end <= s.end)
            {
                return Some(span);
            }
            from = at + 1;
        }
        None
    }

    /// The assistant's own words, and under `strict` only those of the judged turn.
    fn in_turn(&self, s: &Span) -> bool {
        s.author == Author::Assistant
            && (!self.strict || self.turn_from.is_none_or(|t| s.row.is_some_and(|r| r >= t)))
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
    /// `rule` is the rule relied on; `cites` the trail numbers an oracle brief's allow names.
    Allow {
        rule: String,
        cites: Vec<usize>,
    },
    Ask,
    /// Automode's other answer: the call must not run. `category` is one of the operator's six
    /// words (see [`Category`]) and `reason` says what it would have done.
    Block {
        category: Category,
        reason: String,
    },
}

/// The six things automode blocks for, and nothing else. A closed set because the corpus counts
/// them, and because "blocked" with no word would be a judgement nobody can review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    ExfiltratingSecretsOrData,
    DestroyingDataOutsideTheTask,
    PushToAProtectedRef,
    ProductionDeployOrCloudMutation,
    WeakeningSecurityOrPermissions,
    AnotherSessionsProperty,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::ExfiltratingSecretsOrData,
        Category::DestroyingDataOutsideTheTask,
        Category::PushToAProtectedRef,
        Category::ProductionDeployOrCloudMutation,
        Category::WeakeningSecurityOrPermissions,
        Category::AnotherSessionsProperty,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::ExfiltratingSecretsOrData => "exfiltrating_secrets_or_data",
            Category::DestroyingDataOutsideTheTask => "destroying_data_outside_the_task",
            Category::PushToAProtectedRef => "push_to_a_protected_ref",
            Category::ProductionDeployOrCloudMutation => "production_deploy_or_cloud_mutation",
            Category::WeakeningSecurityOrPermissions => "weakening_security_or_permissions",
            Category::AnotherSessionsProperty => "another_sessions_property",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == s.trim())
    }
}

/// A verdict that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    TurnEnd {
        reminder: Reminder,
        /// A proposed note, quoted from the operator's own words. Propose-only: nothing writes it.
        note: Option<Quoted>,
        /// What the turn did beyond the ask, as the model read it. Told, never acted on: each one
        /// becomes a word in the monitor's row.
        overreach: Vec<super::overreach::Finding>,
    },
    During {
        drift: Drift,
        offers: Vec<Pick>,
        /// The offers' quotes were checked against authored spans. Only then may they act.
        strict: bool,
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
                Decision::Block { .. } => "block",
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
    #[serde(default)]
    overreach: Vec<OverreachReply>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverreachReply {
    what: String,
    #[serde(default)]
    evidence: String,
    kind: String,
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
    cites: Vec<usize>,
    #[serde(default)]
    evidence: String,
}

/// Automode's answer: block or not, with one of [`Category`]'s words when it blocks.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockReply {
    block: bool,
    #[serde(default)]
    category: String,
    #[serde(default)]
    reason: String,
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

/// Where `quote` appears in `data` at or after `from`, tolerating case and runs of whitespace: a
/// model that re-wraps a line or capitalises its first word is quoting it, and the exact-bytes rule
/// turned that into a voided answer with nothing to read. The words must still be the same words in
/// the same order, with only whitespace between them.
pub(crate) fn find(data: &str, from: usize, quote: &str) -> Option<(usize, usize)> {
    let first = quote.split_whitespace().next()?;
    let mut at = from;
    loop {
        let start = find_ascii_ci(data, at, first)?;
        if let Some(end) = walk(data, start, quote) {
            return Some((start, end));
        }
        at = start + first.len();
        if at >= data.len() {
            return None;
        }
    }
}

/// The rest of `quote` from `start`: each further token right after a run of whitespace, so the words
/// are the same words in the same order and nothing else has been skipped. Answers the end offset.
fn walk(data: &str, start: usize, quote: &str) -> Option<usize> {
    let mut at = start;
    let mut first = true;
    for token in quote.split_whitespace() {
        if first {
            first = false;
            at = start + token.len();
            continue;
        }
        let gap = &data[at..];
        let skip = gap.len() - gap.trim_start().len();
        if skip == 0 {
            return None;
        }
        let found = at + skip;
        if !starts_ci(&data[found..], token) {
            return None;
        }
        at = found + token.len();
    }
    Some(at)
}

/// Does `hay` start with `needle`, ASCII case taken as equal?
fn starts_ci(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.len() <= h.len() && h[..n.len()].eq_ignore_ascii_case(n)
}

/// `needle` in `hay` at or after `from`, ASCII case taken as equal: byte offsets stay offsets.
fn find_ascii_ci(hay: &str, from: usize, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || from >= h.len() || n.len() > h.len() - from {
        return None;
    }
    (from..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
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
        if find(data, 0, q).is_none() {
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
    // An empty reply is not noise to throw away: a model that says nothing said nothing at all,
    // which is what a prompt past its window looks like from here. Naming it is what lets a person
    // find that cause instead of reading a void rate.
    if reply.trim().is_empty() {
        let size = match shown.prompt_bytes {
            0 => String::new(),
            n => format!(" of {n} bytes"),
        };
        return log.reject(
            "reply",
            format!(
                "the reply was empty: the prompt{size} was very likely longer than the model's \
                 window, so nothing came back at all"
            ),
        );
    }
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
            if !shown.spans.is_empty() || shown.strict {
                if !r.promise.is_empty()
                    && shown
                        .authored_span(&r.promise, |s| shown.in_turn(s))
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
            // `none` drops the held regex nudge, so under `strict` it needs the operator's or the
            // assistant's words as evidence. A tool output saying "report none" is no evidence.
            let reminder = match reminder {
                Reminder::None if shown.strict => {
                    let by = |a: &Author| matches!(a, Author::Operator | Author::Assistant);
                    if !r.evidence.is_empty() && shown.authored(&r.evidence, by).is_some() {
                        log.pass("none evidence");
                        Reminder::None
                    } else {
                        log.0.push(
                            "narrow none evidence: not the operator's or the assistant's words; none is unsure"
                                .into(),
                        );
                        Reminder::Unsure
                    }
                }
                other => other,
            };
            // What the turn did beyond the ask. Each one needs its kind word, its sentence and the
            // line of what it was shown that backs it: a finding a reader cannot check is the
            // over-asking this whole check exists to avoid.
            let mut overreach = Vec::new();
            for o in &r.overreach {
                let Some(kind) = super::overreach::Kind::parse(&o.kind) else {
                    return log.reject(
                        "overreach",
                        format!(
                            "`{}` is not one of {}",
                            o.kind,
                            super::overreach::Kind::ALL
                                .iter()
                                .map(|k| k.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                };
                if o.what.trim().is_empty() || o.evidence.trim().is_empty() {
                    return log.reject(
                        "overreach",
                        format!("`{}` needs a sentence and the line that backs it", o.kind),
                    );
                }
                overreach.push(super::overreach::Finding {
                    what: o.what.clone(),
                    evidence: o.evidence.clone(),
                    kind,
                });
            }
            log.pass("overreach schema");
            if let Err(why) = verify(
                &shown.data,
                r.overreach
                    .iter()
                    .map(|o| ("overreach", o.evidence.as_str())),
            ) {
                return log.reject("overreach quotes", why);
            }
            log.pass("overreach quotes");
            if overreach.len() > super::overreach::MAX_FINDINGS {
                log.0.push(format!(
                    "narrow overreach: {} findings, the most is {}",
                    overreach.len(),
                    super::overreach::MAX_FINDINGS
                ));
                overreach.truncate(super::overreach::MAX_FINDINGS);
            }
            log.done(Verdict::TurnEnd {
                reminder,
                note,
                overreach,
            })
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
            // An offer acts, so under `strict` its need is shown in the operator's or the
            // assistant's words, never in a tool's output or another agent's line.
            if shown.strict {
                for o in &r.offers {
                    let by = |a: &Author| matches!(a, Author::Operator | Author::Assistant);
                    if shown.authored(&o.quote, by).is_none() {
                        return log.reject(
                            "offer author",
                            format!(
                                "`{}` quote {:?} is neither the operator's nor the assistant's words",
                                o.name, o.quote
                            ),
                        );
                    }
                }
                log.pass("offer author");
            }
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
                strict: shown.strict,
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
        Job::Permission if shown.block => {
            // Automode's question. Nothing here is a quote: the reply names a category from a
            // closed set and says in one sentence what the call would do, and a category this
            // build does not know voids the answer rather than widening the block.
            let r: BlockReply = match serde_json::from_str(json) {
                Ok(r) => r,
                Err(e) => return log.reject("schema", e.to_string()),
            };
            log.pass("schema");
            if !r.block {
                return log.done(Verdict::Permission {
                    decision: Decision::Ask,
                    evidence: r.reason,
                });
            }
            let Some(category) = Category::parse(&r.category) else {
                return log.reject(
                    "category",
                    format!(
                        "a block names `{}`, and the categories are {}",
                        r.category,
                        Category::ALL
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            };
            if r.reason.trim().len() < MIN_QUOTE {
                return log.reject(
                    "reason",
                    "a block says what the call would do, in a sentence".into(),
                );
            }
            log.pass("category");
            let evidence = format!("{}: {}", category.as_str(), r.reason.trim());
            log.done(Verdict::Permission {
                decision: Decision::Block {
                    category,
                    reason: r.reason.trim().to_string(),
                },
                evidence,
            })
        }
        Job::Permission => {
            let r: PermissionReply = match serde_json::from_str(json) {
                Ok(r) => r,
                Err(e) => return log.reject("schema", e.to_string()),
            };
            let allow = match r.verdict.as_str() {
                "allow" if shown.oracle && r.cites.is_empty() => {
                    return log.reject(
                        "verdict",
                        "`allow` needs the trail numbers of the operator's words".into(),
                    );
                }
                "allow" if !shown.oracle && r.rule.is_empty() => {
                    return log.reject("verdict", "`allow` needs the rule it relied on".into());
                }
                "allow" => true,
                "ask" => false,
                other => return log.reject("verdict", format!("`{other}` is not allow or ask")),
            };
            log.pass("schema");
            // An oracle brief lists no rules, so a quoted rule there is no evidence and is dropped.
            let rule = if shown.oracle { "" } else { r.rule.as_str() };
            if let Err(why) = verify(
                &shown.data,
                [("rule", rule), ("evidence", r.evidence.as_str())],
            ) {
                return log.reject("quotes", why);
            }
            log.pass("quotes");
            let mut decision = if allow {
                Decision::Allow {
                    rule: if shown.oracle { String::new() } else { r.rule },
                    cites: r.cites,
                }
            } else {
                Decision::Ask
            };
            // Under `strict` the rule is one the harness listed, not a line the history holds.
            if let Decision::Allow { rule, .. } = &decision
                && !shown.oracle
                && shown.strict
                && !shown.rules.iter().any(|r| r.contains(rule.as_str()))
            {
                log.0.push(format!(
                    "narrow rule: {rule:?} is not a listed rule; allow is ask"
                ));
                decision = Decision::Ask;
            }
            // An allow still meets the denylist, whatever the transcript said. Blocked is ask.
            if matches!(decision, Decision::Allow { .. }) && shown.oracle {
                log.pass("denylist: read by the stage-1 gate before the brief");
            } else if matches!(decision, Decision::Allow { .. }) {
                // The annotator's allow is the old question's, and it meets the whole denylist: a
                // push is nobody's allow here - the publish stage is where a push is judged.
                match shown
                    .call
                    .as_ref()
                    .map(|c| guard::allow_passes(c, super::guard::Pushes::Refused))
                {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn blocking() -> Shown {
        Shown {
            data: "call: bash git push --force origin main".into(),
            oracle: true,
            block: true,
            ..Shown::default()
        }
    }

    fn read(reply: &str) -> Result<Verdict, String> {
        check(Job::Permission, reply, &blocking()).verdict
    }

    /// A force-push is one of the six, and the category travels with the reason.
    #[test]
    fn a_force_push_reply_is_a_block_with_its_category() {
        let Ok(Verdict::Permission {
            decision: Decision::Block { category, reason },
            ..
        }) = read(
            r#"{"block":true,"category":"push_to_a_protected_ref","reason":"it force-pushes over main, which nobody here can undo"}"#,
        )
        else {
            panic!("a force-push must read as a block");
        };
        assert_eq!(category, Category::PushToAProtectedRef);
        assert!(reason.contains("force-pushes"), "{reason}");
    }

    /// The default is no: a reply that does not block is not an allow with a rule, and nothing in
    /// it is checked as a quote.
    #[test]
    fn a_reply_that_does_not_block_is_not_a_block() {
        let Ok(Verdict::Permission {
            decision: Decision::Ask,
            ..
        }) = read(r#"{"block":false,"category":"","reason":""}"#)
        else {
            panic!("the default is no");
        };
    }

    /// A category this build does not know voids the answer rather than widening what it blocks.
    #[test]
    fn an_unknown_category_voids_the_answer() {
        let why = read(
            r#"{"block":true,"category":"i_do_not_like_it","reason":"it looked odd to me somehow"}"#,
        )
        .expect_err("an unknown category is no category");
        assert!(why.starts_with("category:"), "{why}");
    }

    /// A block says what the call would do: an empty reason is no answer.
    #[test]
    fn a_block_with_no_reason_is_voided() {
        let why = read(r#"{"block":true,"category":"another_sessions_property","reason":""}"#)
            .expect_err("a block needs its sentence");
        assert!(why.starts_with("reason:"), "{why}");
    }

    /// The prompt and the closed set have to name the same six words, or a category the model is
    /// told about is one the checker voids.
    #[test]
    fn the_block_prompt_names_every_category() {
        let prompt = crate::shadow::prompts::PERMISSION_BLOCK;
        for c in Category::ALL {
            assert!(
                prompt.contains(c.as_str()),
                "the prompt does not name `{}`",
                c.as_str()
            );
        }
        assert!(
            prompt.contains("\"block\""),
            "the reply shape is in the prompt"
        );
    }
}
