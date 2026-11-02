//! The one shape for "the infrastructure this tool needs is not attached".
//!
//! A tool with no backend behind it answers [`ToolOutcome::NotRun`]: `ok` would
//! claim the emptiness is the answer, `abstained` that the thing is not out
//! there, `failed` that something broke - and nothing ran. The refusal carries
//! how to attach the thing and what still works, never a way around the guard.

use letibot_transcript::ToolOutcome;

use crate::runtime::Invocation;

/// A tool whose backend is not attached, and everything the caller needs to
/// either attach it or work without it.
///
/// Every field but [`NotAttached::instead`] is required: each one is a sentence
/// a caller would otherwise leave out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAttached {
    /// The tool that did not run, as declared.
    pub tool: String,
    /// What is missing, as a negative noun phrase so it reads as the subject of
    /// "... is attached to this session": `"no search provider"`.
    pub missing: String,
    /// What did not happen, in the tool's own terms: `"nothing was searched and
    /// no request left this box"`. This is the sentence that stops the model
    /// reading the refusal as a finding.
    pub nothing_happened: String,
    /// How to attach it, imperative, aimed at the operator: `"start the daemon
    /// with --web-search PROVIDER"`.
    pub attach: String,
    /// What still works here. `None` when there is honestly nothing, which is
    /// worth saying by omission rather than by inventing a detour.
    pub instead: Option<String>,
}

impl NotAttached {
    pub fn new(
        tool: impl Into<String>,
        missing: impl Into<String>,
        nothing_happened: impl Into<String>,
        attach: impl Into<String>,
    ) -> Self {
        NotAttached {
            tool: tool.into(),
            missing: missing.into(),
            nothing_happened: nothing_happened.into(),
            attach: attach.into(),
            instead: None,
        }
    }

    pub fn instead(mut self, what: impl Into<String>) -> Self {
        self.instead = Some(what.into());
        self
    }

    /// The one line that goes in [`ToolOutcome::NotRun::why`], and therefore into
    /// the `outcome:` line of the envelope. Short on purpose: the body carries the
    /// rest.
    pub fn why(&self) -> String {
        format!(
            "{} is attached to this session, so `{}` did not run",
            self.missing, self.tool
        )
    }

    /// The body the model reads.
    pub fn body(&self) -> String {
        let mut out = format!("{}.\n", self.why());
        out.push_str(&format!(
            "{}, so this is the absence of a result and not a result: nothing here \
             says anything about what is or is not out there.\n",
            self.nothing_happened
        ));
        out.push_str(&format!("to attach it: {}\n", self.attach));
        if let Some(i) = &self.instead {
            out.push_str(&format!("what still works here: {i}\n"));
        }
        out
    }

    /// The whole thing as one invocation. Always `NotRun`: nothing here can
    /// build another outcome, so the mistake is unspellable rather than warned
    /// about.
    pub fn invocation(&self) -> Invocation {
        Invocation::not_run(self.why(), self.body())
    }

    /// The outcome alone, for a caller that is assembling its own payload.
    pub fn outcome(&self) -> ToolOutcome {
        ToolOutcome::NotRun { why: self.why() }
    }
}

impl std::fmt::Display for NotAttached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NotAttached {
        NotAttached::new(
            "web_search",
            "no search provider",
            "nothing was searched and no request left this box",
            "start the daemon with --web-search PROVIDER",
        )
        .instead("`grep` and `read` still work on this tree")
    }

    #[test]
    fn the_outcome_is_not_run_and_nothing_else_is_constructible() {
        let inv = sample().invocation();
        assert!(matches!(inv.outcome, ToolOutcome::NotRun { .. }));
    }

    #[test]
    fn the_body_says_what_did_not_happen_and_how_to_attach_it() {
        let body = sample().body();
        // The three sentences that make this a door rather than a wall.
        assert!(body.contains("nothing was searched"), "{body}");
        assert!(body.contains("--web-search PROVIDER"), "{body}");
        assert!(body.contains("grep"), "{body}");
    }

    #[test]
    fn it_never_claims_the_thing_is_not_out_there() {
        // The whole reason this is `NotRun` and not `Abstained`: a refusal that
        // reads as a finding is the failure with a different mask on.
        let body = sample().body().to_lowercase();
        for claim in [
            "not found",
            "no results",
            "does not exist",
            "nothing matched",
        ] {
            assert!(
                !body.contains(claim),
                "{claim:?} is a claim about the world"
            );
        }
    }
}
