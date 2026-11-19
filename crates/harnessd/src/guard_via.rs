//! `[gatekeeper] via`: which guard answers a card when the endpoint and the shadow both could.
//!
//! The endpoint is the box's local guard (`[gatekeeper] endpoint`, a llama.cpp `/completion`). The
//! shadow is the permission job on a model the operator configured, which may be a provider. A box
//! whose GPU is down loses the first and keeps the second, so the default with both configured is
//! the endpoint first and the shadow when the endpoint does not answer at all.

use std::sync::{Arc, Mutex};

use letibot_tools::adjudicate::{
    AdjudicationDecision, AdjudicationRequest, Adjudicator, ModelAdvice,
};
use letibot_tools::authorise::UnsureKind;

/// The values `[gatekeeper] via` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Via {
    /// The endpoint alone, exactly as it answered before this existed.
    Endpoint,
    /// The shadow alone: a box with no local guard, and a provider doing the judging.
    Shadow,
    /// The endpoint first, the shadow when the endpoint does not answer.
    #[default]
    Both,
}

impl Via {
    pub const ALL: [Via; 3] = [Via::Endpoint, Via::Shadow, Via::Both];

    pub fn as_str(self) -> &'static str {
        match self {
            Via::Endpoint => "endpoint",
            Via::Shadow => "shadow",
            Via::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<Via> {
        let s = s.trim().trim_matches(['"', '\'']);
        Via::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// What `via` is when the file says nothing: whatever is configured, and `both` when both are.
    /// Neither configured is `both` too - the arm that finds nothing to ask is the same.
    pub fn default_for(endpoint: bool, shadow: bool) -> Via {
        match (endpoint, shadow) {
            (true, true) => Via::Both,
            (false, true) => Via::Shadow,
            _ => Via::Endpoint,
        }
    }
}

/// Which arm answered the last call, so the corpus row carries that arm's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Endpoint,
    Shadow,
}

/// The endpoint first, and the shadow when the endpoint did not answer at all.
pub struct EndpointThenShadow {
    endpoint: Arc<dyn Adjudicator>,
    shadow: Arc<dyn Adjudicator>,
    last: Mutex<Option<Arm>>,
}

impl EndpointThenShadow {
    pub fn new(endpoint: Arc<dyn Adjudicator>, shadow: Arc<dyn Adjudicator>) -> Self {
        EndpointThenShadow {
            endpoint,
            shadow,
            last: Mutex::new(None),
        }
    }

    fn answered(&self) -> Option<&dyn Adjudicator> {
        let which = (*self.last.lock().unwrap_or_else(|e| e.into_inner()))?;
        Some(match which {
            Arm::Endpoint => self.endpoint.as_ref(),
            Arm::Shadow => self.shadow.as_ref(),
        })
    }

    fn set(&self, arm: Arm) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(arm);
    }
}

/// The endpoint was asked and nothing came back: a refused connection, a timeout, or a body this
/// build could not read at all. A verdict, an `ask` and a trail that was never collected are all
/// answers, and the shadow is not consulted over them.
fn silent(endpoint: &dyn Adjudicator) -> bool {
    let Some(advice) = endpoint.last_advice() else {
        return false;
    };
    advice.consulted
        && advice.unsure == Some(UnsureKind::Unreadable)
        && endpoint.last_reply().is_none()
}

impl Adjudicator for EndpointThenShadow {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        let d = self.endpoint.decide(req);
        if !silent(self.endpoint.as_ref()) {
            self.set(Arm::Endpoint);
            return d;
        }
        self.set(Arm::Shadow);
        self.shadow.decide(req)
    }

    fn describe(&self) -> String {
        format!(
            "{} first, and {} when it does not answer",
            self.endpoint.describe(),
            self.shadow.describe()
        )
    }

    fn last_brief(&self) -> Option<String> {
        self.answered()?.last_brief()
    }

    fn last_reply(&self) -> Option<String> {
        self.answered()?.last_reply()
    }

    fn last_advice(&self) -> Option<ModelAdvice> {
        self.answered()?.last_advice()
    }
}

#[cfg(test)]
pub(crate) mod tests;
