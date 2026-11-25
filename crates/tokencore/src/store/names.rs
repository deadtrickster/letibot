//! What a session's name means, in one place: the folder a name is unique in, the names that
//! cannot be names, and the answer a typed name gets. `-r NAME`, `--continue NAME` and the
//! Claude Code bridge all ask here, so a name that opens one session opens that session
//! everywhere, and a name that is ambiguous is OFFERED rather than guessed.

use rusqlite::params;

use super::{Archived, Result, Store, StoreError, StoredSession};

/// The folder a name is unique in: the path with its symlinks resolved and no trailing slash, so
/// `/w/p` and `/w/p/` (and a worktree reached through a link) are one folder rather than three.
/// A path that is not on disk cannot be resolved, and is taken as written minus a trailing slash:
/// a session whose folder was deleted is still a session.
pub fn canonical_folder(path: &str) -> String {
    let written = path.trim();
    let bare = written.trim_end_matches('/');
    let looks = if bare.is_empty() && written.starts_with('/') {
        "/"
    } else {
        bare
    };
    std::fs::canonicalize(looks)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| looks.to_string())
}

/// Is this word an ADDRESS rather than a name? Every session id this tree mints begins `s-` or
/// `cc-`, so a name with that prefix would be a second way to spell one id and a wrong answer to
/// `--session`: refused where a person types a name, never at a row that already has one.
pub fn looks_like_an_id(name: &str) -> bool {
    let lowered = name.trim().to_lowercase();
    lowered.starts_with("s-") || lowered.starts_with("cc-")
}

/// [`looks_like_an_id`] as the refusal a person reads. The key is checked too, because `S 17` is
/// an address by its slug (`s-17`) whatever its case.
pub fn refuse_an_id(title: &str) -> super::Result<()> {
    let title = title.trim();
    if looks_like_an_id(title) || name_key(title).is_some_and(|k| looks_like_an_id(&k)) {
        return Err(super::StoreError::Refused(format!(
            "{title:?} is not a session name: a name beginning `s-` or `cc-` reads as a session id, \
             and `--session` would be asked about two different things. Name it something else."
        )));
    }
    Ok(())
}

/// The key a title is filed under: its [`super::slug_of`], so `Stroppy Conformity` and
/// `stroppy-conformity` are one name. A title with no letters or digits in it has no slug, and is
/// filed under `~` and its own lowercase spelling - which is exactly when [`super::same_name`]
/// calls two such titles equal. `None` for a title nobody has: an unnamed session has no name to
/// clash with.
pub fn name_key(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let slug = super::slug_of(title);
    Some(if slug.is_empty() {
        format!("~{}", title.to_lowercase())
    } else {
        slug
    })
}

/// One row a typed name can mean, as the caller needs it to offer a choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub id: String,
    pub title: String,
    /// The canonical folder the session is in, so a chooser can say where.
    pub folder: String,
    pub archived: bool,
    pub created_ms: i64,
    pub last_activity_ms: i64,
}

impl Named {
    /// A listing row as a name's meaning. Unnamed rows are not here: a name nobody has is not one.
    pub fn of(s: &StoredSession) -> Option<Named> {
        let title = s
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())?;
        Some(Named {
            id: s.id.clone(),
            title: title.to_string(),
            folder: s
                .workspace_canon
                .clone()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| canonical_folder(&s.workspace_root)),
            archived: s.archived_ms.is_some(),
            created_ms: s.created_ms,
            last_activity_ms: s.last_activity_ms,
        })
    }
}

/// What a typed name means among the sessions of the folders asked about. The order is the rule:
/// a LIVE exact name first, and only when nothing live answers is a prefix or an archived row
/// offered - a name that is exactly one session's is never also a guess about another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meaning {
    /// One live session has that name.
    One(Named),
    /// Several live sessions have it exactly. A store that predates the unique name can hold this,
    /// and the caller refuses rather than pick one.
    Ambiguous(Vec<Named>),
    /// No live session is called that; these are called something that begins with what was typed.
    Prefixes(Vec<Named>),
    /// No live session is called that; an archived one is, so it can be reopened by id.
    Archived(Vec<Named>),
    /// Nothing anywhere in these folders is called that.
    Nothing,
}

impl Meaning {
    /// The one session this name means, when it means one. `Err` is the refusal a caller with no
    /// way to ask a person - a tool call - is owed: it names what to do instead.
    pub fn opened(&self, name: &str) -> std::result::Result<String, String> {
        match self {
            Meaning::One(one) => Ok(one.id.clone()),
            Meaning::Nothing => Err(format!("no session here is named {name:?}")),
            Meaning::Ambiguous(rows) => Err(format!(
                "{name:?} names {} sessions: {}. Use the id.",
                rows.len(),
                ids(rows)
            )),
            Meaning::Prefixes(rows) => Err(format!(
                "no session here is named {name:?}; {} begin{} with it: {}. Use the id, or a longer \
                 name.",
                rows.len(),
                if rows.len() == 1 { "s" } else { "" },
                ids(rows)
            )),
            Meaning::Archived(rows) => Err(format!(
                "{name:?} is archived - {} - so it is not in the list. Open it by id and it comes \
                 back.",
                ids(rows)
            )),
        }
    }
}

/// `s-1 "a name"`, `s-2 "another"` - the candidates a refusal names.
fn ids(rows: &[Named]) -> String {
    rows.iter()
        .map(|r| format!("{} {:?}", r.id, r.title))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a typed name means among `rows`: the LIVE exact one first, and only when no live session
/// answers - or answers only approximately - a prefix or an archived row, so a name that is
/// exactly one session's is never also a guess about another's. The caller decides which rows to
/// offer (a folder's, the operator's own conversations), because that is a listing's rule and not
/// a name's.
pub fn meaning_of(rows: &[super::StoredSession], name: &str) -> Meaning {
    let name = name.trim();
    if name.is_empty() {
        return Meaning::Nothing;
    }
    let named: Vec<Named> = rows.iter().filter_map(Named::of).collect();
    let exact: Vec<Named> = named
        .iter()
        .filter(|r| !r.archived && super::same_name(&r.title, name))
        .cloned()
        .collect();
    match exact.len() {
        1 => return Meaning::One(exact[0].clone()),
        n if n > 1 => return Meaning::Ambiguous(exact),
        _ => {}
    }
    let want = name.to_lowercase();
    let slug = super::slug_of(name);
    let mut prefixes: Vec<Named> = named
        .iter()
        .filter(|r| {
            !r.archived
                && (r.title.to_lowercase().starts_with(&want)
                    || (!slug.is_empty() && super::slug_of(&r.title).starts_with(&slug)))
        })
        .cloned()
        .collect();
    if !prefixes.is_empty() {
        prefixes.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
        return Meaning::Prefixes(prefixes);
    }
    let mut away: Vec<Named> = named
        .iter()
        .filter(|r| r.archived && super::same_name(&r.title, name))
        .cloned()
        .collect();
    if away.is_empty() {
        return Meaning::Nothing;
    }
    away.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
    Meaning::Archived(away)
}

/// The dash a derived name takes when a folder already holds it: `base`, then `base-2`,
/// `base-3`... `taken` answers whether a candidate is in use. Never truncates, so a name that is
/// not free at 999 still finds one - the caller's bound, if any, is the caller's.
pub fn counted(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

impl Store {
    /// The live main sessions of `folders` (canonical, or as written) called something like
    /// `name`, by [`meaning_of`]. The store's door for a caller that has the folders rather than
    /// the rows: the bridge's tool calls, and the CLI when it searches every folder.
    pub fn meaning(&self, folders: &[String], name: &str) -> Result<Meaning> {
        let mut rows: Vec<super::StoredSession> = Vec::new();
        for folder in folders {
            let canon = canonical_folder(folder);
            // Both spellings, because a row written before the folder column existed still has
            // its path in the workspace column, and one written after has both.
            let found = self.sessions_matching(
                " WHERE (s.workspace_canon = ?1 OR s.workspace_root = ?1 OR s.workspace_root = ?2)",
                params![canon, folder.trim()],
            )?;
            for s in found {
                if rows.iter().any(|r| r.id == s.id) {
                    continue;
                }
                // The row's own folder, not the one that found it: a row whose path merely starts
                // like it is another folder's, and a name is unique per folder.
                let mine = s
                    .workspace_canon
                    .clone()
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| canonical_folder(&s.workspace_root));
                if mine == canon {
                    rows.push(s);
                }
            }
        }
        Ok(meaning_of(&rows, name))
    }

    /// The live session of `folder` that already answers to `title`, as the refusal names it, and
    /// any archived session that used to - the second half only so a person told the name is taken
    /// is not left wondering which of two sessions they meant.
    pub fn holder(&self, folder: &str, title: &str) -> Result<Option<(Named, Option<Named>)>> {
        let canon = canonical_folder(folder);
        let rows = self.sessions_matching(
            " WHERE (s.workspace_canon = ?1 OR s.workspace_root = ?1 OR s.workspace_root = ?2)",
            params![canon, folder.trim()],
        )?;
        let mut live = None;
        let mut away = None;
        for s in &rows {
            if s.parent_session_id.is_some() {
                continue;
            }
            let Some(named) = Named::of(s) else { continue };
            if !super::same_name(&named.title, title) {
                continue;
            }
            if named.archived {
                away = away.or(Some(named));
            } else {
                live = live.or(Some(named));
            }
        }
        Ok(live.map(|l| (l, away)))
    }

    /// The refusal a taken name is owed, or `None` when nothing live holds it. `id` is the session
    /// doing the naming, so a session may keep the name it already has.
    pub fn taken_by(&self, id: &str, folder: &str, title: &str) -> Result<Option<String>> {
        let Some((live, away)) = self.holder(folder, title)? else {
            return Ok(None);
        };
        if live.id == id {
            return Ok(None);
        }
        let was = match &away {
            Some(a) if a.id != id => {
                format!(" (an archived session, {}, used to answer to it)", a.id)
            }
            _ => String::new(),
        };
        Ok(Some(format!(
            "the name {title:?} is taken in this folder by {} {:?}{was}",
            live.id, live.title
        )))
    }

    /// A free name for a session nobody typed the name of - an auto title, a fork, a seat the
    /// daemon seats itself - so a derived name never overwrites one a person chose. `Err` from
    /// `taken` is a store failure and is passed through.
    pub fn free_derived_name(&self, folder: &str, base: &str) -> Result<String> {
        let canon = canonical_folder(folder);
        let rows = self.sessions_matching(
            " WHERE (s.workspace_canon = ?1 OR s.workspace_root = ?1 OR s.workspace_root = ?2)",
            params![canon, folder.trim()],
        )?;
        let live: Vec<Named> = rows
            .iter()
            .filter(|s| s.parent_session_id.is_none() && s.archived_ms.is_none())
            .filter_map(Named::of)
            .collect();
        Ok(counted(base, |candidate| {
            live.iter().any(|r| super::same_name(&r.title, candidate))
        }))
    }

    /// Is `title` one this folder would take, for a name a person typed: not an address, and not
    /// another live session's. `id` is the session that will hold it - `None` for one about to be
    /// created, which therefore clashes with every name but its own empty one.
    pub fn check_name(&self, id: Option<&str>, folder: &str, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        refuse_an_id(title)?;
        if let Some(said) = self.taken_by(id.unwrap_or(""), folder, title)? {
            return Err(StoreError::Refused(said));
        }
        Ok(())
    }

    /// Every session an `archived` filter admits, oldest name first, as a name's meaning reads
    /// them. The one read a caller that has the whole store in hand needs.
    pub fn named_rows(&self, archived: Archived) -> Result<Vec<Named>> {
        Ok(self
            .list_sessions_archived(archived)?
            .iter()
            .filter(|s| s.parent_session_id.is_none())
            .filter_map(Named::of)
            .collect())
    }
}
