//! opencode's permission model, verbatim.
//!
//! Mirrors `packages/core/src/v1/config/permission.ts`, `packages/core/src/v1/permission.ts`
//! and `packages/core/src/util/wildcard.ts`. The model is:
//!
//! * a permission is a named string (`read`, `bash`, `todowrite`, …),
//! * a call is tested against a list of patterns (paths, arguments),
//! * the resolution is **last matching rule wins** (`findLast`), wildcard on both
//!   the permission and the pattern, defaulting to `ask`,
//! * a rule is `{ permission, pattern, action }` with action `allow` | `deny` | `ask`.
//!
//! The operator's config is the nested form (`"read": "allow"`, or
//! `"edit": { "*.md": "allow", "*": "ask" }`, `"*"` for the default), flattened
//! into a ruleset with the user's key order preserved — precedence depends on it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
    Ask,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Deny => "deny",
            Action::Ask => "ask",
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        match s {
            "allow" => Some(Action::Allow),
            "deny" => Some(Action::Deny),
            "ask" => Some(Action::Ask),
            _ => None,
        }
    }
}

/// `{ permission, pattern, action }`, the resolved unit opencode tests against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub permission: String,
    pub pattern: String,
    pub action: Action,
}

impl Rule {
    pub fn new(permission: impl Into<String>, pattern: impl Into<String>, action: Action) -> Self {
        Rule {
            permission: permission.into(),
            pattern: pattern.into(),
            action,
        }
    }
}

pub type Ruleset = Vec<Rule>;

/// opencode's `Wildcard.match` — `*` any run, `?` one char, backslash normalised
/// to slash, anchored, dotall. The `" .*"` suffix is made optional (a pattern
/// ending in a space before `*` matches with or without that trailing space).
pub fn wildcard_match(input: &str, pattern: &str) -> bool {
    let normalized = input.replace('\\', "/");
    let escaped = pattern.replace('\\', "/");

    // Escape the regex metacharacters opencode escapes (not `*` and `?`, which are
    // the wildcards).
    let mut re = String::with_capacity(escaped.len() + 8);
    for c in escaped.chars() {
        match c {
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' => {
                re.push('\\');
                re.push(c);
            }
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            _ => re.push(c),
        }
    }

    // A trailing space-then-star is "match with or without the space".
    if re.ends_with(" .*") {
        re.truncate(re.len() - 3);
        re.push_str("( .*)?");
    }

    let Ok(regex) = regex::Regex::new(&format!("^(?s:{})$", re)) else {
        return false;
    };
    regex.is_match(&normalized)
}

/// The one opencode function that decides: the last rule whose permission and
/// pattern both wildcard-match, else `ask`.
///
/// `rulesets` are consulted in order and flattened, so later rulesets (the user's
/// `always` approvals) override earlier ones (the config) — exactly `findLast`.
pub fn evaluate(permission: &str, pattern: &str, rulesets: &[&Ruleset]) -> Rule {
    rulesets
        .iter()
        .flat_map(|rs| rs.iter())
        .rev()
        .find(|rule| wildcard_match(permission, &rule.permission) && wildcard_match(pattern, &rule.pattern))
        .cloned()
        .unwrap_or_else(|| Rule::new(permission, "*", Action::Ask))
}

/// The operator's `permission` config, as JSON. Values are an `Action` string or a
/// nested `{ pattern: Action }` object; `"*"` is the default key.
///
/// Preserves key order, because `evaluate`'s last-match precedence depends on it.
pub fn config_to_ruleset(
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<Ruleset, String> {
    let mut rules = Vec::new();
    for (key, value) in config {
        flatten_rule(key, value, &mut rules)?;
    }
    Ok(rules)
}

/// Recursively flatten one `permission` config entry into `Rule`s, key order kept.
///
/// opencode's shape is one level deep: `"tool": "action"` (a bare action over `*`)
/// or `"tool": { "pattern": "action", … }`. Deeper nesting is refused, not guessed.
fn flatten_rule(
    permission: &str,
    value: &serde_json::Value,
    out: &mut Ruleset,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => {
            let action = Action::parse(s)
                .ok_or_else(|| format!("permission `{permission}`: unknown action `{s}`"))?;
            out.push(Rule::new(permission, "*", action));
        }
        serde_json::Value::Object(obj) => {
            for (pattern, v) in obj {
                match v {
                    serde_json::Value::String(s) => {
                        let action = Action::parse(s).ok_or_else(|| {
                            format!("permission `{permission}` pattern `{pattern}`: unknown action `{s}`")
                        })?;
                        out.push(Rule::new(permission, pattern, action));
                    }
                    _ => {
                        return Err(format!(
                            "permission `{permission}` pattern `{pattern}`: expected an action string"
                        ));
                    }
                }
            }
        }
        _ => return Err(format!("permission `{permission}`: expected a string or object")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s).unwrap()
    }

    #[test]
    fn wildcard_matches_star_and_question() {
        assert!(wildcard_match("src/lib.rs", "src/*.rs"));
        assert!(wildcard_match("src/lib.rs", "src/*"));
        // `*` is `.*`, so it crosses `/` — opencode's wildcard is not a glob `*`.
        assert!(wildcard_match("src/a/lib.rs", "src/*.rs"));
        assert!(wildcard_match("abc", "a?c"));
        assert!(!wildcard_match("abc", "a?"));
        assert!(!wildcard_match("x/y/z", "x/?"));
    }

    #[test]
    fn wildcard_normalises_backslash_to_slash() {
        assert!(wildcard_match("src\\lib.rs", "src/lib.rs"));
        assert!(wildcard_match("src/lib.rs", "src\\lib.rs"));
    }

    #[test]
    fn wildcard_matches_across_newlines() {
        // `s` (dotall): `*` reaches newlines.
        assert!(wildcard_match("a\nb", "a*"));
    }

    #[test]
    fn evaluate_last_matching_rule_wins() {
        let config = json(r#"{"read": "allow", "bash": "deny"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        assert_eq!(evaluate("read", "/etc/passwd", &[&ruleset]).action, Action::Allow);
        assert_eq!(evaluate("bash", "systemctl", &[&ruleset]).action, Action::Deny);
        assert_eq!(evaluate("write", "x", &[&ruleset]).action, Action::Ask);
    }

    #[test]
    fn evaluate_prefers_later_rulesets() {
        let config = json(r#"{"*": "deny"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        let approved = vec![Rule::new("bash", "*", Action::Allow)];
        // approved comes after config, so it wins.
        assert_eq!(evaluate("bash", "systemctl", &[&ruleset, &approved]).action, Action::Allow);
        assert_eq!(evaluate("read", "x", &[&ruleset, &approved]).action, Action::Deny);
    }

    #[test]
    fn config_flattens_a_bare_action_to_a_star_pattern() {
        let config = json(r#"{"read": "allow"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        assert_eq!(
            ruleset,
            vec![Rule::new("read", "*", Action::Allow)]
        );
    }

    #[test]
    fn config_preserves_key_order_for_precedence() {
        // `read` under a later key overrides an earlier `*`.
        let config = json(r#"{"*": "deny", "read": "allow"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        assert_eq!(evaluate("read", "x", &[&ruleset]).action, Action::Allow);
        assert_eq!(evaluate("bash", "x", &[&ruleset]).action, Action::Deny);
    }

    #[test]
    fn unknown_action_is_an_error_not_a_guess() {
        let config = json(r#"{"read": "maybe"}"#);
        assert!(config_to_ruleset(&config).is_err());
    }
}
