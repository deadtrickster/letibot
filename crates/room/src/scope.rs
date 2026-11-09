//! An ask by SCOPE: which session owns a path or a topic, and which rule said so. The rules are
//! tried in order - a live lease, the last edit to the path, an `OWNERS` line, then the project
//! room - and the one that matched is what the asker is told. No clock and no socket here.

use crate::own::{Claim, covers, is_path, normalise};

/// The most of a repo's `OWNERS` file the room will read: a file bigger than this is not an
/// ownership table, and an ask is not the place to read a novel.
pub const OWNERS_MAX: usize = 64 * 1024;

/// What an ask names in place of a session: a repo-relative path (a glob allowed) or a topic word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Path(String),
    Topic(String),
}

impl Scope {
    /// The scope an ask named, if it named one: `path` and `topic` are alternatives, and neither is
    /// a direct ask to a session.
    pub fn of(path: &str, topic: &str) -> Result<Option<Scope>, String> {
        let (path, topic) = (path.trim(), topic.trim());
        match (path.is_empty(), topic.is_empty()) {
            (true, true) => Ok(None),
            (false, false) => Err(
                "an ask names a `path` or a `topic`, not both — the room has to know which one it \
                 is resolving, and nothing was sent"
                    .to_string(),
            ),
            (false, _) => Ok(Some(Scope::Path(normalise(path)))),
            (true, _) => Ok(Some(Scope::Topic(normalise(topic)))),
        }
    }

    /// The scope as the asker wrote it, normalised.
    pub fn text(&self) -> &str {
        match self {
            Scope::Path(p) => p,
            Scope::Topic(t) => t,
        }
    }

    /// The word the asker reads for it, and the one an answer is phrased with.
    pub fn word(&self) -> &'static str {
        match self {
            Scope::Path(_) => "path",
            Scope::Topic(_) => "topic",
        }
    }
}

/// Which rule put the question on a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// A live ownership lease covering the scope (R163).
    Lease,
    /// The last session to edit the path in this project.
    Edit,
    /// A line of the repo's `OWNERS` file.
    Owners,
    /// Nothing owns it, so the question is public in the project room.
    Room,
}

impl Rule {
    /// The word the count and the asker's sentence use.
    pub fn word(self) -> &'static str {
        match self {
            Rule::Lease => "lease",
            Rule::Edit => "last edit",
            Rule::Owners => "OWNERS",
            Rule::Room => "room",
        }
    }
}

/// Where an ask by scope landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// A session, because of `rule`, and the scope or file that matched it.
    Owner {
        target: String,
        rule: Rule,
        detail: String,
    },
    /// Nobody owns it: the question goes to the project room.
    Room,
}

impl Placement {
    /// The target, when there is one.
    pub fn target(&self) -> Option<&str> {
        match self {
            Placement::Owner { target, .. } => Some(target),
            Placement::Room => None,
        }
    }

    pub fn rule(&self) -> Rule {
        match self {
            Placement::Owner { rule, .. } => *rule,
            Placement::Room => Rule::Room,
        }
    }

    /// The rule and what matched it, in a phrase: `lease on `crates/room/**``.
    pub fn short(&self) -> String {
        match self {
            Placement::Owner {
                rule: Rule::Lease,
                detail,
                ..
            } => format!("lease on `{detail}`"),
            Placement::Owner {
                rule: Rule::Edit,
                detail,
                ..
            } => format!("last edit to `{detail}`"),
            Placement::Owner {
                rule: Rule::Owners,
                detail,
                ..
            } => format!("`OWNERS` line `{detail}`"),
            Placement::Owner { .. } => String::new(),
            Placement::Room => "no owner".to_string(),
        }
    }

    /// Why this is where the question went, as the asker reads it.
    pub fn why(&self, scope: &Scope) -> String {
        match self {
            Placement::Owner {
                target,
                rule: Rule::Lease,
                detail,
            } => format!("`{target}` holds a live lease on `{detail}`"),
            Placement::Owner {
                target,
                rule: Rule::Edit,
                detail,
            } => format!("`{target}` edited `{detail}` last"),
            Placement::Owner {
                target,
                rule: Rule::Owners,
                detail,
            } => format!("the repo's `OWNERS` says `{detail}`, so `{target}`"),
            Placement::Owner { .. } => String::new(),
            Placement::Room => format!(
                "nothing owns `{}`: no live lease, no edit in this project and no `OWNERS` line",
                scope.text()
            ),
        }
    }
}

/// Resolve a scope to a session. `edits` is `(session, path, at_ms)`; `owners` is the repo's
/// `OWNERS` file, empty when there is none; `usable` says whether a session can be asked at all,
/// so a rule that names a session the room cannot reach falls through to the next one.
pub fn place(
    scope: &Scope,
    claims: &[Claim],
    edits: &[(String, String, u64)],
    owners: &str,
    now: u64,
    usable: impl Fn(&str) -> bool,
) -> Placement {
    // 1. A live lease. The most specific one wins - the lease on a file before the lease on the
    // directory above it - and the newest breaks a tie.
    let mut leases: Vec<&Claim> = claims
        .iter()
        .filter(|c| c.live(now) && names(scope, &c.scope))
        .collect();
    leases.sort_by(|a, b| {
        b.scope
            .len()
            .cmp(&a.scope.len())
            .then(b.renewed_ms.cmp(&a.renewed_ms))
    });
    if let Some(claim) = leases.into_iter().find(|c| usable(&c.owner)) {
        return Placement::Owner {
            target: claim.owner.clone(),
            rule: Rule::Lease,
            detail: claim.scope.clone(),
        };
    }
    // 2. The last session to edit the path in this project. A topic has no file behind it.
    if let Scope::Path(asked) = scope {
        let mut newest: Option<&(String, String, u64)> = None;
        for edit in edits.iter().filter(|e| covers(asked, &e.1) && usable(&e.0)) {
            if newest.is_none_or(|n| edit.2 > n.2) {
                newest = Some(edit);
            }
        }
        if let Some((session, path, _)) = newest {
            return Placement::Owner {
                target: session.clone(),
                rule: Rule::Edit,
                detail: path.clone(),
            };
        }
    }
    // 3. An `OWNERS` line, when the repo keeps one.
    if let Some((line, handle)) = owners_line(owners, scope.text())
        && usable(&handle)
    {
        return Placement::Owner {
            target: handle,
            rule: Rule::Owners,
            detail: line,
        };
    }
    Placement::Room
}

/// Does a claim's scope name the asked scope? Two paths by covering either way - the ask may be the
/// file a directory claim owns, or the directory a file claim lives in - and two topics by equality.
fn names(scope: &Scope, claim: &str) -> bool {
    match scope {
        Scope::Path(asked) => is_path(claim) && (covers(claim, asked) || covers(asked, claim)),
        Scope::Topic(topic) => !is_path(claim) && normalise(claim) == *topic,
    }
}

/// The handle an `OWNERS` file gives a scope: `<glob> <handle>` a line, `#` a comment, the first
/// line whose glob covers the path decides. `*` and `**` alone are every path. The line is
/// returned as written, so the asker reads what the file says.
pub fn owners_line(owners: &str, scope: &str) -> Option<(String, String)> {
    if !is_path(scope) {
        // A topic is not a path, and an `OWNERS` file names paths: the rule cannot answer for it.
        return None;
    }
    for line in owners.lines() {
        let said = line.split('#').next().unwrap_or("").trim();
        let mut words = said.split_whitespace();
        let (Some(glob), Some(handle), None) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let handle = handle.trim_start_matches('@');
        let all = matches!(glob.trim_start_matches("./"), "*" | "**");
        if all || covers(glob.trim_start_matches("./"), scope) {
            return Some((said.to_string(), handle.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::own::Claims;

    fn claim(owner: &str, scope: &str, renewed_ms: u64) -> Claim {
        let mut claims = Claims::default();
        claims
            .own(owner, scope, Some(60 * 60 * 1000), renewed_ms)
            .expect("a claim")
    }

    fn any(_: &str) -> bool {
        true
    }

    #[test]
    fn a_lease_beats_every_other_rule_and_the_most_specific_one_wins() {
        let now = 10_000;
        let claims = vec![
            claim("s-dir", "crates/room/**", now),
            claim("s-file", "crates/room/src/server.rs", now),
        ];
        let scope = Scope::of("crates/room/src/server.rs", "").unwrap().unwrap();
        let placed = place(&scope, &claims, &[], "", now, any);
        assert_eq!(
            placed,
            Placement::Owner {
                target: "s-file".into(),
                rule: Rule::Lease,
                detail: "crates/room/src/server.rs".into()
            }
        );
        // The directory above the ask is covered by its claim.
        let scope = Scope::of("crates/room/src/chat.rs", "").unwrap().unwrap();
        assert_eq!(
            place(&scope, &claims, &[], "", now, any).target(),
            Some("s-dir")
        );
        // An expired lease owns nothing.
        let expired = place(&scope, &claims, &[], "", now + 4 * 60 * 60 * 1000, any);
        assert_eq!(expired, Placement::Room);
    }

    #[test]
    fn a_topic_lease_is_the_same_word() {
        let now = 1_000;
        let claims = vec![claim("s-wire", "wire", now)];
        let scope = Scope::of("", "Wire").unwrap().unwrap();
        assert_eq!(
            place(&scope, &claims, &[], "", now, any).target(),
            Some("s-wire")
        );
        let other = Scope::of("", "budgets").unwrap().unwrap();
        assert_eq!(place(&other, &claims, &[], "", now, any), Placement::Room);
    }

    #[test]
    fn with_no_lease_the_last_editor_is_asked() {
        let now = 5_000;
        let scope = Scope::of("crates/room/src/chat.rs", "").unwrap().unwrap();
        let edits = vec![
            ("s-old".to_string(), "crates/room/src/chat.rs".into(), 1_000),
            ("s-new".to_string(), "crates/room/src/chat.rs".into(), 4_000),
            (
                "s-elsewhere".to_string(),
                "crates/tui/src/app.rs".into(),
                4_500,
            ),
        ];
        let placed = place(&scope, &[], &edits, "", now, any);
        assert_eq!(
            placed,
            Placement::Owner {
                target: "s-new".into(),
                rule: Rule::Edit,
                detail: "crates/room/src/chat.rs".into()
            }
        );
        // An ask that names a directory is answered by the newest edit under it.
        let dir = Scope::of("crates/room", "").unwrap().unwrap();
        assert_eq!(
            place(&dir, &[], &edits, "", now, any).target(),
            Some("s-new")
        );
        // A topic has no file behind it: the edit rule says nothing.
        let topic = Scope::of("", "chat").unwrap().unwrap();
        assert_eq!(place(&topic, &[], &edits, "", now, any), Placement::Room);
    }

    #[test]
    fn a_rule_naming_a_session_that_cannot_be_asked_falls_through() {
        let now = 1_000;
        let claims = vec![claim("s-a", "crates/room/**", now)];
        let scope = Scope::of("crates/room/src/x.rs", "").unwrap().unwrap();
        let owners = "crates/room/** s-b\n";
        let only_b = |who: &str| who == "s-b";
        assert_eq!(
            place(&scope, &claims, &[], owners, now, only_b).target(),
            Some("s-b")
        );
        let nobody = |_: &str| false;
        assert_eq!(
            place(&scope, &claims, &[], owners, now, nobody),
            Placement::Room
        );
    }

    #[test]
    fn an_owners_file_is_glob_handle_a_line_and_the_first_match_decides() {
        let owners = "# who reads what\n\
                      crates/room/src/server.rs s-db\n\
                      crates/room/** s-room\n\
                      * s-host\n";
        assert_eq!(
            owners_line(owners, "crates/room/src/server.rs"),
            Some((
                "crates/room/src/server.rs s-db".to_string(),
                "s-db".to_string()
            ))
        );
        assert_eq!(
            owners_line(owners, "crates/room/src/chat.rs").map(|(_, h)| h),
            Some("s-room".to_string())
        );
        assert_eq!(
            owners_line(owners, "docs/room.md").map(|(_, h)| h),
            Some("s-host".to_string())
        );
        // A line with three words is not a rule, and a topic is not a path.
        assert_eq!(owners_line("a/b s-x s-y\n", "a/b"), None);
        assert_eq!(owners_line("crates/room/** s-room\n", "wire"), None);
        assert_eq!(owners_line("", "crates/room/src/x.rs"), None);
    }

    #[test]
    fn an_ask_names_a_path_or_a_topic_and_never_both() {
        assert_eq!(Scope::of("", "").unwrap(), None);
        assert!(Scope::of("crates/x", "wire").is_err());
        assert_eq!(
            Scope::of("./crates/x/", "").unwrap(),
            Some(Scope::Path("crates/x".into()))
        );
        assert_eq!(
            Scope::of("", "Wire").unwrap(),
            Some(Scope::Topic("wire".into()))
        );
    }
}
