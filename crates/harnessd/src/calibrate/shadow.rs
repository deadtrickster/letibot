//! `--shadow-arm MODEL`: the operator-answered corpus through the SHADOW's permission path, twice.
//!
//! The permission ask spends most of its bytes in two places: the system prompt (one, doing two
//! jobs, chosen by a marker inside the data) and the brief (`ModelBrief::render`, every section
//! carrying its own caveat prose). `--compare` measures the guard's brief VARIANTS; this measures
//! the shadow's ask - the prompt and the data together - against the same rows the operator
//! answered, so a change to either is a number rather than an opinion.
//!
//! Two arms, one model, one snapshot of rows:
//!
//! * `before` is [`crate::shadow::legacy`]: the marker line, the one prompt, and the brief as it
//!   rendered then.
//! * `after` is what the tree does now: the authorisation prompt, and the slim brief.
//!
//! Every number is a fact about the call the daemon would have made: the prompt is built exactly
//! as [`crate::shadow::oracle::ShadowOracle`] builds it, and the reply goes through
//! [`crate::shadow::verdict::check`], which is where an allow with no operator cite dies.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use letibot_tokencore::store::{Store, StoredAdjudication};
use letibot_tools::authorise::{DecisionExample, ModelBrief};

use crate::config::Config;
use crate::shadow::judge;
use crate::shadow::verdict::{self, Decision, Shown, Verdict};
use crate::shadow::{Job, prompts};

use super::access_by_tool;

/// How long one replayed call may take. The daemon's own permission budget is 2.5 s because it
/// sits in the tool path; a measurement has no turn behind it, and a provider's TLS round trip
/// plus a long brief is allowed the room the answer needs.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// One arm: a prompt and a brief, fixed at the commit that measured them.
#[derive(Debug, Clone, Copy)]
pub struct ShadowArm {
    pub name: &'static str,
    /// `true` for the composition the split replaced.
    pub before: bool,
}

/// The two arms, in the order they are printed.
pub const ARMS: &[ShadowArm] = &[
    ShadowArm {
        name: "before: one prompt, marker in the data, full brief",
        before: true,
    },
    ShadowArm {
        name: "after: the authorisation prompt, slim brief",
        before: false,
    },
];

/// What one arm would have done with one answered call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// An allow the operator's own words back: the only verdict that acts.
    Allow,
    /// An allow the gate would NOT have acted on - no cite naming an operator message, or no
    /// witness to widen. The card goes to the operator either way; counted apart because it is
    /// the model failing the check, not the model declining.
    Uncited,
    /// The model asked. The operator's card stands, as it did.
    Ask,
    /// The reply failed a check and was voided whole.
    Void(String),
    /// No reply: a refused connection, an auth failure, a spent budget.
    Failed(String),
}

impl Said {
    pub fn word(&self) -> &'static str {
        match self {
            Said::Allow => "allow",
            Said::Uncited => "uncited allow",
            Said::Ask => "ask",
            Said::Void(_) => "void",
            Said::Failed(_) => "failed",
        }
    }

    /// A verdict the check accepted, as opposed to a reply that never became one. Agreement is
    /// measured over these rows only: a void is no answer, and counting it as one would let a
    /// smaller prompt earn accuracy by failing to answer at all.
    pub fn is_verdict(&self) -> bool {
        matches!(self, Said::Allow | Said::Uncited | Said::Ask)
    }
}

/// One replayed call.
#[derive(Debug, Clone)]
pub struct Row {
    pub request_id: String,
    pub tool: String,
    /// What the operator did: `true` for admitted.
    pub admitted: bool,
    pub said: Said,
    /// Bytes the model was sent: the system prompt and the fenced data.
    pub chars: usize,
    pub ms: u64,
}

/// One arm's numbers.
#[derive(Debug)]
pub struct Report {
    pub arm: ShadowArm,
    pub model: String,
    pub store: String,
    pub rows: Vec<Row>,
    /// Rows the store held that this arm could not replay, and why - never silently dropped,
    /// because a sample that quietly shrank is a sample nobody can read.
    pub skipped: Vec<(String, String)>,
}

/// A median and a ninetieth percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub median: u64,
    pub p90: u64,
}

impl Spread {
    /// The median and the p90 of `v`, or zeros for an empty sample.
    pub fn of(mut v: Vec<u64>) -> Spread {
        if v.is_empty() {
            return Spread { median: 0, p90: 0 };
        }
        v.sort_unstable();
        let at = |p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
        Spread {
            median: at(0.5),
            p90: at(0.9),
        }
    }
}

impl Report {
    pub fn allows(&self) -> usize {
        self.count(|s| matches!(s, Said::Allow))
    }

    pub fn asks(&self) -> usize {
        self.count(|s| matches!(s, Said::Ask))
    }

    pub fn uncited(&self) -> usize {
        self.count(|s| matches!(s, Said::Uncited))
    }

    pub fn voids(&self) -> usize {
        self.count(|s| matches!(s, Said::Void(_)))
    }

    pub fn failed(&self) -> usize {
        self.count(|s| matches!(s, Said::Failed(_)))
    }

    fn count(&self, f: impl Fn(&Said) -> bool) -> usize {
        self.rows.iter().filter(|r| f(&r.said)).count()
    }

    /// The one fault: the shadow would have admitted what the operator refused.
    pub fn false_allows(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.said, Said::Allow) && !r.admitted)
            .count()
    }

    /// Rows where the model answered a verdict, which is the denominator of agreement.
    pub fn verdicts(&self) -> usize {
        self.rows.iter().filter(|r| r.said.is_verdict()).count()
    }

    /// Rows where the verdict matched the operator's own answer: allow on an admitted call, ask
    /// on a refused one. An `uncited` allow is the gate asking, so it agrees with a refusal.
    pub fn agreed(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| match r.said {
                Said::Allow => r.admitted,
                Said::Ask | Said::Uncited => !r.admitted,
                _ => false,
            })
            .count()
    }

    /// Void reasons by the check that refused them (`schema`, `quotes`, `verdict`), with counts,
    /// most frequent first: "the model answered nothing" and "the model answered off-schema" are
    /// different problems and one number would hide both.
    pub fn void_reasons(&self) -> Vec<(String, usize)> {
        let mut n: BTreeMap<String, usize> = BTreeMap::new();
        for r in &self.rows {
            if let Said::Void(why) = &r.said {
                let key = why.split(':').next().unwrap_or(why).trim().to_string();
                *n.entry(key).or_default() += 1;
            }
        }
        let mut v: Vec<(String, usize)> = n.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    pub fn chars(&self) -> Spread {
        Spread::of(self.rows.iter().map(|r| r.chars as u64).collect())
    }

    pub fn ms(&self) -> Spread {
        Spread::of(self.rows.iter().map(|r| r.ms).collect())
    }

    /// The arm's numbers as the lines an operator reads.
    pub fn render(&self) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let _ = writeln!(o, "  {}", self.arm.name);
        let _ = writeln!(o, "    model: {}", self.model);
        let _ = writeln!(
            o,
            "    {} of {} answered call(s): {} allow, {} ask, {} uncited allow, {} void, {} failed",
            self.rows.len(),
            self.rows.len() + self.skipped.len(),
            self.allows(),
            self.asks(),
            self.uncited(),
            self.voids(),
            self.failed()
        );
        let verdicts = self.verdicts();
        let agreed = self.agreed();
        let _ = writeln!(
            o,
            "    agreement with the operator: {agreed} of {verdicts}{}   FALSE ALLOWS: {}",
            match verdicts {
                0 => String::new(),
                n => format!(" ({:.1}%)", 100.0 * agreed as f64 / n as f64),
            },
            self.false_allows()
        );
        let reasons = self.void_reasons();
        if !reasons.is_empty() {
            let _ = writeln!(
                o,
                "    void: {}",
                reasons
                    .iter()
                    .map(|(k, n)| format!("{k} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let (c, t) = (self.chars(), self.ms());
        let _ = writeln!(
            o,
            "    prompt chars: median {}, p90 {}   latency: median {} ms, p90 {} ms",
            c.median, c.p90, t.median, t.p90
        );
        if !self.skipped.is_empty() {
            let _ = writeln!(
                o,
                "    {} row(s) could not be replayed:",
                self.skipped.len()
            );
            for (id, why) in &self.skipped {
                let _ = writeln!(o, "      {id}: {why}");
            }
        }
        o
    }
}

/// Replay `answered` through one arm, on `model`. `rows_to_replay` takes the snapshot once, so
/// two arms judge the same calls while the store is being written underneath them.
pub fn replay(
    cfg: &Config,
    store: &Path,
    answered: Vec<StoredAdjudication>,
    model: &str,
    arm: ShadowArm,
) -> Result<Report, String> {
    let db = Store::open(store).map_err(|e| format!("opening {}: {e}", store.display()))?;
    let judge = judge::build_timed(
        model,
        cfg.providers_file.as_deref(),
        &cfg.endpoint,
        DEADLINE,
    )?;
    // The brief both arms render is the one that ships, so the difference between them is the
    // composition and nothing else.
    let basis = super::ARMS[3];
    let access = access_by_tool();
    let mut report = Report {
        arm,
        model: judge.label(),
        store: store.display().to_string(),
        rows: Vec::new(),
        skipped: Vec::new(),
    };
    // Oldest first, so each row is measured against the answers that preceded it - the same order
    // `replay_rows` uses, and the reason `--compare` reads one snapshot.
    let mut answered = answered;
    answered.reverse();
    let mut before: Vec<DecisionExample> = Vec::new();
    for row in answered {
        let super::Prepared { req, .. } =
            match super::request_of(&db, &row, &mut before, basis, &access, cfg.mode) {
                Ok(r) => r,
                Err(why) => {
                    report.skipped.push((row.request_id.clone(), why));
                    continue;
                }
            };
        // The gate's own reading travels on the row a real session wrote; a row without one is a
        // different measurement (the fallback re-derives layer A) and is named, not guessed at.
        let Some(reading) = req.reading.clone() else {
            report.skipped.push((
                row.request_id.clone(),
                "the row carries no layer A reading, so no brief can be built from it".into(),
            ));
            continue;
        };
        let mut brief = ModelBrief::new(&req, &reading);
        brief.examples = req.examples.clone();
        let witness = brief.adjudicable().is_some();
        let (system, data) = match report.arm.before {
            true => (
                crate::shadow::legacy::PROMPT.to_string(),
                crate::shadow::legacy::data(&brief),
            ),
            false => (prompts::PERMISSION_ASKING.to_string(), brief.render()),
        };
        let user = prompts::fence(&data);
        let chars = system.chars().count() + user.chars().count();
        let shown = Shown {
            data,
            oracle: true,
            ..Shown::default()
        };
        let started = Instant::now();
        let reply = judge.judge(&system, &user);
        let ms = started.elapsed().as_millis() as u64;
        let said = match reply {
            Err(why) => Said::Failed(why),
            Ok(reply) => match verdict::check(Job::Permission, &reply, &shown).verdict {
                Ok(Verdict::Permission {
                    decision: Decision::Allow { cites, .. },
                    ..
                }) => {
                    // The two rules `ShadowOracle::authorised` applies before an allow acts.
                    if witness && !brief.trail.cited_operator_words(&cites).is_empty() {
                        Said::Allow
                    } else {
                        Said::Uncited
                    }
                }
                Ok(_) => Said::Ask,
                Err(why) => Said::Void(why),
            },
        };
        report.rows.push(Row {
            request_id: row.request_id.clone(),
            tool: row.tool.clone(),
            admitted: row.effect == "admit",
            said,
            chars,
            ms,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(admitted: bool, said: Said) -> Row {
        Row {
            request_id: "adj-t".into(),
            tool: "bash".into(),
            admitted,
            said,
            chars: 500,
            ms: 40,
        }
    }

    fn report(rows: Vec<Row>) -> Report {
        Report {
            arm: ARMS[1],
            model: "stub/model".into(),
            store: "fixture".into(),
            rows,
            skipped: Vec::new(),
        }
    }

    /// A percentile read off the sample, not an average dressed up as one: the ask is long-tailed
    /// (one 76k prompt) and a mean would say nothing about what a typical call costs.
    #[test]
    fn the_median_and_p90_are_read_off_the_sorted_sample() {
        assert_eq!(Spread::of(vec![]), Spread { median: 0, p90: 0 });
        assert_eq!(Spread::of(vec![7]), Spread { median: 7, p90: 7 });
        let ten: Vec<u64> = (1..=10).collect();
        let s = Spread::of(ten);
        assert_eq!(s.median, 6, "index 5 of ten is the median by this rule");
        assert_eq!(s.p90, 10, "index 9 of ten is the ninetieth");
    }

    /// Agreement is measured over the rows where a verdict came back: a void is no answer, and
    /// counting it as one would let a smaller prompt earn accuracy by failing to answer at all.
    /// The one fault is an allow on a call the operator refused.
    #[test]
    fn agreement_counts_verdicts_and_a_false_allow_is_the_one_fault() {
        let r = report(vec![
            row(true, Said::Allow),
            row(false, Said::Ask),
            row(false, Said::Uncited),
            row(true, Said::Ask),
            row(false, Said::Allow),
            row(true, Said::Void("schema: EOF while parsing a value".into())),
            row(true, Said::Failed("connection refused".into())),
        ]);
        assert_eq!(r.verdicts(), 5, "void and failed are not verdicts");
        assert_eq!(r.allows(), 2);
        assert_eq!(r.false_allows(), 1);
        assert_eq!(
            r.agreed(),
            3,
            "allow+admitted, ask+refused, uncited+refused"
        );
        assert_eq!(r.voids(), 1);
        assert_eq!(r.failed(), 1);
        // The line an operator reads names the denominator, so 3 reads as 3 of 5.
        let said = r.render();
        assert!(
            said.contains("agreement with the operator: 3 of 5"),
            "{said}"
        );
        assert!(said.contains("FALSE ALLOWS: 1"), "{said}");
    }

    /// "It answered nothing" and "it answered off-schema" are different problems: the first is
    /// the prompt, the second is the model, and one number would hide both.
    #[test]
    fn void_reasons_are_grouped_by_the_check_that_refused_them() {
        let r = report(vec![
            row(true, Said::Void("schema: EOF while parsing a value".into())),
            row(true, Said::Void("schema: expected value".into())),
            row(
                true,
                Said::Void("quotes: `rule` is not in what it was shown".into()),
            ),
            row(true, Said::Ask),
        ]);
        assert_eq!(
            r.void_reasons(),
            vec![("schema".to_string(), 2), ("quotes".to_string(), 1)]
        );
    }
}
