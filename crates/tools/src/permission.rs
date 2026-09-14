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
        .find(|rule| {
            wildcard_match(permission, &rule.permission) && wildcard_match(pattern, &rule.pattern)
        })
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
        _ => {
            return Err(format!(
                "permission `{permission}`: expected a string or object"
            ));
        }
    }
    Ok(())
}

/// **The preapproved list.** What a coding session runs a hundred times a day
/// and nobody wants to answer for: the read-only half of `git` and `gh`, the
/// build-and-test verbs of the toolchains on this fleet, and the shell's own
/// read-only utilities. The operator's rule, 2026-09-14: *"we badly need a list
/// of preapproved globs, like all read-only git and gh commands, cargo, go, and
/// other tests"*.
///
/// Every pattern is a PREFIX over one simple command — see [`bash_segments`] for
/// what makes a compound command safe to test prefix-by-prefix — and the list is
/// the first ruleset consulted, so a row in the operator's file or environment
/// outranks any of it (last match wins). Deliberately absent: `find` (`-delete`,
/// `-exec`), `awk` (`system()`), `sed` without `-n` (`-i`), `env` (prints
/// secrets), `gh api` (POSTs), `cargo run`, `go run`, `git branch -d`, `git
/// stash` beyond `list` — each is one flag away from a write or a disclosure.
pub const DEFAULT_ALLOW: &[(&str, &[&str])] = &[(
    "bash",
    &[
        // git, read-only
        "git status*",
        "git log*",
        "git diff*",
        "git show*",
        "git blame*",
        "git grep*",
        "git ls-files*",
        "git ls-tree*",
        "git rev-parse*",
        "git rev-list*",
        "git describe*",
        "git branch",
        "git branch --list*",
        "git branch -a*",
        "git branch -r*",
        "git branch -v*",
        "git branch --show-current*",
        "git tag",
        "git tag -l*",
        "git tag --list*",
        "git remote",
        "git remote -v*",
        "git remote show*",
        "git remote get-url*",
        "git stash list*",
        "git stash show*",
        "git worktree list*",
        "git config --get*",
        "git config --list*",
        "git config -l*",
        "git cat-file*",
        "git shortlog*",
        "git count-objects*",
        "git name-rev*",
        "git merge-base*",
        "git reflog",
        "git reflog show*",
        "git --version*",
        // gh, read-only
        "gh pr view*",
        "gh pr list*",
        "gh pr status*",
        "gh pr checks*",
        "gh pr diff*",
        "gh issue view*",
        "gh issue list*",
        "gh issue status*",
        "gh run list*",
        "gh run view*",
        "gh workflow list*",
        "gh workflow view*",
        "gh release list*",
        "gh release view*",
        "gh repo view*",
        "gh search*",
        "gh auth status*",
        "gh --version*",
        // rust
        "cargo build*",
        "cargo check*",
        "cargo test*",
        "cargo clippy*",
        "cargo fmt*",
        "cargo doc*",
        "cargo tree*",
        "cargo metadata*",
        "cargo bench*",
        "cargo --version*",
        "rustc --version*",
        "rustfmt --check*",
        "rustfmt --edition*",
        "rustup show*",
        // go
        "go build*",
        "go test*",
        "go vet*",
        "go fmt*",
        "go list*",
        "go mod tidy*",
        "go mod download*",
        "go version*",
        "gofmt*",
        // other test runners
        "pytest*",
        "python3 -m pytest*",
        "python -m pytest*",
        "npm test*",
        "npm run test*",
        "npm run build*",
        "npm run lint*",
        "npx vitest*",
        "npx jest*",
        "pnpm test*",
        "yarn test*",
        "make test*",
        "make check*",
        "ctest*",
        // the shell's read-only utilities
        "ls*",
        "cat*",
        "head*",
        "tail*",
        "wc*",
        "grep*",
        "rg*",
        "ugrep*",
        "pwd",
        "which*",
        "type *",
        "echo*",
        "printf*",
        "stat*",
        "file*",
        "du*",
        "df*",
        "date*",
        "uname*",
        "nproc",
        "hostname",
        "id",
        "id -*",
        "sha256sum*",
        "md5sum*",
        "diff*",
        "sort*",
        "uniq*",
        "tr *",
        "cut*",
        "sed -n*",
        "jq*",
        "xxd*",
        "hexdump*",
        "strings*",
        "readlink*",
        "realpath*",
        "basename*",
        "dirname*",
        "true",
        "false",
        "test *",
        "[ *",
        "tree*",
        "nl*",
        "column*",
        "fold*",
        "less*",
        "tac*",
    ],
)];

/// [`DEFAULT_ALLOW`] as a ruleset, in the order written.
pub fn defaults() -> Ruleset {
    DEFAULT_ALLOW
        .iter()
        .flat_map(|(permission, patterns)| {
            patterns
                .iter()
                .map(move |p| Rule::new(*permission, *p, Action::Allow))
        })
        .collect()
}

/// A compound command as the simple commands it runs, or `None` when it cannot be
/// read that way.
///
/// A prefix rule says something about one program and its arguments; `git log;
/// rm -rf ~` matches `git log*` as a string and runs `rm`. So a command is tested
/// **segment by segment** — split on `|`, `||`, `&&`, `;`, `&` and newlines, each
/// segment trimmed — and admitted only when every segment is. And it is not
/// tested at all when it contains a construct the split cannot see through:
/// command substitution (`$(`, backticks), process substitution (`<(`, `>(`), a
/// redirection to anything but `/dev/null` or another descriptor (`> file` is a
/// write), a here-document, a brace or subshell group, or a leading variable
/// assignment (`PATH=x cargo` is not `cargo`). Those go to the adjudicator like
/// anything else — `None` here is "ask", never "allow".
///
/// This is a parser for the purpose of NOT admitting, which is the one job
/// `docs/tool-survey.md` says a shell parser in a gate may do.
pub fn bash_segments(command: &str) -> Option<Vec<String>> {
    if command.contains("$(")
        || command.contains('`')
        || command.contains("<(")
        || command.contains(">(")
        || command.contains("<<")
    {
        return None;
    }
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '|' | '&' | ';' | '\n' => {
                    // `||`, `&&` and `2>&1` — the descriptor duplication is handled
                    // in the redirection check below, not here.
                    if c == '&' && cur.ends_with('>') {
                        cur.push(c);
                        continue;
                    }
                    if (c == '|' || c == '&') && chars.peek() == Some(&c) {
                        chars.next();
                    }
                    segments.push(std::mem::take(&mut cur));
                }
                '(' | ')' | '{' | '}' if !cur.trim().is_empty() || c != '{' => {
                    // A group or a subshell: not one simple command.
                    return None;
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    segments.push(cur);
    let mut out = Vec::new();
    for seg in segments {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        if !redirections_are_harmless(seg) {
            return None;
        }
        // `FOO=bar cmd` runs cmd under a different environment; the prefix rule
        // is about cmd as written, so it is not the same command.
        let first = seg.split_whitespace().next().unwrap_or("");
        if first.contains('=') && !first.starts_with('=') {
            return None;
        }
        out.push(seg.to_string());
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Every `>` or `<` in the segment goes to `/dev/null` or duplicates a
/// descriptor (`2>&1`); anything else is a file the command would write or read.
fn redirections_are_harmless(seg: &str) -> bool {
    let bytes: Vec<char> = seg.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '>' || bytes[i] == '<' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == '>' || bytes[j] == '<') {
                j += 1;
            }
            let rest: String = bytes[j..].iter().collect();
            let target = rest.trim_start();
            if target.starts_with('&') {
                // `>&2`, `2>&1`
                i = j;
                continue;
            }
            let word: String = target.chars().take_while(|c| !c.is_whitespace()).collect();
            if word != "/dev/null" {
                return false;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    true
}

/// Evaluate a `bash` command against the rulesets, segment by segment: every
/// segment allowed → allow; any segment denied → deny; else ask. A command
/// [`bash_segments`] cannot read is `ask` unless a rule denies the whole string.
pub fn evaluate_bash(command: &str, rulesets: &[&Ruleset]) -> Action {
    let whole = evaluate("bash", command, rulesets);
    if whole.action == Action::Deny {
        return Action::Deny;
    }
    let Some(segments) = bash_segments(command) else {
        return Action::Ask;
    };
    let mut all_allowed = true;
    for seg in &segments {
        match evaluate("bash", seg, rulesets).action {
            Action::Deny => return Action::Deny,
            Action::Allow => {}
            Action::Ask => all_allowed = false,
        }
    }
    if all_allowed {
        Action::Allow
    } else {
        Action::Ask
    }
}

/// The pattern an *Always allow* answer over a `bash` command writes down: the
/// program, its verb when the program has verbs, and `*`. `git status
/// --short` → `git status*`; `cargo test -p x` → `cargo test*`; `ls -la` →
/// `ls*`. Claude Code's `Bash(git status:*)`, in opencode's spelling.
pub fn always_pattern_for_command(command: &str) -> String {
    const VERBED: &[&str] = &[
        "git",
        "gh",
        "cargo",
        "go",
        "npm",
        "npx",
        "pnpm",
        "yarn",
        "make",
        "docker",
        "kubectl",
        "systemctl",
        "rustup",
        "pip",
        "pip3",
        "uv",
        "poetry",
        "flowy",
        "firecode",
        "letibot",
        "leticode",
        "apt",
        "apt-get",
        "brew",
        "just",
        "mise",
    ];
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return "*".to_string();
    };
    if VERBED.contains(&program) {
        if let Some(verb) = words.next().filter(|v| !v.starts_with('-')) {
            return format!("{program} {verb}*");
        }
    }
    format!("{program}*")
}

/// `~/.config/letibot/permission.json`: opencode's nested shape, the file an
/// *Always allow* answer appends to and the operator edits by hand.
pub fn file_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("letibot").join("permission.json"))
}

/// The rules in the file, or none when there is no file. A file that does not
/// parse is an error, not an empty ruleset: a rule the operator wrote and the
/// gate silently dropped is the failure this whole module exists to prevent.
pub fn load_file(path: &std::path::Path) -> Result<Ruleset, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{}: not a JSON object", path.display()))?;
    config_to_ruleset(obj).map_err(|e| format!("{}: {e}", path.display()))
}

/// Append one rule to the file, keeping every other entry: read, insert
/// `permission → pattern → action`, write back. The file is created when absent.
pub fn append_to_file(path: &std::path::Path, rule: &Rule) -> Result<(), String> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
            .map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{}: not a JSON object", path.display()))?;
    let entry = obj
        .entry(rule.permission.clone())
        .or_insert_with(|| serde_json::json!({}));
    // A bare `"bash": "allow"` becomes `{ "*": "allow" }` before a pattern joins it.
    if let serde_json::Value::String(a) = entry.clone() {
        *entry = serde_json::json!({ "*": a });
    }
    let patterns = entry.as_object_mut().ok_or_else(|| {
        format!(
            "{}: `{}` is neither an action nor an object",
            path.display(),
            rule.permission
        )
    })?;
    patterns.insert(
        rule.pattern.clone(),
        serde_json::Value::String(rule.action.as_str().into()),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
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
        assert_eq!(
            evaluate("read", "/etc/passwd", &[&ruleset]).action,
            Action::Allow
        );
        assert_eq!(
            evaluate("bash", "systemctl", &[&ruleset]).action,
            Action::Deny
        );
        assert_eq!(evaluate("write", "x", &[&ruleset]).action, Action::Ask);
    }

    #[test]
    fn evaluate_prefers_later_rulesets() {
        let config = json(r#"{"*": "deny"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        let approved = vec![Rule::new("bash", "*", Action::Allow)];
        // approved comes after config, so it wins.
        assert_eq!(
            evaluate("bash", "systemctl", &[&ruleset, &approved]).action,
            Action::Allow
        );
        assert_eq!(
            evaluate("read", "x", &[&ruleset, &approved]).action,
            Action::Deny
        );
    }

    #[test]
    fn config_flattens_a_bare_action_to_a_star_pattern() {
        let config = json(r#"{"read": "allow"}"#);
        let ruleset = config_to_ruleset(&config).unwrap();
        assert_eq!(ruleset, vec![Rule::new("read", "*", Action::Allow)]);
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

    // --- the preapproved list -------------------------------------------------

    #[test]
    fn the_defaults_admit_read_only_git_and_the_test_verbs_and_nothing_wider() {
        let d = defaults();
        let rs: &[&Ruleset] = &[&d];
        for ok in [
            "git status --short",
            "git log --oneline -5",
            "cargo test -p letibot-tools",
            "go test ./...",
            "gh pr view 12",
            "ls -la crates",
            "git diff | head -20",
            "cargo build 2>&1 | tail -3",
            "grep -rn foo src && echo found",
            "cat x > /dev/null",
        ] {
            assert_eq!(evaluate_bash(ok, rs), Action::Allow, "{ok}");
        }
        for ask in [
            "git push",
            "git branch -d x",
            "gh api repos/x/y",
            "cargo run",
            "rm -rf x",
            "git log; rm -rf ~",
            "git log && rm x",
            "cat $(ls)",
            "cat `ls`",
            "ls > out.txt",
            "cat x >> log",
            "PATH=/x git status",
            "(git status)",
            "find . -delete",
            "sed -i s/a/b/ x",
            "env",
            "cargo test && rm -rf target",
        ] {
            assert_eq!(evaluate_bash(ask, rs), Action::Ask, "{ask}");
        }
    }

    #[test]
    fn the_operators_rows_outrank_the_defaults_in_both_directions() {
        let d = defaults();
        let mine = config_to_ruleset(&json(
            r#"{"bash": {"git log*": "deny", "cargo run*": "allow"}}"#,
        ))
        .unwrap();
        let rs: &[&Ruleset] = &[&d, &mine];
        assert_eq!(evaluate_bash("git log -3", rs), Action::Deny);
        assert_eq!(evaluate_bash("cargo run --bin x", rs), Action::Allow);
        // A deny on one segment denies the compound.
        assert_eq!(evaluate_bash("git status && git log", rs), Action::Deny);
    }

    #[test]
    fn an_always_answer_writes_the_program_and_its_verb() {
        assert_eq!(
            always_pattern_for_command("git status --short"),
            "git status*"
        );
        assert_eq!(
            always_pattern_for_command("cargo test -p x -- --nocapture"),
            "cargo test*"
        );
        assert_eq!(always_pattern_for_command("ls -la"), "ls*");
        assert_eq!(always_pattern_for_command("git -C x status"), "git*");
        assert_eq!(always_pattern_for_command("python3 -m pytest"), "python3*");
    }

    #[test]
    fn the_file_round_trips_and_an_append_keeps_the_rest() {
        let dir = crate::backend::tempdir::TempDir::new();
        let path = dir.path().join("letibot").join("permission.json");
        assert!(load_file(&path).unwrap().is_empty(), "no file is no rules");
        append_to_file(&path, &Rule::new("bash", "cargo run*", Action::Allow)).unwrap();
        append_to_file(&path, &Rule::new("edit", "*.md", Action::Allow)).unwrap();
        append_to_file(&path, &Rule::new("bash", "git push*", Action::Deny)).unwrap();
        let rules = load_file(&path).unwrap();
        assert_eq!(rules.len(), 3, "{rules:?}");
        let rs: &[&Ruleset] = &[&rules];
        assert_eq!(evaluate_bash("cargo run --bin x", rs), Action::Allow);
        assert_eq!(evaluate_bash("git push origin main", rs), Action::Deny);
        assert_eq!(evaluate("edit", "README.md", rs).action, Action::Allow);
        // The file is the operator's to read.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"cargo run*\": \"allow\""), "{text}");
        // A file that does not parse is an error, never an empty ruleset.
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_file(&path).is_err());
    }
}
