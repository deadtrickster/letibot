//! **The asks' state**: a provider key, a secret, a prompt — what the card is waiting for.

/// **A provider key the picker is collecting, for a row this box holds no key behind.**
///
/// `choice` is the whole `PROVIDER/MODEL`, because Enter does both things in one verb —
/// `/models CHOICE --key K` stores the key (mode 600, the file the daemon reads) AND takes the
/// row, which is the round trip the typed spelling already is. `provider` is the name alone, for
/// the sentence the card asks with.
///
/// **Only asked when the daemon named the keyless row.** `models.keys` absent means *no
/// greening*, never *no keys* — an older daemon — and asking on its absence would block every
/// switch behind a prompt for a key the box may well hold.
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

/// **A command of the operator's own is waiting for an answer**, as the head shows it.
///
/// # It is not [`SecretAsk`] and it must not become it
///
/// The two are separate types, separate fields, separate buffers, separate actions and
/// separate frames — and the separation is the requirement rather than tidiness. A password
/// has its own path (`SUDO_ASKPASS`, a helper that attaches as an `askpass` head,
/// `ClientFrame::Secret`), and a prompt card is drawn **in the open**: what a person types
/// here is a line for a program's **stdin**, visible on the screen and unremarkable there.
/// A secret must never travel down it, and the cheapest way to keep that true is for the
/// two channels to have nothing in common to borrow — no masked buffer, no `secret` field,
/// no `Action::Secret`.
///
/// # What is on it
///
/// * `req_id` — what the answer is addressed to, so a stale card cannot answer a later
///   command. The daemon holds the open request and refuses one that is not.
/// * `command` — **the operator's own line, verbatim**. The daemon cannot know which
///   process in a pipeline asked (`sudo apt install mc` is three programs and the question
///   is the third one's), so the card names the one thing that is certain.
/// * `question` — **the last line the program wrote, to SHOW and never to decide on.**
///   `None` when it has written nothing at all, which is a real case (`! cat`, blocked
///   before its first byte): the card then says the command is waiting rather than showing
///   an empty line as if that were the question.
#[derive(Debug, Clone)]
pub(crate) struct PromptAsk {
    pub(crate) req_id: String,
    pub(crate) job: String,
    pub(crate) command: String,
    pub(crate) question: Option<String>,
}
