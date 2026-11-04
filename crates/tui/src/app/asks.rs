//! The asks' state: a provider key, a secret, a prompt - what the card is waiting for.

/// The provider key an ask collects; `provider` is the bare name for the card's words.
/// Asked only when the daemon named the keyless row: `models.keys` absent means no greening,
/// never no keys, so asking on its absence would block every switch behind a needless prompt.
#[derive(Debug, Clone)]
pub(crate) struct KeyAsk {
    pub(crate) choice: String,
    pub(crate) provider: String,
}

/// An open password request, as the head shows it.
#[derive(Debug, Clone)]
pub(crate) struct SecretAsk {
    pub(crate) req_id: String,
    pub(crate) prompt: String,
    pub(crate) command: String,
    pub(crate) deadline: u64,
}

/// A command of the operator's own waiting for an answer, drawn in the open on purpose.
/// Kept apart from `SecretAsk` in type, field, buffer and frame so no secret can borrow this
/// channel; `reading` says whether the daemon saw the run blocked or could not look at all.
#[derive(Debug, Clone)]
pub(crate) struct PromptAsk {
    pub(crate) req_id: String,
    pub(crate) job: String,
    pub(crate) command: String,
    pub(crate) question: Option<String>,
    pub(crate) reading: letibot_sessionlog::PromptReading,
}
