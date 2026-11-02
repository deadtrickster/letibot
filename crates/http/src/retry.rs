//! What a failed provider call is worth: the tree's one answer to what is retryable, and for how
//! long.
//!
//! The rule lives in this leaf crate because every caller reaches it without a cycle: the turn
//! engine already turns its backend's failures into [`HttpError`], and the daemon already matches
//! on it. A rule spread over several private predicates drifts, and the answers it gives about one
//! failure stop agreeing.
//!
//! [`Class::Transport`] means no answer was ever reached - DNS, a refused or reset connection, a
//! timeout, an answer that stopped half way - so the same bytes may be sent again.
//! [`Class::TryAgain`] means the provider answered *not now*, and [`TRY_AGAIN`] is the whole list.
//! [`Class::Refused`] means it answered *no*: the retry's premise is that it sends exactly the
//! bytes this one did, which is what makes it safe and equally what makes it pointless here.
//!
//! A status outside [`TRY_AGAIN`] is deliberately `Refused`, including the 5xx the list leaves
//! out. Widening it to `code >= 500` spends the operator's attention on a permanent answer, and a
//! code nobody has argued is transient does not get to. Adding one is a line in that table.
//!
//! [`Policy::standard`] retries five times, doubling from a second: six calls and 31 seconds of
//! waiting in the worst case. 31 because a llama.cpp cold reload is the longest legitimate
//! silence the ladder exists for, and it announces itself as a `503`: a shorter ladder would fail
//! the case it was written for. A caller that knows the endpoint is not there sets zero retries
//! and is told at once.
//!
//! Every ending says what was tried, how many times and for how long ([`Ladder::ending`]), because
//! "nothing was recorded" does not say whether the daemon tried once or six times.

use std::time::Duration;

use crate::HttpError;

/// The statuses a provider uses to say *not now*, and the whole of the list. `408` is here with the
/// 5xx and the `429` because the server is saying the request did not get through this time; a `429`
/// whose body names money is [`Policy::states_a_refusal`]'s exception.
pub const TRY_AGAIN: &[u16] = &[408, 429, 500, 502, 503, 504];

/// The default number of retries: calls after the first. See [`Policy::standard`].
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

    /// The class in the words a sentence reads with: `transport`, `try again`, `refused`.
    pub fn name(self) -> &'static str {
        match self {
            Class::Transport => "transport",
            Class::TryAgain => "try again",
            Class::Refused => "refused",
        }
    }
}

/// The rule: how many times, how far apart, and which failures. Cheap to copy, since the daemon
/// builds one per turn and a test builds [`Policy::none`].
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

    /// No retry at all, for a caller that already knows the endpoint is not there.
    pub const fn none() -> Self {
        Policy::new(0)
    }

    /// The daemon's default: [`STANDARD_RETRIES`] calls after the first, doubling from a second.
    /// See the module docs for why 31 seconds.
    pub const fn standard() -> Self {
        Policy::new(STANDARD_RETRIES)
    }

    pub const fn retries(&self) -> u32 {
        self.retries
    }

    /// Which of the three classes this failure is in.
    pub fn class(&self, e: &HttpError) -> Class {
        match e {
            HttpError::Io(_) | HttpError::Malformed(_) => Class::Transport,
            HttpError::Status { code, body } => {
                if Self::states_a_refusal(body) {
                    // A 429 or a 400 whose body says the account cannot pay. The status is not
                    // the instrument: the same 429 means both *not yet* and *not ever until
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

    /// Is this worth taking the call again, and how long to wait first? `failed` is how many calls
    /// have already failed, and `None` means no: the class refused it or the ladder is spent.
    pub fn after(&self, e: &HttpError, failed: u32) -> Option<Duration> {
        if failed >= self.retries || !self.class(e).retryable() {
            return None;
        }
        Some(Duration::from_secs(1u64 << failed.min(31)))
    }

    /// Did the MODEL refuse, rather than the wire failing? Not the same question as *is this worth
    /// waiting for*: a refused connection may mean the endpoint is restarting, while the model is only
    /// out on a `429` or a 5xx.
    pub fn the_model_refused(e: &HttpError) -> bool {
        matches!(e, HttpError::Status { code, .. } if *code == 429 || *code >= 500)
    }

    /// The status, when the provider refused what was SENT: `4xx` except the two about timing. A
    /// `400`/`401`/`403`/`404` is a request a model read and said no to, so it is the one class a
    /// switch-back notice may cite.
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

    /// A refusal no retry can lift: the account behind the key cannot pay for the call. Narrow on
    /// purpose, to phrases that name money, since a bare `quota` is the transient thing the ladder
    /// exists for.
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

    /// What failed, in the few words the ending sentence needs. `name resolution failed` is the one
    /// that pays for this: the io error behind it is a sentence about lookups.
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
            // A 2xx whose body did not arrive whole: the likeliest cause is a server going down
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

/// One run of the ladder, and the sentence it ends with. The count and the waiting are recorded here
/// rather than at the failure site because nothing else keeps them.
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

    /// One call failed: the wait before the next one, or `None` when the ladder has nothing left.
    /// The wait is added to what this ladder reports as having waited, since the caller sleeps it.
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

    /// What was tried and how many times: a refusal with its single call, a ladder switched off saying
    /// so, or one that ran out with the count, the wait and the provider's own words.
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

/// `0s`, `31s`, `2m 03s` - the same words the daemon's outage row uses, for the same reason.
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

    /// The three classes, over the statuses the policy names.
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

    /// `408` waits and does not move a session: the one that looks like a 5xx and is not.
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

    /// The six-times 429: a refusal that names money is one call, and a rate limit is not.
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

    /// The ladder's numbers, and the cap is a cap.
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
        // `0` is none, and `1` is one: the field is named for RETRIES, and the arithmetic has
        // to agree with the name.
        assert!(Policy::none().after(&status(503), 0).is_none());
        assert!(Policy::new(1).after(&status(503), 0).is_some());
        assert!(Policy::new(1).after(&status(503), 1).is_none());
    }

    /// The budget is the caller's: the ladder is right for a server that might be reloading and wrong
    /// for one that is not there, and only the caller knows which. The premise is asserted first, or
    /// this passes on a build that stopped retrying.
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
        // A budget in between is honoured as itself, so this is a number and not a boolean
        // wearing one: the field is named for RETRIES, so `1` is one call after the first.
        assert!(Policy::new(1).after(&e, 0).is_some(), "one retry is one");
        assert!(Policy::new(1).after(&e, 1).is_none(), "and only one");
        assert!(Policy::new(3).after(&e, 2).is_some());
        assert!(Policy::new(3).after(&e, 3).is_none());
    }

    /// A turn that never reached the network is tried before it is called a failure.
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

    /// The ending sentence, in the two shapes the operator reads.
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
