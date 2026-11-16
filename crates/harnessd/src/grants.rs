//! The store behind a gate's session and queue grants, and the `/permissions` verb that lists
//! and revokes them.

use std::path::PathBuf;

use letibot_tokencore::store::NewGrant;
use letibot_tools::grant_ledger::{GrantLedger, Kept, Scope, Stored};

use crate::config::{Config, StoreAt};

/// A [`GrantLedger`] over the session store.
pub struct StoreLedger {
    at: StoreAt,
}

impl StoreLedger {
    pub fn new(at: StoreAt) -> StoreLedger {
        StoreLedger { at }
    }

    fn store(&self) -> Result<letibot_tokencore::store::Store, String> {
        self.at.open().map_err(|e| e.to_string())
    }
}

impl GrantLedger for StoreLedger {
    fn keep(&self, scope: &Scope, kept: &Kept, why: &str) -> Result<(), String> {
        self.store()?
            .put_grant(&NewGrant {
                session_id: scope.owner(),
                scope: scope.as_str(),
                kind: kept.kind(),
                key: &kept.key(),
                body: &kept.body(),
                why,
            })
            .map_err(|e| e.to_string())
    }

    fn load(&self, scope: &Scope) -> Result<Vec<Stored>, String> {
        load(&self.store()?, scope)
    }

    fn clear(&self, scope: &Scope, kind: &str) -> Result<usize, String> {
        self.store()?
            .clear_grants(scope.owner(), scope.as_str(), kind)
            .map_err(|e| e.to_string())
    }
}

/// Every grant `store` keeps under `scope`. A row this build cannot read is left out.
pub fn load(store: &letibot_tokencore::store::Store, scope: &Scope) -> Result<Vec<Stored>, String> {
    let rows = store
        .grants(scope.owner(), scope.as_str())
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Stored {
                id: r.id,
                scope: scope.clone(),
                kept: Kept::parse(&r.kind, &r.body)?,
                why: r.why,
            })
        })
        .collect())
}

/// The review queue a session's grants also come from: its gatekeeper, for a queue seat.
pub fn queue_for(cfg: &Config) -> Option<String> {
    if !crate::harness::open::is_review_seat(cfg) {
        return None;
    }
    letibot_tools::grant_ledger::queue_of(&cfg.session_id).map(str::to_string)
}

/// The scopes `cfg`'s session reads its grants from.
pub fn scopes_for(cfg: &Config) -> Vec<Scope> {
    let mut out = vec![Scope::Session(cfg.session_id.clone())];
    if let Some(q) = queue_for(cfg) {
        out.push(Scope::Queue(q));
    }
    out
}

/// The view grants kept for `cfg`'s session, as `(path, writable, why)`, to bind at open.
pub fn kept_views(
    store: &letibot_tokencore::store::Store,
    cfg: &Config,
) -> Vec<(PathBuf, bool, String)> {
    let mut out = Vec::new();
    for scope in scopes_for(cfg) {
        for s in load(store, &scope).unwrap_or_default() {
            if let Kept::View { path, writable } = s.kept {
                let why = match &s.scope {
                    Scope::Session(_) => "granted earlier in this session, from a card".to_string(),
                    Scope::Queue(_) => {
                        "granted to this project's review queue, from a card".to_string()
                    }
                };
                out.push((path, writable, why));
            }
        }
    }
    out
}

/// `/permissions` and `/permissions revoke ID`, for `session` and, when given, the queue under
/// `gatekeeper`.
pub fn permissions(
    store: &letibot_tokencore::store::Store,
    session: &str,
    gatekeeper: Option<&str>,
    revoke: Option<i64>,
) -> crate::slash::SlashReply {
    let mut scopes = vec![Scope::Session(session.to_string())];
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
    if let Some(id) = revoke {
        let Some(row) = rows.iter().find(|r| r.id == id) else {
            return crate::slash::SlashReply::refused(vec![format!(
                "no grant {id} here - `/permissions` lists the ones this session can revoke"
            )]);
        };
        return match store.revoke_grant(id) {
            Ok(_) => {
                letibot_tools::grant_ledger::bump();
                let mut lines = vec![format!(
                    "revoked {id}: {} for {}",
                    row.kept.describe(),
                    row.scope.describe()
                )];
                if matches!(row.kept, Kept::View { .. }) {
                    lines.push(
                        "a session that already bound the path keeps it until it reopens; no new \
                         session gets it"
                            .into(),
                    );
                }
                crate::slash::SlashReply::listing(lines)
            }
            Err(e) => crate::slash::SlashReply::refused(vec![e.to_string()]),
        };
    }
    if rows.is_empty() {
        return crate::slash::SlashReply::listing(vec![
            "no kept grants for this session or its review queue".into(),
        ]);
    }
    let mut lines = vec!["kept grants (`/permissions revoke ID` removes one):".to_string()];
    for r in &rows {
        lines.push(format!(
            "  {:>4}  {} - {}",
            r.id,
            r.kept.describe(),
            r.scope.describe()
        ));
    }
    crate::slash::SlashReply::listing(lines)
}

#[cfg(test)]
mod tests;
