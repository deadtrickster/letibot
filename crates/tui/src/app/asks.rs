//! The asks' state: a provider key, a secret, a prompt - what the card is waiting for.

/// The provider key an ask collects; `provider` is the bare name for the card's words.
/// Asked only when the daemon named the keyless row: `models.keys` absent means no greening,
/// never no keys, so asking on its absence would block every switch behind a needless prompt.
#[derive(Debug, Clone)]
pub(crate) struct KeyAsk {
    pub(crate) choice: String,
    pub(crate) provider: String,
    /// What the card is for, which decides the words on it and the line Enter sends. One card and
    /// one buffer for every secret the operator types: `/models X --key` and `/flowy login`, and a
    /// third that arrives later is a variant here rather than a second card.
    pub(crate) kind: KeyKind,
}

/// The secrets the masked card collects, and the line each one becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyKind {
    /// A provider's key: Enter sends `/models CHOICE --key …`, which stores it at mode 600 and
    /// switches the session to that model.
    Provider,
    /// The fabric's token: Enter sends `/flowy login REST --token …`, where `rest` is what the
    /// operator typed between `login` and the flag - the seat and the address, which a card that
    /// dropped them would make them type again.
    Flowy { rest: String },
}

/// An open secret request, as the head shows it.
#[derive(Debug, Clone)]
pub(crate) struct SecretAsk {
    pub(crate) req_id: String,
    pub(crate) prompt: String,
    pub(crate) command: String,
    pub(crate) deadline: u64,
    /// What will use it, in the asker's words (`letibot-secret --for`). Empty for a program's
    /// own prompt, which already says what it is for.
    pub(crate) for_what: String,
    /// The name the value may be filed under, when the ask has one.
    pub(crate) key: Option<String>,
    /// How long the person may choose to keep it, in ms. `None` means "this command only" is the
    /// only answer.
    pub(crate) keep_ms: Option<u64>,
    /// The machine(s) it is going to.
    pub(crate) host: String,
    /// How many asks this one card settles: more than one when two helpers wanted the same key.
    pub(crate) waiting: u32,
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
