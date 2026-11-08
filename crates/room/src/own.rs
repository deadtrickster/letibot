//! Ownership as a lease: one live owner per scope (a path glob or a topic), renewed by the owner's
//! own work in scope and expiring otherwise. A handover is closed-loop: the old owner holds the
//! scope until the receiver accepts. No clock and no socket here; the server hands in `now`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::item::duration;

/// A claim lives this long without renewal unless the owner names another TTL.
pub const OWN_TTL_DEFAULT_MS: u64 = 60 * 60 * 1000;

/// The longest TTL a claim may ask for.
pub const OWN_TTL_MAX_MS: u64 = 4 * 60 * 60 * 1000;

/// Ended claims kept per project, for the record; a live one is never dropped.
pub const ENDED_KEEP: usize = 200;

/// A handover offered and not yet accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hand {
    pub to: String,
    #[serde(default)]
    pub note: String,
    pub at_ms: u64,
    /// The message that told the receiver, so the accept is recorded as acting on it.
    #[serde(default)]
    pub seq: Option<u64>,
}

/// How a claim ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ended", rename_all = "snake_case")]
pub enum Ended {
    Released { by: String, at_ms: u64 },
    Expired { at_ms: u64 },
}

/// One scope and its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: u64,
    pub scope: String,
    pub owner: String,
    pub since_ms: u64,
    pub renewed_ms: u64,
    pub ttl_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hand: Option<Hand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
    /// The operator was told its owner stopped; cleared when the owner is back.
    #[serde(default)]
    pub flagged: bool,
}

impl Claim {
    pub fn live(&self, now: u64) -> bool {
        self.ended.is_none() && self.renewed_ms.saturating_add(self.ttl_ms) > now
    }

    pub fn left_ms(&self, now: u64) -> u64 {
        self.renewed_ms
            .saturating_add(self.ttl_ms)
            .saturating_sub(now)
    }

    /// `crates/room/** 52m left`, with a pending handover when there is one.
    pub fn line(&self, now: u64) -> String {
        let mut s = format!("{} {} left", self.scope, duration(self.left_ms(now)));
        if let Some(h) = &self.hand {
            s.push_str(&format!(", handing to {}", h.to));
        }
        s
    }
}

/// A scope is a path glob when it looks like a path, a topic word otherwise.
pub fn is_path(scope: &str) -> bool {
    scope.contains(['/', '*', '?', '.'])
}

/// The scope as kept: trimmed, a topic lowercased, a path without a leading `./`.
pub fn normalise(scope: &str) -> String {
    let s = scope.trim();
    if is_path(s) {
        s.trim_start_matches("./").trim_end_matches('/').to_string()
    } else {
        s.to_lowercase()
    }
}

fn literal_prefix(glob: &str) -> &str {
    let end = glob.find(['*', '?', '[']).unwrap_or(glob.len());
    &glob[..end]
}

/// Does path scope `scope` cover file `path`? `*` stays in a segment, `**` crosses them, and a
/// scope with no wildcard covers itself and everything under it.
pub fn covers(scope: &str, path: &str) -> bool {
    let path = path.trim_start_matches("./");
    if !scope.contains(['*', '?']) {
        return path == scope || path.starts_with(&format!("{scope}/"));
    }
    glob(scope.as_bytes(), path.as_bytes())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
            (0..=s.len()).any(|i| glob(rest, &s[i..]))
        }
        Some(b'*') => (0..=s.len())
            .take_while(|i| *i == 0 || s[i - 1] != b'/')
            .any(|i| glob(&p[1..], &s[i..])),
        Some(b'?') => !s.is_empty() && s[0] != b'/' && glob(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && glob(&p[1..], &s[1..]),
    }
}

/// Could two scopes name the same thing? Topics by equality; paths by covering when both are
/// literal, by their literal prefixes otherwise, which errs towards refusing.
pub fn overlap(a: &str, b: &str) -> bool {
    match (is_path(a), is_path(b)) {
        (false, false) => a == b,
        (true, true) => {
            if !a.contains(['*', '?']) && !b.contains(['*', '?']) {
                return covers(a, b) || covers(b, a);
            }
            let (pa, pb) = (literal_prefix(a), literal_prefix(b));
            pa.starts_with(pb) || pb.starts_with(pa)
        }
        _ => false,
    }
}

/// One project's claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    next: u64,
    rows: BTreeMap<u64, Claim>,
}

impl Claims {
    /// Rebuilt from the store's rows.
    pub fn from_rows(rows: Vec<Claim>) -> Claims {
        let mut c = Claims::default();
        for r in rows {
            c.next = c.next.max(r.id);
            c.rows.insert(r.id, r);
        }
        c
    }

    pub fn all(&self) -> impl Iterator<Item = &Claim> {
        self.rows.values()
    }

    pub fn live(&self, now: u64) -> Vec<&Claim> {
        self.rows.values().filter(|c| c.live(now)).collect()
    }

    pub fn live_of(&self, owner: &str, now: u64) -> Vec<&Claim> {
        self.rows
            .values()
            .filter(|c| c.owner == owner && c.live(now))
            .collect()
    }

    fn find_live(&self, scope: &str, now: u64) -> Option<u64> {
        self.rows
            .values()
            .find(|c| c.scope == scope && c.live(now))
            .map(|c| c.id)
    }

    fn trim(&mut self) {
        let ended: Vec<u64> = self
            .rows
            .values()
            .filter(|c| c.ended.is_some())
            .map(|c| c.id)
            .collect();
        for id in ended.iter().take(ended.len().saturating_sub(ENDED_KEEP)) {
            self.rows.remove(id);
        }
    }

    /// Claim `scope`, or renew the owner's own claim on it. An overlapping live claim of
    /// somebody else's is refused, naming the holder.
    pub fn own(
        &mut self,
        owner: &str,
        scope: &str,
        ttl: Option<u64>,
        now: u64,
    ) -> Result<Claim, String> {
        let scope = normalise(scope);
        if scope.is_empty() {
            return Err("a claim needs a scope: a path glob (`crates/room/**`) or a topic".into());
        }
        let ttl = match ttl {
            None | Some(0) => OWN_TTL_DEFAULT_MS,
            Some(t) => t.min(OWN_TTL_MAX_MS),
        };
        if let Some(theirs) = self
            .rows
            .values()
            .find(|c| c.live(now) && c.owner != owner && overlap(&c.scope, &scope))
        {
            return Err(format!(
                "`{scope}` overlaps `{}`, owned by `{}` for {} ({} left) - ask them, or wait for it \
                 to expire",
                theirs.scope,
                theirs.owner,
                duration(now.saturating_sub(theirs.since_ms)),
                duration(theirs.left_ms(now))
            ));
        }
        if let Some(id) = self.find_live(&scope, now) {
            let c = self.rows.get_mut(&id).expect("found");
            c.renewed_ms = now;
            c.ttl_ms = ttl;
            return Ok(c.clone());
        }
        self.next += 1;
        let c = Claim {
            id: self.next,
            scope,
            owner: owner.to_string(),
            since_ms: now,
            renewed_ms: now,
            ttl_ms: ttl,
            hand: None,
            ended: None,
            flagged: false,
        };
        self.rows.insert(c.id, c.clone());
        Ok(c)
    }

    fn mine(&mut self, owner: &str, scope: &str, now: u64) -> Result<&mut Claim, String> {
        let scope = normalise(scope);
        match self.find_live(&scope, now) {
            Some(id) if self.rows[&id].owner == owner => Ok(self.rows.get_mut(&id).expect("found")),
            Some(id) => Err(format!(
                "`{scope}` is `{}`'s, not yours",
                self.rows[&id].owner
            )),
            None => Err(format!("nobody owns `{scope}` now")),
        }
    }

    /// Offer the scope to `to`. The owner keeps it until `to` accepts.
    pub fn hand(
        &mut self,
        owner: &str,
        scope: &str,
        to: &str,
        note: &str,
        now: u64,
    ) -> Result<Claim, String> {
        if to == owner {
            return Err("a handover goes to another seat".into());
        }
        let c = self.mine(owner, scope, now)?;
        c.hand = Some(Hand {
            to: to.to_string(),
            note: note.trim().to_string(),
            at_ms: now,
            seq: None,
        });
        c.renewed_ms = now;
        Ok(c.clone())
    }

    /// Note the message that told the receiver of a handover.
    pub fn hand_told(&mut self, id: u64, seq: u64) -> Option<Claim> {
        let c = self.rows.get_mut(&id)?;
        if let Some(h) = c.hand.as_mut() {
            h.seq = Some(seq);
        }
        Some(c.clone())
    }

    /// Take a scope handed to `me`. Returns the claim and who held it before.
    pub fn accept(
        &mut self,
        me: &str,
        scope: &str,
        now: u64,
    ) -> Result<(Claim, String, Option<u64>), String> {
        let scope = normalise(scope);
        let Some(id) = self.find_live(&scope, now) else {
            return Err(format!(
                "nobody owns `{scope}` now, so there is nothing to accept"
            ));
        };
        let c = self.rows.get_mut(&id).expect("found");
        let Some(hand) = c.hand.clone().filter(|h| h.to == me) else {
            return Err(format!("`{scope}` was not handed to you"));
        };
        let was = std::mem::replace(&mut c.owner, me.to_string());
        c.hand = None;
        c.since_ms = now;
        c.renewed_ms = now;
        c.flagged = false;
        Ok((c.clone(), was, hand.seq))
    }

    /// End a claim: its owner, or the operator's seat (`wide`), or the receiver declining a hand.
    pub fn release(
        &mut self,
        me: &str,
        scope: &str,
        wide: bool,
        now: u64,
    ) -> Result<Claim, String> {
        let scope = normalise(scope);
        let Some(id) = self.find_live(&scope, now) else {
            return Err(format!("nobody owns `{scope}` now"));
        };
        let c = self.rows.get_mut(&id).expect("found");
        if c.hand.as_ref().is_some_and(|h| h.to == me) {
            c.hand = None;
            return Ok(c.clone());
        }
        if c.owner != me && !wide {
            return Err(format!("`{scope}` is `{}`'s, not yours", c.owner));
        }
        c.ended = Some(Ended::Released {
            by: me.to_string(),
            at_ms: now,
        });
        let out = c.clone();
        self.trim();
        Ok(out)
    }

    /// Renew `owner`'s path claims covering a file it edited after their last renewal, and its
    /// topic claims named by the row it has in progress. Returns the claims that moved.
    pub fn renew(
        &mut self,
        owner: &str,
        edits: &[(String, u64)],
        row: &str,
        now: u64,
    ) -> Vec<Claim> {
        let row = row.to_lowercase();
        let mut moved = Vec::new();
        for c in self.rows.values_mut() {
            if c.owner != owner || !c.live(now) {
                continue;
            }
            let at = if is_path(&c.scope) {
                edits
                    .iter()
                    .filter(|(p, at)| *at > c.renewed_ms && covers(&c.scope, p))
                    .map(|(_, at)| (*at).min(now))
                    .max()
            } else {
                (!row.is_empty() && row.contains(&c.scope)).then_some(now)
            };
            if let Some(at) = at.filter(|at| *at > c.renewed_ms) {
                c.renewed_ms = at;
                moved.push(c.clone());
            }
        }
        moved
    }

    /// Mark the claims past their TTL ended, and return them.
    pub fn sweep(&mut self, now: u64) -> Vec<Claim> {
        let mut out = Vec::new();
        for c in self.rows.values_mut() {
            if c.ended.is_none() && !c.live(now) {
                c.ended = Some(Ended::Expired { at_ms: now });
                out.push(c.clone());
            }
        }
        if !out.is_empty() {
            self.trim();
        }
        out
    }

    /// Set or clear the stopped-owner flag; returns the claim when it changed.
    pub fn flag(&mut self, id: u64, on: bool) -> Option<Claim> {
        let c = self.rows.get_mut(&id)?;
        (c.flagged != on).then(|| {
            c.flagged = on;
            c.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glob_covers_its_files_and_a_literal_its_tree() {
        assert!(covers("crates/room/**", "crates/room/src/own.rs"));
        assert!(covers("crates/*/Cargo.toml", "crates/room/Cargo.toml"));
        assert!(!covers("crates/*/Cargo.toml", "crates/room/x/Cargo.toml"));
        assert!(covers("crates/room", "crates/room/src/a.rs"));
        assert!(!covers("crates/room", "crates/roomx/a.rs"));
        assert!(covers("docs/*.md", "./docs/room.md"));
    }

    #[test]
    fn overlap_is_by_cover_for_paths_and_equality_for_topics() {
        assert!(overlap("crates/room/**", "crates/room/src/own.rs"));
        assert!(overlap("crates/room", "crates/room/src"));
        assert!(!overlap("crates/room", "crates/roomx"));
        assert!(!overlap("crates/tui/**", "crates/room/**"));
        assert!(overlap("flaky-gate", "flaky-gate"));
        assert!(!overlap("flaky-gate", "crates/flaky-gate/**"));
    }

    #[test]
    fn an_overlapping_live_claim_is_refused_naming_the_holder_and_expiry_frees_it() {
        let mut c = Claims::default();
        c.own("s-a", "crates/room/**", Some(60_000), 0).unwrap();
        let why = c
            .own("s-b", "crates/room/src/own.rs", None, 10_000)
            .unwrap_err();
        assert!(why.contains("owned by `s-a` for 10s (50s left)"), "{why}");
        // Its own claim again is a renewal, not a second claim.
        let again = c
            .own("s-a", "./crates/room/**", Some(60_000), 20_000)
            .unwrap();
        assert_eq!((again.id, again.renewed_ms), (1, 20_000));
        assert!(c.sweep(79_999).is_empty());
        assert_eq!(c.sweep(80_000).len(), 1);
        assert!(c.own("s-b", "crates/room/src/own.rs", None, 80_001).is_ok());
    }

    #[test]
    fn a_handover_is_held_by_the_old_owner_until_the_receiver_accepts() {
        let mut c = Claims::default();
        c.own("s-a", "gate", None, 0).unwrap();
        assert!(
            c.accept("s-b", "gate", 1)
                .unwrap_err()
                .contains("not handed to you")
        );
        c.hand("s-a", "gate", "s-b", "it is yours", 2).unwrap();
        assert_eq!(c.live_of("s-a", 3).len(), 1, "still s-a's while pending");
        assert!(c.own("s-b", "gate", None, 4).is_err());
        let (got, was, _) = c.accept("s-b", "gate", 5).unwrap();
        assert_eq!((got.owner.as_str(), was.as_str()), ("s-b", "s-a"));
        assert!(c.release("s-a", "gate", false, 6).is_err());
        assert!(c.release("s-b", "gate", false, 7).is_ok());
        assert!(c.live(8).is_empty());
    }

    #[test]
    fn edits_in_scope_and_a_row_naming_the_topic_renew() {
        let mut c = Claims::default();
        c.own("s-a", "crates/room/**", Some(60_000), 0).unwrap();
        c.own("s-a", "gate", Some(60_000), 0).unwrap();
        let edits = vec![("crates/room/src/own.rs".to_string(), 30_000)];
        let moved = c.renew("s-a", &edits, "Fix the GATE flake", 40_000);
        assert_eq!(moved.len(), 2);
        assert!(
            c.renew("s-a", &edits, "", 41_000).is_empty(),
            "an old edit renews once"
        );
        assert!(
            c.renew("s-b", &edits, "gate", 42_000).is_empty(),
            "only the owner's work"
        );
        assert_eq!(
            c.sweep(90_000).len(),
            1,
            "the path claim lives to 90s, the topic to 100s"
        );
    }
}
