//! The store behind a gate's session and queue grants, and the `/permissions` verb that lists
//! and revokes them.

use std::path::PathBuf;

use letibot_sessionlog::pending_asks::{PendingAsks, RevokedGrant};
use letibot_tokencore::store::{GrantRow, NewGrant};
use letibot_tools::grant_ledger::{GrantLedger, Kept, SHADOW_BY, Scope, Stored};

use crate::config::{Config, StoreAt};

/// A [`GrantLedger`] over the session store.
pub struct StoreLedger {
    at: StoreAt,
    queue_ttl: std::time::Duration,
}

impl StoreLedger {
    pub fn new(at: StoreAt) -> StoreLedger {
        StoreLedger {
            at,
            queue_ttl: default_queue_ttl(),
        }
    }

    pub fn with_queue_ttl(mut self, ttl: std::time::Duration) -> StoreLedger {
        self.queue_ttl = ttl;
        self
    }

    fn store(&self) -> Result<letibot_tokencore::store::Store, String> {
        self.at.open().map_err(|e| e.to_string())
    }
}

impl GrantLedger for StoreLedger {
    fn keep(&self, scope: &Scope, kept: &Kept, why: &str, by: &str) -> Result<(), String> {
        self.store()?
            .put_grant(&NewGrant {
                session_id: scope.owner(),
                scope: scope.as_str(),
                kind: kept.kind(),
                key: &kept.key(),
                body: &kept.body(),
                why,
                by,
            })
            .map_err(|e| e.to_string())
    }

    fn load(&self, scope: &Scope) -> Result<Vec<Stored>, String> {
        let store = self.store()?;
        expire_queue(&store, scope, self.queue_ttl)?;
        load(&store, scope)
    }

    fn clear(&self, scope: &Scope, kind: &str) -> Result<usize, String> {
        self.store()?
            .clear_grants(scope.owner(), scope.as_str(), kind)
            .map_err(|e| e.to_string())
    }

    fn generation(&self) -> Result<String, String> {
        self.store()?.grant_generation().map_err(|e| e.to_string())
    }
}

fn default_queue_ttl() -> std::time::Duration {
    std::time::Duration::from_secs(crate::mergequeue::settings::GRANT_TTL_HOURS * 3600)
}

/// How long `cfg`'s queue grants last: `[queue] grant_ttl_hours`, the default when it cannot be
/// read.
pub fn queue_ttl(cfg: &Config) -> std::time::Duration {
    let hours = cfg
        .read_project_config
        .then(|| crate::mergequeue::settings::for_repository(&cfg.workspace).ok())
        .flatten()
        .map_or(crate::mergequeue::settings::GRANT_TTL_HOURS, |s| {
            s.grant_ttl_hours
        });
    std::time::Duration::from_secs(hours * 3600)
}

/// Drop `scope`'s grants older than `ttl` when it is a queue's. A session's live with it.
pub fn expire_queue(
    store: &letibot_tokencore::store::Store,
    scope: &Scope,
    ttl: std::time::Duration,
) -> Result<(), String> {
    let Scope::Queue(owner) = scope else {
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let gone = store
        .expire_grants(owner, scope.as_str(), now - ttl.as_millis() as i64)
        .map_err(|e| e.to_string())?;
    if gone > 0 {
        letibot_tools::grant_ledger::bump();
    }
    Ok(())
}

/// Every grant `store` keeps under `scope`. A row this build cannot read is left out.
pub fn load(store: &letibot_tokencore::store::Store, scope: &Scope) -> Result<Vec<Stored>, String> {
    let rows = store
        .grants(scope.owner(), scope.as_str())
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().filter_map(stored).collect())
}

/// A row as a gate reads it. `None` for one this build cannot read whole.
fn stored(r: GrantRow) -> Option<Stored> {
    Some(Stored {
        id: r.id,
        scope: Scope::parse(&r.scope, &r.session_id)?,
        kept: Kept::parse(&r.kind, &r.body)?,
        why: r.why,
        by: r.by,
    })
}

/// The view grants the shadow answered, in any session, oldest first.
pub fn shadow_grants(store: &letibot_tokencore::store::Store) -> Result<Vec<Stored>, String> {
    let rows = store.grants_by(SHADOW_BY).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(stored)
        .filter(|s| matches!(s.kept, Kept::View { .. }))
        .collect())
}

/// The one revoke, for `/permissions revoke` and `/shadow revoke` alike: the row leaves the
/// store, and every gate reading it drops the grant at its next call, unbinding a view path. A
/// grant the shadow answered counts against its breaker.
pub fn revoke(
    store: &letibot_tokencore::store::Store,
    row: &Stored,
    asks: Option<&PendingAsks>,
) -> Result<Vec<String>, String> {
    if !store.revoke_grant(row.id).map_err(|e| e.to_string())? {
        return Err(format!(
            "grant {} was already gone; nothing changed",
            row.id
        ));
    }
    letibot_tools::grant_ledger::bump();
    if row.by_shadow()
        && let Kept::View { path, .. } = &row.kept
        && let Some(a) = asks.and_then(PendingAsks::annotator)
    {
        a.revoked(&RevokedGrant {
            id: row.id,
            session: row.scope.owner().to_string(),
            path: path.display().to_string(),
        });
    }
    let mut lines = vec![format!(
        "revoked {}: {} for {}",
        row.id,
        row.kept.describe(),
        row.scope.describe()
    )];
    if matches!(row.kept, Kept::View { .. }) {
        lines.push("a running session unbinds the path at its next call".into());
    }
    Ok(lines)
}

/// `/shadow`'s listing of the reads the shadow answered, or `/shadow revoke [N]`: grant N, 1-based
/// and oldest first, the newest when N is left out.
pub fn shadow(
    store: &letibot_tokencore::store::Store,
    asks: Option<&PendingAsks>,
    revoke_n: Option<Option<usize>>,
) -> crate::slash::SlashReply {
    let rows = match shadow_grants(store) {
        Ok(r) => r,
        Err(e) => return crate::slash::SlashReply::refused(vec![e]),
    };
    let Some(n) = revoke_n else {
        return crate::slash::SlashReply::listing(
            rows.iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}", i + 1, line(r)))
                .collect(),
        );
    };
    let at = match n {
        _ if rows.is_empty() => {
            return crate::slash::SlashReply::refused(vec![
                "the shadow has answered no ask that is still in force".into(),
            ]);
        }
        None => rows.len() - 1,
        Some(n) if (1..=rows.len()).contains(&n) => n - 1,
        Some(n) => {
            return crate::slash::SlashReply::refused(vec![format!(
                "there is no grant {n}; the shadow holds {}",
                rows.len()
            )]);
        }
    };
    match revoke(store, &rows[at], asks) {
        Ok(lines) => crate::slash::SlashReply::listing(lines),
        Err(e) => crate::slash::SlashReply::refused(vec![e]),
    }
}

/// One kept grant in a listing.
fn line(r: &Stored) -> String {
    let by = if r.by.is_empty() {
        String::new()
    } else {
        format!(" (by {})", r.by)
    };
    format!(
        "{:>4}  {} - {}{by}",
        r.id,
        r.kept.describe(),
        r.scope.describe()
    )
}

/// The review queue a session's grants also come from: its gatekeeper, for a queue seat.
pub fn queue_for(cfg: &Config) -> Option<String> {
    if !crate::harness::open::is_review_seat(cfg) {
        return None;
    }
    letibot_tools::grant_ledger::queue_of(&cfg.session_id).map(str::to_string)
}

/// The scopes `cfg`'s session reads its grants from: its own, its tree's, and its queue's. A view
/// grant is kept at the tree's root, so a session that never answered a card still finds the one a
/// sibling or the parent answered - that is the whole of *a grant answered by any session of a
/// tree is in the view of every session of that tree opened afterwards*.
pub fn scopes_for(cfg: &Config, root: &str) -> Vec<Scope> {
    let mut out = vec![Scope::Session(cfg.session_id.clone())];
    if root != cfg.session_id {
        out.push(Scope::Session(root.to_string()));
    }
    if let Some(q) = queue_for(cfg) {
        out.push(Scope::Queue(q));
    }
    out
}

/// The session at the root of `id`'s tree: the id a view grant of the tree is kept under. `parent`
/// is `id`'s own parent for a session with no store row yet, which is the state a session is in at
/// its own open - it is minted and opened before anything writes it down.
pub fn tree_root(
    store: &letibot_tokencore::store::Store,
    id: &str,
    parent: Option<&str>,
) -> String {
    let mut cur = match (store.session_parent(id), parent) {
        (Ok(Some(_)), _) => id.to_string(),
        (_, Some(p)) if !p.is_empty() => p.to_string(),
        _ => return id.to_string(),
    };
    // The same cap the other chain walks take: a corrupt row must not hang an open.
    for _ in 0..32 {
        match store.session_parent(&cur).ok().flatten().flatten() {
            Some(p) if !p.is_empty() => cur = p,
            _ => break,
        }
    }
    cur
}

/// The view grants kept for `cfg`'s session tree, as `(path, writable, why)`, to bind at open.
pub fn kept_views(
    store: &letibot_tokencore::store::Store,
    cfg: &Config,
) -> Vec<(PathBuf, bool, String)> {
    let mut out: Vec<(PathBuf, bool, String)> = Vec::new();
    let ttl = queue_ttl(cfg);
    let root = tree_root(store, &cfg.session_id, cfg.parent_session_id.as_deref());
    for scope in scopes_for(cfg, &root) {
        let _ = expire_queue(store, &scope, ttl);
        for s in load(store, &scope).unwrap_or_default() {
            if let Kept::View { path, writable } = s.kept {
                let why = match &s.scope {
                    Scope::Session(_) => {
                        "granted earlier in this session's tree, from a card".to_string()
                    }
                    Scope::Queue(_) => {
                        "granted to this project's review queue, from a card".to_string()
                    }
                };
                // A session's own row and the tree's can name one path: that is one grant, and
                // the wider of the two is the one the mount binds.
                match out.iter_mut().find(|(had, _, _)| *had == path) {
                    Some((_, writable_at, _)) => *writable_at |= writable,
                    None => out.push((path, writable, why)),
                }
            }
        }
    }
    out
}

/// `/permissions` and `/permissions revoke ID`, for `session` and, when given, the queue under
/// `gatekeeper` and the reads the shadow answered for its seats.
pub fn permissions(
    store: &letibot_tokencore::store::Store,
    session: &str,
    gatekeeper: Option<&str>,
    revoke_id: Option<i64>,
    asks: Option<&PendingAsks>,
) -> crate::slash::SlashReply {
    let mut scopes = vec![Scope::Session(session.to_string())];
    // The tree's own: a view grant a sibling or the parent answered is this session's too, and a
    // revoke has to reach it from here.
    let root = tree_root(store, session, None);
    if root != session {
        scopes.push(Scope::Session(root));
    }
    if let Some(g) = gatekeeper.filter(|g| *g != session) {
        scopes.push(Scope::Queue(g.to_string()));
    }
    if let Some(q) = letibot_tools::grant_ledger::queue_of(session)
        && !scopes.contains(&Scope::Queue(q.to_string()))
    {
        scopes.push(Scope::Queue(q.to_string()));
    }
    let mut rows = Vec::new();
    for scope in &scopes {
        match load(store, scope) {
            Ok(r) => rows.extend(r),
            Err(e) => return crate::slash::SlashReply::refused(vec![e]),
        }
    }
    if gatekeeper.is_some() {
        match shadow_grants(store) {
            Ok(r) => {
                for s in r {
                    if !rows.iter().any(|had| had.id == s.id) {
                        rows.push(s);
                    }
                }
            }
            Err(e) => return crate::slash::SlashReply::refused(vec![e]),
        }
    }
    if let Some(id) = revoke_id {
        let Some(row) = rows.iter().find(|r| r.id == id) else {
            return crate::slash::SlashReply::refused(vec![format!(
                "no grant {id} here - `/permissions` lists the ones this session can revoke"
            )]);
        };
        return match revoke(store, row, asks) {
            Ok(lines) => crate::slash::SlashReply::listing(lines),
            Err(e) => crate::slash::SlashReply::refused(vec![e]),
        };
    }
    if rows.is_empty() {
        return crate::slash::SlashReply::listing(vec![
            "no kept grants for this session or its review queue".into(),
        ]);
    }
    let mut lines = vec!["kept grants (`/permissions revoke ID` removes one):".to_string()];
    for r in &rows {
        lines.push(format!("  {}", line(r)));
    }
    crate::slash::SlashReply::listing(lines)
}

#[cfg(test)]
mod tests;
