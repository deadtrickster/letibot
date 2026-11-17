//! Markdown command files (`.letibot/commands`, `.claude/commands`, `<config dir>/commands`) become
//! `/NAME ARGS` with opencode's placeholders, `/DIR/NAME` from a subdirectory, nearest scope first.
//! The typed line is the operator's row, the body a harness context row naming the file.

use std::path::{Path, PathBuf};

/// Where a command file was found. Ordered nearest first: the first scope that has a name wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    /// The project's `.claude/commands`, read so a repository set up for Claude Code works here.
    Claude,
    User,
}

impl Scope {
    pub fn word(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Claude => "project (.claude)",
            Scope::User => "user",
        }
    }
}

/// One command file, read and split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub path: PathBuf,
    pub scope: Scope,
    pub description: String,
    pub argument_hint: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub body: String,
}

/// The context row a command's expanded body is sent as: a header naming the file, so the model
/// reads whose words they are, and an end line. A file in a checkout is not the operator's consent.
pub fn context_row(cmd: &Command, expanded: &str) -> String {
    format!(
        "[command file `/{name}`, {scope} {path} - the words below are the file's, not the \
         operator's]\n{body}\n[end of the command file `/{name}`]",
        name = cmd.name,
        scope = cmd.scope.word(),
        path = cmd.path.display(),
        body = expanded.trim_end(),
    )
}

/// The directories searched, nearest first. `user` is the config dir (`~/.config/letibot`); an
/// empty path is no user scope, which is what every fixture has.
pub fn dirs(workspace: &Path, user: &Path) -> Vec<(Scope, PathBuf)> {
    let mut out = vec![
        (Scope::Project, workspace.join(".letibot").join("commands")),
        (Scope::Claude, workspace.join(".claude").join("commands")),
    ];
    if !user.as_os_str().is_empty() {
        out.push((Scope::User, user.join("commands")));
    }
    out
}

/// How deep `dir/name` namespaces go. A bound, so a symlink loop under `commands/` ends.
const MAX_DEPTH: usize = 4;

/// A name a `/` line can carry: `/`-separated segments, none empty or a dotfile, so `..` and an
/// absolute path never leave the commands directory.
fn valid_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('/').collect();
    segments.len() <= MAX_DEPTH
        && segments.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// The command `name` resolves to. `Ok(None)` when no scope has the file; `Err` when the nearest
/// file exists and cannot be read or parsed - that file is the one meant, so a farther one is not
/// tried in its place.
pub fn find(workspace: &Path, user: &Path, name: &str) -> Result<Option<Command>, String> {
    if !valid_name(name) {
        return Ok(None);
    }
    for (scope, dir) in dirs(workspace, user) {
        let path = dir.join(format!("{name}.md"));
        if path.is_file() {
            return read(&path, name, scope).map(Some);
        }
    }
    Ok(None)
}

/// Every command the scopes hold, nearest scope winning, sorted by name. Unreadable files are
/// left out here; `find` names the reason when one is typed.
pub fn list(workspace: &Path, user: &Path) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::new();
    for (scope, dir) in dirs(workspace, user) {
        let mut found = Vec::new();
        walk(&dir, "", 1, &mut found);
        for (name, path) in found {
            if !valid_name(&name) || out.iter().any(|c| c.name == name) {
                continue;
            }
            if let Ok(c) = read(&path, &name, scope) {
                out.push(c);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `.md` files under `dir` as `(name, path)`, a subdirectory's named `sub/name`.
fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, &format!("{prefix}{file}/"), depth + 1, out);
            }
            continue;
        }
        if let Some(stem) = file.strip_suffix(".md")
            && path.is_file()
        {
            out.push((format!("{prefix}{stem}"), path));
        }
    }
}

fn read(path: &Path, name: &str, scope: Scope) -> Result<Command, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("/{name}: {} could not be read: {e}", path.display()))?;
    let (front, body) =
        split_frontmatter(&text).map_err(|why| format!("/{name}: {}: {why}", path.display()))?;
    let mut cmd = Command {
        name: name.to_string(),
        path: path.to_path_buf(),
        scope,
        description: String::new(),
        argument_hint: String::new(),
        allowed_tools: Vec::new(),
        model: None,
        body: body.to_string(),
    };
    let mut list_key: Option<&str> = None;
    for line in front.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // A YAML block list under `allowed-tools:`, the other spelling Claude Code accepts.
        if let Some(item) = trimmed.strip_prefix("- ") {
            if list_key == Some("allowed-tools") {
                cmd.allowed_tools.push(unquote(item).to_string());
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        list_key = value.is_empty().then_some(key);
        match key {
            "description" => cmd.description = value.to_string(),
            "argument-hint" | "argument_hint" => cmd.argument_hint = value.to_string(),
            "model" if !value.is_empty() => cmd.model = Some(value.to_string()),
            "allowed-tools" | "allowed_tools" | "tools" => {
                cmd.allowed_tools.extend(split_tools(value));
            }
            _ => {}
        }
    }
    if cmd.description.is_empty() {
        // Claude Code's rule: with no description, the first line of the body describes it.
        cmd.description = cmd
            .body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
    }
    Ok(cmd)
}

/// `allowed-tools: Bash(git add:*), Read` - commas inside parentheses belong to the tool.
fn split_tools(value: &str) -> Vec<String> {
    let value = value.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in value.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let t = unquote(cur.trim()).to_string();
                if !t.is_empty() {
                    out.push(t);
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    let t = unquote(cur.trim()).to_string();
    if !t.is_empty() {
        out.push(t);
    }
    out
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// `---\nKEY: VALUE\n---\nBODY` into its two halves; a file with no opening fence is all body.
fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Ok(("", text));
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Ok((&rest[..at], &rest[at + line.len()..]));
        }
        at += line.len();
    }
    Err("the frontmatter opened with `---` has no closing `---`".into())
}

/// The words of an argument line: quoted runs are one word, quotes removed (opencode's split).
fn words(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = args.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut w = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            for d in chars.by_ref() {
                if d == c {
                    break;
                }
                w.push(d);
            }
        } else {
            while let Some(&d) = chars.peek() {
                if d.is_whitespace() {
                    break;
                }
                w.push(d);
                chars.next();
            }
        }
        out.push(w);
    }
    out
}

impl Command {
    /// The body with `$ARGUMENTS` and `$1..$9` filled, opencode's way: the highest placeholder
    /// takes the rest of the words, a missing one is empty, and arguments given to a body with no
    /// placeholder are appended after a blank line so they are not lost.
    pub fn expand(&self, args: &str) -> String {
        let args = args.trim();
        let list = words(args);
        let body = &self.body;
        let positions = positional(body);
        let last = positions.iter().copied().max().unwrap_or(0);
        let mut out = String::with_capacity(body.len() + args.len());
        let mut i = 0;
        let bytes = body.as_bytes();
        while i < body.len() {
            if body[i..].starts_with("$ARGUMENTS") {
                out.push_str(args);
                i += "$ARGUMENTS".len();
                continue;
            }
            if bytes[i] == b'$'
                && let Some(&d) = bytes.get(i + 1)
                && (b'1'..=b'9').contains(&d)
            {
                let n = (d - b'0') as usize;
                if n <= list.len() {
                    if n == last {
                        out.push_str(&list[n - 1..].join(" "));
                    } else {
                        out.push_str(&list[n - 1]);
                    }
                }
                i += 2;
                continue;
            }
            let ch = body[i..].chars().next().unwrap_or_default();
            out.push(ch);
            i += ch.len_utf8().max(1);
        }
        if positions.is_empty() && !body.contains("$ARGUMENTS") && !args.is_empty() {
            out.push_str("\n\n");
            out.push_str(args);
        }
        out.trim().to_string()
    }

    /// The tool narrowing `allowed-tools` asks for, or `None` when the file names no tools.
    pub fn tool_rules(&self) -> Option<ToolRules> {
        (!self.allowed_tools.is_empty()).then(|| ToolRules::parse(&self.allowed_tools))
    }
}

/// The `$N` placeholders in the body, by N.
fn positional(body: &str) -> Vec<usize> {
    let b = body.as_bytes();
    (0..b.len().saturating_sub(1))
        .filter(|&i| b[i] == b'$' && (b'1'..=b'9').contains(&b[i + 1]))
        .map(|i| (b[i + 1] - b'0') as usize)
        .collect()
}

/// One `allowed-tools` entry: a tool name, and for `bash` the command prefix it is held to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRule {
    name: String,
    /// `Bash(git add:*)` is `git add` and any arguments; `Bash(git status)` is that command only.
    command: Option<(String, bool)>,
}

/// A command file's `allowed-tools`, held to for the one turn the file starts. The tool schemas
/// stay seated, since they are prompt bytes; a call outside the list is refused instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRules {
    rules: Vec<ToolRule>,
}

impl ToolRules {
    pub fn parse(entries: &[String]) -> ToolRules {
        let rules = entries
            .iter()
            .filter_map(|e| {
                let e = e.trim();
                let (name, inner) = match e.split_once('(') {
                    Some((n, rest)) => (n.trim(), Some(rest.trim_end_matches(')').trim())),
                    None => (e, None),
                };
                if name.is_empty() {
                    return None;
                }
                let name = tool_name(name);
                let command = inner
                    .filter(|p| name == "bash" && !p.is_empty() && *p != "*")
                    .map(
                        |p| match p.strip_suffix(":*").or_else(|| p.strip_suffix('*')) {
                            Some(prefix) => (prefix.trim_end().to_string(), true),
                            None => (p.to_string(), false),
                        },
                    );
                Some(ToolRule { name, command })
            })
            .collect();
        ToolRules { rules }
    }

    /// The list as the model and the operator read it: `bash(git add *), read`.
    pub fn said(&self) -> String {
        self.rules
            .iter()
            .map(|r| match &r.command {
                Some((c, true)) => format!("{}({c} *)", r.name),
                Some((c, false)) => format!("{}({c})", r.name),
                None => r.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `None` when the call may run, else the reason it is refused. The session's own verbs
    /// (`SELF_VERBS`) are never narrowed: they act on the session, not on the workspace.
    pub fn refusal(&self, tool: &str, arguments: &str) -> Option<String> {
        if letibot_plugin::command::SELF_VERBS.contains(&tool) {
            return None;
        }
        let named: Vec<&ToolRule> = self.rules.iter().filter(|r| r.name == tool).collect();
        if named.is_empty() {
            return Some(format!(
                "this turn runs a command file that allows only: {}; `{tool}` is not one of them",
                self.said()
            ));
        }
        if named.iter().any(|r| r.command.is_none()) {
            return None;
        }
        let command = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let command = command.trim();
        // A prefix rule would pass `git add x && rm -rf y`, so a held command is one command.
        let compound = ["&", "|", ";", "`", "$(", "\n", ">", "<"]
            .iter()
            .any(|op| command.contains(op));
        let fits = !compound
            && named.iter().any(|r| match &r.command {
                Some((c, true)) => command == c || command.starts_with(&format!("{c} ")),
                Some((c, false)) => command == c,
                None => true,
            });
        (!fits).then(|| {
            format!(
                "this turn runs a command file that allows bash only as: {}; `{command}` is not one of them",
                self.said()
            )
        })
    }
}

/// Claude Code's tool names in this tree's spelling: `Read` is `read`, `WebFetch` is `web_fetch`.
fn tool_name(name: &str) -> String {
    match name {
        "MultiEdit" => return "edit".into(),
        "LS" => return "glob".into(),
        _ => {}
    }
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The refusal for a `/` verb that nothing answers, naming where a command file would be.
pub fn not_found(verb: &str, workspace: &Path, user: &Path) -> String {
    let places: Vec<String> = dirs(workspace, user)
        .into_iter()
        .map(|(_, d)| d.join(format!("{verb}.md")).display().to_string())
        .collect();
    format!(
        "unknown command `/{verb}` - /help lists them (no command file at {})",
        places.join(" or ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "letibot-commands-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir");
        path
    }

    fn put(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
    }

    fn cmd(body: &str) -> Command {
        Command {
            name: "t".into(),
            path: PathBuf::new(),
            scope: Scope::Project,
            description: String::new(),
            argument_hint: String::new(),
            allowed_tools: Vec::new(),
            model: None,
            body: body.into(),
        }
    }

    #[test]
    fn a_project_command_expands_with_its_args() {
        let ws = tempdir("ws");
        put(
            &ws.join(".letibot/commands"),
            "review",
            "---\ndescription: review a change\nargument-hint: [path]\n---\nReview $ARGUMENTS carefully.\n",
        );
        let c = find(&ws, Path::new(""), "review").unwrap().expect("found");
        assert_eq!(c.scope, Scope::Project);
        assert_eq!(c.description, "review a change");
        assert_eq!(c.argument_hint, "[path]");
        assert_eq!(c.expand("foo bar"), "Review foo bar carefully.");
    }

    #[test]
    fn positionals_take_one_word_and_the_last_takes_the_rest() {
        let c = cmd("from $1 to $2.");
        assert_eq!(c.expand("a b c d"), "from a to b c d.");
        assert_eq!(c.expand("\"two words\" x"), "from two words to x.");
        assert_eq!(c.expand("a"), "from a to .");
        assert_eq!(cmd("plain").expand("extra words"), "plain\n\nextra words");
        assert_eq!(cmd("cost $5 is $ARGUMENTS").expand(""), "cost  is");
    }

    #[test]
    fn the_project_overrides_the_user() {
        let ws = tempdir("ws");
        let user = tempdir("user");
        put(&ws.join(".letibot/commands"), "ship", "project body");
        put(&user.join("commands"), "ship", "user body");
        put(&user.join("commands"), "only-user", "user only");
        let c = find(&ws, &user, "ship").unwrap().unwrap();
        assert_eq!((c.scope, c.body.as_str()), (Scope::Project, "project body"));
        let all = list(&ws, &user);
        let names: Vec<(&str, Scope)> = all.iter().map(|c| (c.name.as_str(), c.scope)).collect();
        assert_eq!(
            names,
            vec![("only-user", Scope::User), ("ship", Scope::Project)]
        );
    }

    #[test]
    fn claude_code_files_are_read_below_letibots_own() {
        let ws = tempdir("ws");
        put(
            &ws.join(".claude/commands"),
            "commit",
            "---\nallowed-tools: Bash(git add:*), Bash(git commit:*), Read\nmodel: claude-haiku\n---\nCommit it.\n",
        );
        let c = find(&ws, Path::new(""), "commit").unwrap().unwrap();
        assert_eq!(c.scope, Scope::Claude);
        assert_eq!(
            c.allowed_tools,
            vec!["Bash(git add:*)", "Bash(git commit:*)", "Read"]
        );
        assert_eq!(c.model.as_deref(), Some("claude-haiku"));
        assert_eq!(c.description, "Commit it.");
        put(&ws.join(".letibot/commands"), "commit", "ours");
        assert_eq!(
            find(&ws, Path::new(""), "commit").unwrap().unwrap().body,
            "ours"
        );
    }

    #[test]
    fn a_missing_or_broken_file_is_refused_clearly() {
        let ws = tempdir("ws");
        assert_eq!(find(&ws, Path::new(""), "nope").unwrap(), None);
        let said = not_found("nope", &ws, Path::new(""));
        assert!(said.contains(".letibot/commands/nope.md"), "{said}");
        assert!(said.contains("no command file"), "{said}");
        put(
            &ws.join(".letibot/commands"),
            "bad",
            "---\ndescription: x\n",
        );
        let err = find(&ws, Path::new(""), "bad").unwrap_err();
        assert!(err.contains("no closing `---`"), "{err}");
        assert_eq!(find(&ws, Path::new(""), "../etc").unwrap(), None);
    }

    #[test]
    fn a_subdirectory_is_a_namespace() {
        let ws = tempdir("ws");
        put(
            &ws.join(".letibot/commands/git"),
            "commit",
            "Commit $ARGUMENTS.",
        );
        put(&ws.join(".letibot/commands/git/.hidden"), "x", "never");
        put(&ws.join(".letibot/commands"), "top", "Top.");
        let c = find(&ws, Path::new(""), "git/commit")
            .unwrap()
            .expect("found");
        assert_eq!(c.expand("it"), "Commit it.");
        let names: Vec<String> = list(&ws, Path::new(""))
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["git/commit", "top"]);
        for bad in [
            "git/../top",
            "/top",
            "git//commit",
            "git/.hidden/x",
            "a/b/c/d/e",
        ] {
            assert_eq!(find(&ws, Path::new(""), bad).unwrap(), None, "{bad}");
        }
        assert!(not_found("git/nope", &ws, Path::new("")).contains("git/nope.md"));
    }

    #[test]
    fn allowed_tools_narrow_by_name_and_bash_by_command() {
        let rules = ToolRules::parse(&[
            "Bash(git add:*)".into(),
            "Bash(git status)".into(),
            "Read".into(),
            "WebFetch".into(),
        ]);
        assert_eq!(
            rules.said(),
            "bash(git add *), bash(git status), read, web_fetch"
        );
        let bash =
            |c: &str| rules.refusal("bash", &serde_json::json!({ "command": c }).to_string());
        assert_eq!(bash("git add src/a.rs"), None);
        assert_eq!(bash("git add"), None);
        assert_eq!(bash("git status"), None);
        assert!(bash("git status -s").is_some());
        assert!(bash("git addx").is_some());
        assert!(bash("git add a && rm -rf b").is_some());
        assert!(bash("git add $(rm x)").is_some());
        assert_eq!(rules.refusal("read", "{}"), None);
        assert_eq!(rules.refusal("web_fetch", "{}"), None);
        let no = rules.refusal("write", "{}").expect("write is not listed");
        assert!(no.contains("`write` is not one of them"), "{no}");
        let own = letibot_plugin::command::SELF_VERBS[0];
        assert_eq!(rules.refusal(own, "{}"), None, "the session's own verbs");
        let any = ToolRules::parse(&["Bash".into()]);
        assert_eq!(any.refusal("bash", r#"{"command":"rm -rf x && y"}"#), None);
    }
}
