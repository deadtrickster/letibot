//! What a failed provider call is worth: the tree's one answer to what is retryable,
//! and for how long. The rule was spread over four private functions in the daemon,
//! each answering a slightly different question about the same failure, and the
//! questions drifted - so it lives here, in the leaf crate that owns [`HttpError`].
//!
//! Three classes: `Transport` (no answer reached, so the same bytes may be sent
//! again), `TryAgain` (the provider saying not now, which the ladder exists for) and
//! `Refused` (the provider saying no, and a retry would be refused identically).

use std::time::Duration;

use crate::HttpError;

/// The statuses a provider uses to say not now, and the whole of the list. `408` is
/// here with the 5xx and the `429` because the server is saying the request did not
/// get through this time; a `429` naming money is [`Policy::states_a_refusal`]'s.
pub const TRY_AGAIN: &[u16] = &[408, 429, 500, 502, 503, 504];

/// **The default number of retries** - calls after the first. See [`Policy::standard`].
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

    /// The class in the words a sentence reads with - `transport`, `try again`, `refused`.
    pub fn name(self) -> &'static str {
        match self {
            Class::Transport => "transport",
            Class::TryAgain => "try again",
            Class::Refused => "refused",
        }
    }
}

/// **The four ways a model's provider is in trouble**, and the whole of the vocabulary an
/// automation branches on: a caller waits out a rate limit, gives up on a quota, retries a
/// timeout, and replaces a provider that keeps stalling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trouble {
    /// The provider said *not now* and, with luck, when: `429` (or a `408`) whose body is not about
    /// money.
    RateLimit,
    /// The account behind the key cannot pay. No wait lifts it.
    Quota,
    /// The request never answered: the transport timed out.
    Timeout,
    /// The round produced nothing for long enough to be worth saying, and the daemon said it.
    /// Raised by the caller's own clock rather than by a failure, which is why it has no `HttpError`.
    Stall,
}

impl Trouble {
    /// The word on the wire, and the one a caller matches on.
    pub fn name(self) -> &'static str {
        match self {
            Trouble::RateLimit => "rate_limit",
            Trouble::Quota => "quota",
            Trouble::Timeout => "timeout",
            Trouble::Stall => "stall",
        }
    }
}

/// The rule: how many times, how far apart, and which failures. Cheap to copy - the
/// daemon builds one per turn from `Config::http_retries`, and a test builds
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

    /// **No retry at all** - a caller that already knows the endpoint is not there.
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
            HttpError::Status { code, body, .. } => {
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

    /// Is this worth taking the call again, and how long to wait first? `failed` is
    /// how many calls have already failed - `0` on the first. `None` means no: the
    /// class refused it, or the ladder is spent.
    pub fn after(&self, e: &HttpError, failed: u32) -> Option<Duration> {
        if failed >= self.retries || !self.class(e).retryable() {
            return None;
        }
        Some(Duration::from_secs(1u64 << failed.min(31)))
    }

    /// Did the MODEL refuse, rather than the wire failing? A connection refused or a
    /// half answer is not one, because the endpoint may be restarting and the model
    /// behind it may be well. Read by the daemon's `[fallback]`, which MOVES a session.
    pub fn the_model_refused(e: &HttpError) -> bool {
        matches!(e, HttpError::Status { code, .. } if *code == 429 || *code >= 500)
    }

    /// **Which of the four troubles this failure is**, or `None` for one that is not about the
    /// provider being unavailable: a bad request, a bad key, a server that is merely broken.
    pub fn trouble(e: &HttpError) -> Option<Trouble> {
        match e {
            HttpError::Status { code, body, .. } => {
                if Self::states_a_refusal(body) {
                    // Money, whatever the status says: the provider's own words are the instrument,
                    // and a 400 naming an unpaid balance is the same fact as a 429 that does.
                    Some(Trouble::Quota)
                } else if *code == 429 || *code == 408 {
                    Some(Trouble::RateLimit)
                } else {
                    None
                }
            }
            // A transport timeout is the model's own API not answering in time. `TimedOut` is the
            // kind; the text is the fallback, because a provider wraps its own io error one layer up
            // and the kind does not always survive the trip.
            HttpError::Io(err) => {
                let said = err.to_string().to_ascii_lowercase();
                (err.kind() == std::io::ErrorKind::TimedOut
                    || said.contains("timed out")
                    || said.contains("timeout"))
                .then_some(Trouble::Timeout)
            }
            HttpError::Malformed(_) => None,
        }
    }

    /// **When the provider said its limit lifts**, Unix millis, from a clock the caller owns
    /// (`now_ms`). Only `Retry-After` is read: it is seconds, and seconds need no timezone. A
    /// provider that says it in prose has said it to a person, and `None` says nothing rather than
    /// guessing an instant in a zone this box does not know.
    pub fn reset_ms(e: &HttpError, now_ms: u64) -> Option<u64> {
        let HttpError::Status { retry_after, .. } = e else {
            return None;
        };
        retry_after.map(|secs| now_ms.saturating_add(secs.saturating_mul(1000)))
    }

    /// The status, when the provider refused what was SENT - `4xx` except the two
    /// about timing. A `400`/`401`/`403`/`404` is a request a model read and said no
    /// to, so it is the one class a `switch_back_notice` may cite.
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

    /// A refusal no retry can lift: the account behind the key cannot pay. Only
    /// phrases naming MONEY, because a bare `quota` is the transient thing the ladder
    /// exists for, and swallowing it would stop retrying the case it was written around.
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

    /// What failed, in the few words the ending sentence needs. `name resolution
    /// failed` is the one that pays for it: the operator's report was a full `io:`
    /// chain whose first four words were the whole story.
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
            // A 2xx whose body did not arrive whole - the likeliest cause is a server going down
            // mid-answer, which is the same story as a reset.
            HttpError::Malformed(_) => "the answer did not arrive whole",
            HttpError::Status { code, body, .. } => {
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

/// One run of the ladder, and the sentence it ends with. The count and the waiting
/// are what [`Ladder::ending`] says when the turn finally ends, and nothing else
/// records them, so a head cannot otherwise say whether the daemon tried once or six.
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

    /// One call failed. Returns the wait before the next one, or `None` when the
    /// ladder has nothing left - the answer for a refusal on the very first call. The
    /// wait is added to what this ladder reports, because the caller sleeps it next.
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

    /// What was tried and how many times - the sentence the ending carries. It says
    /// the one thing the failure alone cannot: whether the provider refused at once,
    /// the ladder was switched off, or it ran out after so many tries and so long.
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

/// `0s`, `31s`, `2m 03s` - the same words `harnessd`'s outage row uses, for the same reason.
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
            retry_after: None,
        }
    }

    fn said(code: u16, body: &str) -> HttpError {
        HttpError::Status {
            code,
            body: body.to_string(),
            retry_after: None,
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

    /// **The four troubles, and the failures that are none of them.** A 500 is an outage the ladder
    /// handles; a 400 is what was sent; only these four are worth waking an automation for.
    #[test]
    fn a_trouble_is_one_of_four_and_nothing_else() {
        // A rate limit names no money, and the same 429 that names money is a quota.
        assert_eq!(Policy::trouble(&status(429)), Some(Trouble::RateLimit));
        assert_eq!(Policy::trouble(&status(408)), Some(Trouble::RateLimit));
        assert_eq!(
            Policy::trouble(&said(429, "{\"error\":{\"code\":\"insufficient_quota\"}}")),
            Some(Trouble::Quota),
            "a 429 whose body says the account cannot pay is not a wait"
        );
        assert_eq!(
            Policy::trouble(&said(400, "Your credit balance is too low")),
            Some(Trouble::Quota),
            "the provider's own words decide it, not the status"
        );
        // A timeout is the model's API not answering, by kind or by the words a provider wraps it in.
        assert_eq!(
            Policy::trouble(&HttpError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timed out"
            ))),
            Some(Trouble::Timeout)
        );
        assert_eq!(
            Policy::trouble(&HttpError::Io(std::io::Error::other(
                "provider unreachable: error sending request: timed out reading response body"
            ))),
            Some(Trouble::Timeout),
            "a provider wraps its own io error, and the kind does not survive the trip"
        );
        // And everything else is not a trouble: the turn's own failure is enough for these.
        assert_eq!(Policy::trouble(&status(500)), None);
        assert_eq!(Policy::trouble(&status(400)), None);
        assert_eq!(Policy::trouble(&dns()), None);
        assert_eq!(Policy::trouble(&refused()), None);
        assert_eq!(Policy::trouble(&HttpError::Malformed("x".into())), None);
        // The words are the wire's, and they are the ones a caller branches on.
        assert_eq!(Trouble::RateLimit.name(), "rate_limit");
        assert_eq!(Trouble::Quota.name(), "quota");
        assert_eq!(Trouble::Timeout.name(), "timeout");
        assert_eq!(Trouble::Stall.name(), "stall");
    }

    /// **The reset time is the header's, or it is nothing.** A provider that says it in prose has
    /// said it to a person; an instant invented out of a wall clock in an unknown zone is worse than
    /// `None`, because a caller sleeps on it.
    #[test]
    fn the_reset_time_comes_from_retry_after_and_nowhere_else() {
        let with = HttpError::Status {
            code: 429,
            body: "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-12 15:01:48."
                .into(),
            retry_after: Some(90),
        };
        assert_eq!(Policy::reset_ms(&with, 1_000_000), Some(1_090_000));
        let prose_only = said(429, "Your limit will reset at 2026-10-12 15:01:48.");
        assert_eq!(
            Policy::reset_ms(&prose_only, 1_000_000),
            None,
            "a wall clock in the body is the operator's copy, not a deadline"
        );
        assert_eq!(Policy::reset_ms(&dns(), 1_000_000), None);
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

    /// **`408` waits and does not move a session** - the one that looks like a 5xx and is not.
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
        // `0` is none, and `1` is one - the field is named for RETRIES, and the arithmetic has
        // to agree with the name.
        assert!(Policy::none().after(&status(503), 0).is_none());
        assert!(Policy::new(1).after(&status(503), 0).is_some());
        assert!(Policy::new(1).after(&status(503), 1).is_none());
    }

    /// The budget is the caller's, and `0` means the first failure is the answer: the
    /// ladder is right for a server that might be reloading and wrong for one that is
    /// not there. The premise is asserted first so a build that stopped retrying fails.
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

    /// **A turn that never reached the network is tried before it is called a failure** - the
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
