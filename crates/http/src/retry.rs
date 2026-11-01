//! **What a failed provider call is worth** — the tree's one answer to *what is retryable, and
//! for how long*.
//!
//! # Why this is a module and not a predicate in the daemon
//!
//! The rule existed, and it was spread over four private functions in `harnessd`
//! (`http_retry_after`, `model_refused`, `provider_refusal`, `refused_for_want_of_credit`) plus
//! the loop that called them. Each answered a slightly different question about the same failure,
//! and the questions drifted: a `400` was retried six times because the rule was *"every code
//! except unauthenticated"*; a `429` naming money was retried six times because the status says
//! *not yet* and only the body says *not ever*; a `408` was retried but was not allowed to move a
//! session. Every one of those was found by an operator watching the same turn being taken again.
//!
//! So the rule lives here, in the leaf crate that owns [`HttpError`], where every caller can reach
//! it without a cycle: the turn engine already turns its backend's failures into this type, and
//! the daemon already matches on it.
//!
//! # The three classes
//!
//! * [`Class::Transport`] — **no answer was ever reached.** DNS resolution, a connection refused
//!   or reset, a connect or read timeout, an answer that stopped arriving half way. Nothing was
//!   recorded and nothing was refused, so the same bytes may be sent again; and a host that could
//!   not be resolved this second may resolve the next. MEASURED 2026-10-01: a resolver's
//!   `EAI_AGAIN` under memory pressure ended a turn as `malformed http response: provider
//!   unreachable: … Temporary failure in name resolution` while the local server was answering
//!   `/health` with 200.
//! * [`Class::TryAgain`] — **the provider answered, and said *not now*.** [`TRY_AGAIN`] is the
//!   whole list: `408` (it waited too long for us), `429` (not yet — unless the body says the
//!   account cannot pay, which is [`Policy::states_a_refusal`]), and `500`, `502`, `503`, `504`.
//!   This is the class the ladder exists for: llama.cpp reloading a six-shard GGUF after
//!   `--sleep-idle-seconds` answers `503 Loading model` for as long as it takes.
//! * [`Class::Refused`] — **the provider answered, and said *no*.** `400`, `401`, `403`, `404`,
//!   every other status, and a `429` whose body names money. The retry's premise is that *it sends
//!   exactly the bytes this one did* — which is what makes it safe, and equally what makes it
//!   pointless when the complaint is about those bytes. A refusal is refused identically on every
//!   attempt.
//!
//! A status outside [`TRY_AGAIN`] is deliberately `Refused`, including the 5xx the list leaves
//! out (`501 Not Implemented`, `505`): widening it to `code >= 500` is how the operator came to
//! watch six identical attempts on a permanent answer, and a code nobody has argued is transient
//! does not get to spend their attention. Adding one is a line in that table.
//!
//! # The ladder
//!
//! [`Policy::standard`] retries five times, doubling from a second: waits of **1, 2, 4, 8, 16**
//! seconds, so **six calls and 31 seconds of waiting** in the worst case.
//!
//! * **31 seconds because that is the longest legitimate silence this ladder exists for** — a
//!   llama.cpp cold reload was measured at 29–31 s on this box, and it announces itself as a
//!   `503`. A ladder shorter than that would fail the case the ladder was written for.
//! * **Half of what it was.** The old rule retried six times doubling to 32 s — 63 seconds —
//!   because the count was also carrying the *classification*: with a rule that could not tell a
//!   permanent refusal from a transient one, patience was the only thing standing between an
//!   operator and a silent failure. Now the class decides first, and the count is only about
//!   weather.
//! * **Bounded, not endless.** A caller that already knows the endpoint is not there sets zero
//!   retries (`--http-retries 0`) and is told at once; the tests that point a daemon at a dead
//!   port do exactly that.
//!
//! # The sentence
//!
//! Every ending says what was tried, how many times and for how long — [`Ladder::ending`]. After
//! the ladder: *"tried 6 times over 31s, then gave up: name resolution failed. The last thing it
//! said: http io: …"*. A refusal: *"http 401: invalid api key — the provider refused it, and a
//! refusal is not retried (1 call, no waiting)"*. The count is the thing an operator cannot get
//! any other way: *nothing was recorded* does not say whether the daemon tried once or six times.

use std::time::Duration;

use crate::HttpError;

/// **The statuses a provider uses to say *not now*, and the whole of the list.**
///
/// `408` is here with the 5xx and the `429` for the same reason: the server is saying the request
/// did not get through *this time*, and nothing about it was refused. A `429` whose body names
/// money is the exception, and it is [`Policy::states_a_refusal`]'s job rather than this table's —
/// the status genuinely means both things.
pub const TRY_AGAIN: &[u16] = &[408, 429, 500, 502, 503, 504];

/// **The default number of retries** — calls after the first. See [`Policy::standard`].
pub const STANDARD_RETRIES: u32 = 5;

/// What one failure is, in the only terms a retry can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// No answer was ever reached: the transport failed.
    Transport,
    /// The provider answered, and said *not now*.
    TryAgain,
    /// The provider answered, and said *no*.
    Refused,
}

impl Class {
    /// Is this worth taking the call again at all?
    pub fn retryable(self) -> bool {
        !matches!(self, Class::Refused)
    }

    /// The class in the words a sentence reads with — `transport`, `try again`, `refused`.
    pub fn name(self) -> &'static str {
        match self {
            Class::Transport => "transport",
            Class::TryAgain => "try again",
            Class::Refused => "refused",
        }
    }
}

/// **The rule: how many times, how far apart, and which failures.**
///
/// Cheap to copy — the daemon builds one per turn from `Config::http_retries`, and a test builds
/// [`Policy::none`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    retries: u32,
}

impl Policy {
    /// `retries` calls after the first, waiting 1 s, 2 s, 4 s… between them. `0` is none, and the
    /// first failure is the answer.
    pub const fn new(retries: u32) -> Self {
        Policy { retries }
    }

    /// **No retry at all** — a caller that already knows the endpoint is not there.
    pub const fn none() -> Self {
        Policy::new(0)
    }

    /// **The daemon's default**: [`STANDARD_RETRIES`], doubling from a second. See the module
    /// docs for why 31 seconds and why half of what it was.
    pub const fn standard() -> Self {
        Policy::new(STANDARD_RETRIES)
    }

    pub const fn retries(&self) -> u32 {
        self.retries
    }

    /// **Which of the three classes this failure is in.**
    pub fn class(&self, e: &HttpError) -> Class {
        match e {
            HttpError::Io(_) | HttpError::Malformed(_) => Class::Transport,
            HttpError::Status { code, body } => {
                if Self::states_a_refusal(body) {
                    // A 429 (or a 400) whose body says the account cannot pay. The status is not
                    // the instrument here: the same 429 means both *not yet* and *not ever until
                    // somebody tops up*, and only the body knows which.
                    Class::Refused
                } else if TRY_AGAIN.contains(code) {
                    Class::TryAgain
                } else {
                    Class::Refused
                }
            }
        }
    }

    /// **Is this worth taking the call again, and how long to wait first?**
    ///
    /// `failed` is how many calls have already failed — `0` on the first. `None` means *no*: the
    /// class refused it, or the ladder is spent.
    pub fn after(&self, e: &HttpError, failed: u32) -> Option<Duration> {
        if failed >= self.retries || !self.class(e).retryable() {
            return None;
        }
        Some(Duration::from_secs(1u64 << failed.min(31)))
    }

    /// **Did the MODEL refuse, rather than the wire failing?**
    ///
    /// The second question about one failure, and deliberately not the first: *is this worth
    /// waiting for* includes a connection that was refused and an answer that did not arrive
    /// whole, because the endpoint may be restarting and the model behind it may be well. *Is the
    /// model itself out* is a `429` or a 5xx and nothing else — a `408` is the server waiting too
    /// long for US, which is the route rather than a statement about the model, and a transport
    /// failure says nothing about it either.
    ///
    /// Read by the daemon's `[fallback]`, which is the one thing that MOVES a session.
    pub fn the_model_refused(e: &HttpError) -> bool {
        matches!(e, HttpError::Status { code, .. } if *code == 429 || *code >= 500)
    }

    /// **The status, when the provider refused what was SENT** — `4xx` except the two that are
    /// about timing.
    ///
    /// The third question: *is this evidence about the model?* A `400`/`401`/`403`/`404` is a
    /// request a model read and said no to, so it is the one class a `switch_back_notice` may
    /// cite; a `429` waits, a 5xx is an unwell endpoint, and a transport failure is neither.
    pub fn refusal_of_what_was_sent(e: &HttpError) -> Option<u16> {
        match e {
            HttpError::Status { code, .. }
                if (400..500).contains(code) && *code != 429 && *code != 408 =>
            {
                Some(*code)
            }
            _ => None,
        }
    }

    /// **A refusal no retry can lift: the account behind the key cannot pay for the call.**
    ///
    /// Narrow on purpose: only phrases that name MONEY. A bare `quota` is left out deliberately —
    /// *rate* quota is exactly the transient thing this ladder exists for, and a list that
    /// swallowed it would stop retrying the case the `429` exception was written around.
    ///
    /// The list has been extended twice by measurement, and both times the shape that was missing
    /// was the one the report used: *out of credit* (the wording `TODO.md` filed the bug with) and
    /// *credit balance* (Anthropic's, which arrives as a 429 as well as a 400). A phrase list is
    /// where a variant goes missing, which is why the sentence a refusal ends with names the
    /// class rather than trusting the list to be complete.
    pub fn states_a_refusal(body: &str) -> bool {
        const FILLED: &[&str] = &[
            "insufficient balance",
            "insufficient_quota",
            "insufficient funds",
            "insufficient credit",
            "out of credit",
            "credit balance",
            "please recharge",
            "no available resource package",
            "billing",
            // The provider's own words, which are what actually arrived: a reader who cannot
            // decode them would otherwise be told only that the endpoint "did not answer".
            "余额不足",
            "请充值",
            "资源包",
        ];
        let b = body.to_ascii_lowercase();
        FILLED.iter().any(|p| b.contains(p))
    }

    /// **What failed, in the few words the ending sentence needs.**
    ///
    /// `name resolution failed` is the one that pays for this function: the operator's report was
    /// `http io: provider unreachable: io: failed to lookup address information: Temporary failure
    /// in name resolution`, and the first four words of that are the whole story.
    pub fn what(e: &HttpError) -> &'static str {
        match e {
            HttpError::Io(err) => {
                let said = err.to_string().to_ascii_lowercase();
                if said.contains("lookup address")
                    || said.contains("name resolution")
                    || said.contains("nodename nor servname")
                    || said.contains("dns")
                {
                    "name resolution failed"
                } else {
                    match err.kind() {
                        std::io::ErrorKind::ConnectionRefused => "the connection was refused",
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
                            "the connection was reset"
                        }
                        std::io::ErrorKind::TimedOut => "the connection timed out",
                        std::io::ErrorKind::HostUnreachable
                        | std::io::ErrorKind::NetworkUnreachable => "the host could not be reached",
                        _ => "the connection failed",
                    }
                }
            }
            // A 2xx whose body did not arrive whole — the likeliest cause is a server going down
            // mid-answer, which is the same story as a reset.
            HttpError::Malformed(_) => "the answer did not arrive whole",
            HttpError::Status { code, body } => {
                if Self::states_a_refusal(body) {
                    "the account behind the key cannot pay for the call"
                } else if TRY_AGAIN.contains(code) {
                    "the provider said to try again"
                } else {
                    "the provider refused it"
                }
            }
        }
    }
}

/// **One run of the ladder, and the sentence it ends with.**
///
/// The daemon's retry loop keeps one of these per turn and asks it after every failed call; the
/// count and the waiting it accumulates are what [`Ladder::ending`] says when the turn finally
/// ends. Nothing else records them, which is why the ending is built here rather than at the
/// failure site: `(nothing was recorded)` on a head cannot say whether the daemon tried once or
/// six times.
#[derive(Debug, Clone)]
pub struct Ladder {
    policy: Policy,
    failed: u32,
    waited: Duration,
}

impl Ladder {
    pub fn new(policy: Policy) -> Self {
        Ladder {
            policy,
            failed: 0,
            waited: Duration::ZERO,
        }
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// **One call failed.** Returns the wait before the next one, or `None` when the ladder has
    /// nothing left to offer — which is the answer for a refusal on the very first call.
    ///
    /// The wait is added to what this ladder will report as having waited, because the caller
    /// sleeps it in the next line and there is no path where it does not.
    pub fn failed(&mut self, e: &HttpError) -> Option<Duration> {
        let wait = self.policy.after(e, self.failed);
        self.failed += 1;
        if let Some(w) = wait {
            self.waited += w;
        }
        wait
    }

    /// How many calls have failed so far.
    pub fn attempts(&self) -> u32 {
        self.failed
    }

    /// How long the waits before them add up to.
    pub fn waited(&self) -> Duration {
        self.waited
    }

    /// **What was tried and how many times** — the sentence the ending carries.
    ///
    /// Three shapes, and each says the one thing the failure alone cannot:
    ///
    /// * the provider refused it — *`http 401: …` — the provider refused it, and a refusal is not
    ///   retried (1 call, no waiting)*;
    /// * the ladder is switched off — the same, saying so;
    /// * the ladder ran out — *tried 6 times over 31s, then gave up: name resolution failed. The
    ///   last thing it said: http io: …*.
    pub fn ending(&self, e: &HttpError) -> String {
        let what = Policy::what(e);
        if !self.policy.class(e).retryable() {
            return format!(
                "{e} — {what}, and a refusal is not retried ({} call{}, no waiting)",
                self.failed.max(1),
                plural(self.failed.max(1)),
            );
        }
        if self.policy.retries() == 0 {
            return format!("{e} — {what}, and this daemon retries nothing (`--http-retries 0`)");
        }
        if self.failed <= 1 {
            return format!("{e} — {what}, and the ladder stopped after one call");
        }
        format!(
            "tried {} times over {}, then gave up: {what}. The last thing it said: {e}",
            self.failed,
            span(self.waited),
        )
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `0s`, `31s`, `2m 03s` — the same words `harnessd`'s outage row uses, for the same reason.
fn span(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16) -> HttpError {
        HttpError::Status {
            code,
            body: String::new(),
        }
    }

    fn said(code: u16, body: &str) -> HttpError {
        HttpError::Status {
            code,
            body: body.to_string(),
        }
    }

    fn dns() -> HttpError {
        HttpError::Io(std::io::Error::other(
            "provider unreachable: io: failed to lookup address information: Temporary failure \
             in name resolution",
        ))
    }

    fn refused() -> HttpError {
        HttpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "no",
        ))
    }

    /// **The three classes, over the statuses the brief names.**
    #[test]
    fn the_classes_are_the_statuses_the_policy_states() {
        let p = Policy::standard();
        // Retryable: the provider's own "try again", and every transport failure.
        for code in [429, 500, 502, 503, 504] {
            assert_eq!(p.class(&status(code)), Class::TryAgain, "{code}");
        }
        assert_eq!(p.class(&dns()), Class::Transport);
        assert_eq!(p.class(&refused()), Class::Transport);
        assert_eq!(
            p.class(&HttpError::Malformed("truncated".into())),
            Class::Transport
        );
        // Not retryable: what was sent, was refused.
        for code in [400, 401, 403, 404, 422] {
            assert_eq!(p.class(&status(code)), Class::Refused, "{code}");
        }
        // A 5xx the table leaves out is a refusal, not a bet on the operator's patience.
        assert_eq!(p.class(&status(501)), Class::Refused);
    }

    /// **`408` waits and does not move a session** — the one that looks like a 5xx and is not.
    #[test]
    fn a_server_that_waited_too_long_for_us_is_waited_for() {
        let p = Policy::standard();
        assert_eq!(p.class(&status(408)), Class::TryAgain);
        assert!(p.after(&status(408), 0).is_some());
        assert!(
            !Policy::the_model_refused(&status(408)),
            "a 408 is the route, not a statement about the model"
        );
    }

    /// **The six-times 429**: a refusal that names money is one call, and a rate limit is not.
    #[test]
    fn a_429_that_names_money_is_refused_and_a_rate_limit_is_not() {
        let p = Policy::standard();
        for body in [
            "http 429: 余额不足或无可用资源包,请充值。",
            r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#,
            "http 429: you are out of credits",
            r#"{"error":{"message":"Your credit balance is too low to access the API"}}"#,
        ] {
            let e = said(429, body);
            assert_eq!(p.class(&e), Class::Refused, "{body}");
            assert!(
                p.after(&e, 0).is_none(),
                "a billing refusal was retried: {body}"
            );
            let mut ladder = Ladder::new(p);
            assert!(ladder.failed(&e).is_none());
            let ending = ladder.ending(&e);
            assert!(ending.contains("1 call, no waiting"), "{ending}");
            assert!(
                ending.contains("cannot pay"),
                "the ending says what the body said: {ending}"
            );
        }
        // The case the 429 exception exists for: requests-per-minute, which is transient by
        // construction.
        let limit = said(
            429,
            r#"{"error":{"type":"requests","message":"Rate limit reached"}}"#,
        );
        assert_eq!(p.class(&limit), Class::TryAgain);
        assert!(p.after(&limit, 0).is_some());
    }

    /// **The ladder's numbers**, and the cap is a cap.
    #[test]
    fn the_ladder_doubles_from_a_second_and_runs_out() {
        let p = Policy::standard();
        let secs: Vec<u64> = (0..p.retries())
            .map(|n| p.after(&status(503), n).expect("retryable").as_secs())
            .collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16]);
        assert_eq!(secs.iter().sum::<u64>(), 31, "31 s of waiting, worst case");
        assert!(
            p.after(&status(503), p.retries()).is_none(),
            "the cap is a cap: {} calls and no more",
            p.retries() + 1
        );
        // `0` is none, and `1` is one — the field is named for RETRIES, and the arithmetic has
        // to agree with the name.
        assert!(Policy::none().after(&status(503), 0).is_none());
        assert!(Policy::new(1).after(&status(503), 0).is_some());
        assert!(Policy::new(1).after(&status(503), 1).is_none());
    }

    /// **The budget is the caller's, and `0` means the first failure is the answer.**
    ///
    /// The ladder is right for a server that might be reloading and wrong for one that is not
    /// there, and only the caller knows which. The premise is asserted first: without it this
    /// passes on a build that stopped retrying altogether.
    #[test]
    fn one_attempt_means_no_retry_and_the_default_still_retries() {
        let e = refused();
        assert!(
            Policy::standard().after(&e, 0).is_some(),
            "the premise: a refused connection is retryable under the default"
        );
        assert!(
            Policy::none().after(&e, 0).is_none(),
            "no retries means the first failure is the answer"
        );
        let mut ladder = Ladder::new(Policy::none());
        assert!(ladder.failed(&e).is_none());
        assert_eq!(ladder.attempts(), 1, "one call, and no waiting after it");
        assert_eq!(ladder.waited(), Duration::ZERO);
        // **A budget in between is honoured as itself**, so this is a number and not a boolean
        // wearing one: the field is named for RETRIES, and `1` is one call after the first.
        assert!(Policy::new(1).after(&e, 0).is_some(), "one retry is one");
        assert!(Policy::new(1).after(&e, 1).is_none(), "and only one");
        assert!(Policy::new(3).after(&e, 2).is_some());
        assert!(Policy::new(3).after(&e, 3).is_none());
    }

    /// **A turn that never reached the network is tried before it is called a failure** — the
    /// report this module opens with.
    #[test]
    fn a_name_that_did_not_resolve_is_tried_again() {
        let p = Policy::standard();
        assert_eq!(Policy::what(&dns()), "name resolution failed");
        assert!(p.after(&dns(), 0).is_some());
        assert_eq!(Policy::what(&refused()), "the connection was refused");
        assert!(
            Policy::what(&status(503)).contains("try again"),
            "{}",
            Policy::what(&status(503))
        );
    }

    /// **The ending sentence, in the two shapes the operator reads.**
    #[test]
    fn the_ending_names_the_attempts_or_says_there_were_none() {
        // A ladder that ran out.
        let p = Policy::standard();
        let mut ladder = Ladder::new(p);
        for n in 0..=p.retries() {
            let wait = ladder.failed(&dns());
            assert_eq!(wait, p.after(&dns(), n), "attempt {n}");
        }
        assert_eq!(ladder.attempts(), 6);
        assert_eq!(ladder.waited(), Duration::from_secs(31));
        let ending = ladder.ending(&dns());
        assert!(ending.starts_with("tried 6 times over 31s, then gave up: name resolution failed"));
        assert!(
            ending.contains("Temporary failure in name resolution"),
            "and the provider's own words, for the bug report: {ending}"
        );

        // A refusal: no attempts at all, and the status reads as itself.
        let mut ladder = Ladder::new(p);
        let e = said(401, "invalid api key");
        assert!(ladder.failed(&e).is_none());
        let ending = ladder.ending(&e);
        assert!(ending.starts_with("http 401: invalid api key"), "{ending}");
        assert!(
            ending.contains("a refusal is not retried (1 call, no waiting)"),
            "{ending}"
        );

        // And a ladder switched off says so rather than claiming it gave up.
        let mut ladder = Ladder::new(Policy::none());
        assert!(ladder.failed(&dns()).is_none());
        let ending = ladder.ending(&dns());
        assert!(ending.contains("--http-retries 0"), "{ending}");
    }
}
