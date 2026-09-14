//! The tool schema: clause 4 (`access` is declared) and clause 6 (the description
//! says how to use the tool, never what the data contains).
//!
//! Both clauses are enforced here rather than reviewed, because both fail
//! silently. A tool whose access is decided per call reads as working right up
//! until the first call that should have prompted and did not; a description that
//! names data reads as working right up until the data changes.

use serde::{Deserialize, Serialize};

/// What a tool is allowed to touch. **Declared once, in the schema.**
///
/// §8.1 clause 4. This is the field §11.3's policy table keys on, so it is not a
/// hint: [`crate::runtime::ToolRuntime`] consults the gate only for a tool that is
/// not [`Access::Read`], which is what makes "a read-only tool never prompts" a
/// structural property rather than a promise.
///
/// A tool declares the *widest* thing it can do. A tool that reads a file and
/// sometimes writes one is a `Write` tool on every call, including the calls that
/// only read: the point of the declaration is that the class is knowable before
/// the arguments are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
    Exec,
    Network,
    /// Changes the **session's own** state and nothing the operator owns: the
    /// intent board, the goal, plan mode, a question posted to a head.
    ///
    /// Declared rather than folded into `Read` because the survey found exactly
    /// that hole in grok-build — `todo_write` and `update_goal` declare `Read`
    /// while mutating session state (`docs/tool-survey.md` §1.4) — and a tool that
    /// under-declares is a hole whether or not the thing it writes is a file.
    /// `is_unattended` is still true: nobody needs to adjudicate the model writing
    /// its own todo list. The tool that *widens* capability (`exit_plan_mode`)
    /// declares `Write` for that reason and goes to the gate like any other write.
    Session,
}

/// What a subagent is denied **below** the session that spawned it. A set of
/// access classes, and it only ever removes: a subagent's downgrade is the
/// parent's downgrade plus its own, so nothing a parent could not do can be
/// handed down by naming a wider role. `Read` and `Session` cannot be denied —
/// a subagent that cannot read its files or its own todo list is not a
/// subagent, and a caller asking for that is told so.
///
/// Three things read it, and all three must agree: the tools seated (a denied
/// class's tools are not in the prompt, so the model is not told it has a
/// capability it does not); the backend (no writable view without `Write`, no
/// process host without `Exec`); and the permission ruleset (a `deny` rule per
/// denied tool, so a call that reached the gate anyway is refused by name).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Downgrade {
    pub deny: std::collections::BTreeSet<Access>,
}

impl Downgrade {
    pub fn none() -> Self {
        Downgrade::default()
    }

    /// `read-only` (no write, exec or network), or any of `no-write`, `no-exec`,
    /// `no-network`, comma-separated. Anything else is refused by name.
    pub fn parse(s: &str) -> Result<Downgrade, String> {
        let mut d = Downgrade::default();
        for word in s.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            match word.to_ascii_lowercase().as_str() {
                "read-only" | "readonly" | "read_only" | "survey" => {
                    d.deny.insert(Access::Write);
                    d.deny.insert(Access::Exec);
                    d.deny.insert(Access::Network);
                }
                "no-write" | "no_write" | "nowrite" => {
                    d.deny.insert(Access::Write);
                }
                "no-exec" | "no_exec" | "noexec" => {
                    d.deny.insert(Access::Exec);
                }
                "no-network" | "no_network" | "nonetwork" | "offline" => {
                    d.deny.insert(Access::Network);
                }
                "no-read" | "no-session" | "no_read" | "no_session" => {
                    return Err(format!(
                        "`{word}` is not a downgrade: a subagent that cannot read, or cannot \
                         keep its own todo list, is not a subagent"
                    ));
                }
                other => {
                    return Err(format!(
                        "`{other}` is not a downgrade. There are four: `read-only` (no write, \
                         exec or network), `no-write`, `no-exec`, `no-network`, comma-separated"
                    ));
                }
            }
        }
        Ok(d)
    }

    pub fn denies(&self, a: Access) -> bool {
        self.deny.contains(&a)
    }

    pub fn is_none(&self) -> bool {
        self.deny.is_empty()
    }

    /// This downgrade plus another. Union, never intersection: the child of a
    /// downgraded session inherits every denial and may add its own.
    pub fn and(&self, other: &Downgrade) -> Downgrade {
        Downgrade {
            deny: self.deny.union(&other.deny).copied().collect(),
        }
    }

    /// `no write, no exec` — for a disclosure or a listing.
    pub fn describe(&self) -> String {
        if self.deny.is_empty() {
            return "none".into();
        }
        self.deny
            .iter()
            .map(|a| format!("no {}", a.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Access {
    pub fn as_str(&self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Exec => "exec",
            Access::Network => "network",
            Access::Session => "session",
        }
    }

    /// Whether a call of this class may proceed without anyone being asked.
    ///
    /// `Read` and `Session`. Everything else goes through the gate, and in M1
    /// there is no adjudicator behind it, so everything else is
    /// [`crate::runtime::Gate`]'s problem and not a tool's.
    ///
    /// `Session` is unattended because nothing outside the session changes: the
    /// model writing its own working list is not an act on the operator's machine,
    /// and a session whose todo tool prompts is a session nobody uses. The moment a
    /// session-state tool can *widen* what the session may do, it declares `Write`
    /// instead — see `exit_plan_mode`.
    pub fn is_unattended(&self) -> bool {
        matches!(self, Access::Read | Access::Session)
    }
}

/// One tool, as the model sees it and as the policy reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    /// How to use the tool. Never what the data contains — see [`lint_description`].
    pub description: String,
    /// JSON Schema for the arguments. Used for the prompt and, more importantly,
    /// by [`crate::args`] to salvage a malformed call: knowing that `path` is a
    /// string and `limit` is an integer is what turns `"limit": "40"` into a
    /// repair instead of a rejection.
    pub parameters: serde_json::Value,
    pub access: Access,
}

impl ToolSchema {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        access: Access,
    ) -> Self {
        ToolSchema {
            name: name.into(),
            description: description.into(),
            parameters,
            access,
        }
    }

    /// The bytes that go into the stable prefix (`StablePrefix::tools_json`).
    ///
    /// `access` is **not** rendered. It is ours, not the model's: a model that can
    /// read the access class can argue about it, and §11's policy is not a
    /// negotiation. Key order is fixed by construction because a reorder here
    /// re-prefills the whole conversation.
    pub fn prompt_json(&self) -> String {
        let v = serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        });
        serde_json::to_string(&v).expect("a schema built from Values always serialises")
    }

    /// The declared type of one named parameter, if the schema gives one.
    pub fn param_type(&self, name: &str) -> Option<&str> {
        self.parameters
            .get("properties")?
            .get(name)?
            .get("type")?
            .as_str()
    }

    /// Parameter names, in schema order.
    pub fn param_names(&self) -> Vec<&str> {
        match self
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
        {
            Some(o) => o.keys().map(|k| k.as_str()).collect(),
            None => vec![],
        }
    }

    /// Required parameter names.
    pub fn required(&self) -> Vec<&str> {
        match self.parameters.get("required").and_then(|r| r.as_array()) {
            Some(a) => a.iter().filter_map(|v| v.as_str()).collect(),
            None => vec![],
        }
    }
}

/// One reason a description violates clause 6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptionFinding {
    /// A stable code, so a test can assert on the kind rather than the wording.
    pub code: &'static str,
    /// The offending fragment, quoted back.
    pub found: String,
}

impl std::fmt::Display for DescriptionFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.code, self.found)
    }
}

/// Clause 6, mechanised.
///
/// > The description says how to use the tool and never what the data contains.
/// > Tool descriptions are prompt, are never audited, and go stale.
///
/// This is a lint and not a proof: it catches the four ways a description goes
/// stale that can be recognised from the text alone. It is applied at
/// registration ([`crate::runtime::Registry::register`]) rather than in review,
/// because "never audited" is precisely the premise.
///
/// What it deliberately does not do is judge prose quality. A vague description is
/// a bad description; a description that says the corpus covers Rust 1.89 is a
/// **wrong** description six weeks from now, and only the second one is mechanical.
pub fn lint_description(text: &str) -> Vec<DescriptionFinding> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();

    // 1. A path into somebody's tree. The tool works on whatever tree it is
    //    pointed at; naming one bakes today's machine into the prompt.
    for marker in ["/home/", "/users/", "/data/", "c:\\", "/var/", "/opt/"] {
        if let Some(at) = lower.find(marker) {
            out.push(DescriptionFinding {
                code: "absolute_path",
                found: snippet(text, at),
            });
        }
    }

    // 2. A host, a port or a URL. Same failure, one layer out.
    for marker in ["http://", "https://", "localhost:", "127.0.0.1", "192.168."] {
        if let Some(at) = lower.find(marker) {
            out.push(DescriptionFinding {
                code: "endpoint",
                found: snippet(text, at),
            });
        }
    }

    // 3. A claim about the content. This is the clause proper: the description is
    //    prompt, so a claim here is a claim the model will act on long after it
    //    stopped being true.
    for marker in [
        "the corpus contains",
        "the corpus covers",
        "the index contains",
        "the index covers",
        "the repository contains",
        "indexed as of",
        "currently contains",
        "there are ",
        "the corpus has",
        "documents about",
    ] {
        if let Some(at) = lower.find(marker) {
            out.push(DescriptionFinding {
                code: "data_claim",
                found: snippet(text, at),
            });
        }
    }

    // 4. A date or a version. Always true when written, never true later.
    if let Some(at) = find_year_or_version(&lower) {
        out.push(DescriptionFinding {
            code: "dated",
            found: snippet(text, at),
        });
    }

    out
}

fn snippet(text: &str, at: usize) -> String {
    let start = text[..at]
        .char_indices()
        .rev()
        .nth(12)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[at..]
        .char_indices()
        .nth(40)
        .map(|(i, _)| at + i)
        .unwrap_or(text.len());
    text[start..end].trim().to_string()
}

/// A 19xx/20xx year, or a `v1.2`-shaped version. Hand-rolled: the crate has no
/// regex dependency and this is four lines of it.
fn find_year_or_version(lower: &str) -> Option<usize> {
    let b = lower.as_bytes();
    for i in 0..b.len() {
        // A year, not preceded or followed by another digit.
        if i + 4 <= b.len()
            && b[i..i + 4].iter().all(|c| c.is_ascii_digit())
            && (b[i] == b'1' && b[i + 1] == b'9' || b[i] == b'2' && b[i + 1] == b'0')
            && i.checked_sub(1).is_none_or(|p| !b[p].is_ascii_digit())
            && b.get(i + 4).is_none_or(|c| !c.is_ascii_digit())
        {
            return Some(i);
        }
        // `v` followed by digit-dot-digit.
        if b[i] == b'v'
            && i.checked_sub(1)
                .is_none_or(|p| !b[p].is_ascii_alphanumeric())
            && b.get(i + 1).is_some_and(|c| c.is_ascii_digit())
            && b[i + 1..].iter().take(6).any(|c| *c == b'.')
        {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_is_unattended() {
        assert!(Access::Read.is_unattended());
        for a in [Access::Write, Access::Exec, Access::Network] {
            assert!(!a.is_unattended(), "{a:?} must be gated");
        }
    }

    #[test]
    fn access_is_not_shown_to_the_model() {
        let s = ToolSchema::new(
            "read",
            "Return the text of a file.",
            serde_json::json!({"type": "object"}),
            Access::Read,
        );
        assert!(
            !s.prompt_json().contains("access"),
            "the access class is policy input, not prompt: {}",
            s.prompt_json()
        );
    }

    #[test]
    fn the_lint_catches_the_four_ways_a_description_goes_stale() {
        let cases = [
            ("Search under /home/dead/Projects.", "absolute_path"),
            ("Ask the server at http://192.168.1.55:9755.", "endpoint"),
            (
                "The corpus contains the Rust standard library.",
                "data_claim",
            ),
            ("Indexed against Rust 1.89 as of 2026.", "dated"),
        ];
        for (text, code) in cases {
            let found = lint_description(text);
            assert!(
                found.iter().any(|f| f.code == code),
                "{text:?} should trip {code}, got {found:?}"
            );
        }
    }

    #[test]
    fn a_usage_description_lints_clean() {
        // The shape every built-in must keep: verbs, arguments, and what comes
        // back. No nouns from anybody's disk.
        let ok = "Return the text of a file. Give a path; optionally an offset and \
                  a line limit. A path that does not exist returns the nearest \
                  listing instead of an error.";
        assert_eq!(lint_description(ok), vec![]);
    }
}
