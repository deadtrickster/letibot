//! What would be reaped and why - the read-only plan (`docs/lifecycle.md` d.6 step 4, d.9).
//!
//! Nothing here acts. A verdict is a sentence and a time, and G1's whole contract is that running
//! this changes neither the store nor the disk: the pass reads what is there, judges it from the
//! owner's state, and says so. The hands are G2 (compute), G3 (work) and G4 (caches and records).

use std::collections::BTreeMap;

use letibot_tokencore::store::{
    HoldingClass, HoldingState, OwnerKind, OwnerRow, OwnerState, Result as StoreResult, Store,
};

use super::{Found, Places, discover_all, human_bytes, human_millis, unowned};
use crate::exec::scope::ScopeUse;

/// How long a closed owner's work is kept once the queue proved it landed (`docs/lifecycle.md`
/// d.3's settle triggers).
pub const LANDED_GRACE_MS: i64 = 60 * 60 * 1000;

/// How long a closed, unlanded owner's work is kept: a day to fetch or restore it, then it is
/// archived and removed.
pub const UNLANDED_GRACE_MS: i64 = 24 * 60 * 60 * 1000;

/// What the plan says about one holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It would go now, and why.
    Now(String),
    /// It would go at that instant, unix ms, and why.
    At(i64, String),
    /// It stays, and why. A verdict that reaps nothing is a verdict: "not being able to tell is a
    /// reason to keep".
    Keep(String),
}

impl Verdict {
    /// `reap now`, `reap at`, `keep`: the word a row leads with.
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::Now(_) => "reap now",
            Verdict::At(_, _) => "reap at",
            Verdict::Keep(_) => "keep",
        }
    }

    /// The sentence after the word.
    pub fn said(&self) -> &str {
        match self {
            Verdict::Now(why) | Verdict::At(_, why) | Verdict::Keep(why) => why,
        }
    }

    /// Would anything be removed by this row? `false` for every row G1 itself can produce on a
    /// live owner, which is what the package's tests assert.
    pub fn reaps(&self) -> bool {
        matches!(self, Verdict::Now(_) | Verdict::At(_, _))
    }
}

/// One holding as the plan prints it: the store's row merged with what this pass found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldingView {
    pub kind: String,
    pub class: HoldingClass,
    pub host: Option<String>,
    pub handle: String,
    pub state: HoldingState,
    pub stamp: String,
    pub bytes: Option<u64>,
    /// Where its content was preserved, once it was.
    pub archive: Option<String>,
    /// What the resource itself says: a live VM's pid and tap, a cgroup's process count and memory.
    pub why: Option<String>,
    /// The cgroup's own numbers, when this holding IS one the kernel measures: `memory.current`,
    /// `memory.peak` and the OOM counters. `None` for a row the plan could only read from the
    /// store, which is the absence of a measurement rather than a zero.
    pub memory: Option<ScopeUse>,
    /// What would happen to it, and why.
    pub verdict: Verdict,
}

impl HoldingView {
    /// `6.1G`, or `?` for a size nobody measured - which is said rather than shown as nothing.
    pub fn size(&self) -> String {
        self.bytes.map(human_bytes).unwrap_or_else(|| "?".into())
    }

    /// One line: `worktree <path> 38G keep — …`.
    pub fn line(&self) -> String {
        // The HANDLE is in the line: a plan row about a worktree that does not say which worktree
        // cannot be acted on (`docs/lifecycle.md` d.9's line names the thing, `wt/pk/r193`).
        let mut out = format!(
            "{} {} {} {}",
            self.kind,
            self.handle,
            self.size(),
            self.verdict.word()
        );
        if let Verdict::At(at, _) = &self.verdict {
            out.push_str(&format!(" (at {})", stamp_of(*at)));
        }
        out.push_str(" — ");
        out.push_str(self.verdict.said());
        if let Some(why) = &self.why
            && !why.is_empty()
            && !self.verdict.said().contains(why.as_str())
        {
            out.push_str(&format!(" [{why}]"));
        }
        out
    }
}

/// One owner and everything the plan says it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerView {
    pub id: String,
    pub kind: OwnerKind,
    pub state: OwnerState,
    pub repo: Option<String>,
    pub closed_ms: Option<i64>,
    /// Held by the operator until, unix ms (`0` for ever).
    pub held_until_ms: Option<i64>,
    /// The idle TTL the owner row names, if any: what `closes in` is measured from.
    pub ttl_ms: Option<i64>,
    /// The last activity, from the owner row: an idle span is measured from it.
    pub lease_ms: i64,
    pub holds: Vec<HoldingView>,
}

impl OwnerView {
    /// The bytes this owner holds, all classes together.
    pub fn bytes(&self) -> u64 {
        self.holds.iter().filter_map(|h| h.bytes).sum()
    }

    /// What the kernel says this owner holds in memory right now, and the most it ever did: its
    /// session cgroup and its VM's run cgroup, read from `memory.current`/`memory.peak` by the
    /// discovery walk (G5, `docs/lifecycle.md` d.7). `(0, 0)` is an owner that runs nothing, which
    /// is a fact; a cgroup nobody could read is simply not counted and says so on its own row.
    pub fn memory(&self) -> (u64, u64) {
        let mut now = 0;
        let mut peak = 0;
        for h in &self.holds {
            if matches!(h.state, HoldingState::Gone) || !matches!(h.kind.as_str(), "scope" | "vm") {
                continue;
            }
            if let Some(u) = &h.memory {
                now += u.memory_current.unwrap_or(0);
                peak += u.memory_peak.unwrap_or(0);
            }
        }
        (now, peak)
    }

    /// `session s-… (live, 3 procs)` - the heading of its block in the plan.
    pub fn heading(&self, now_ms: i64) -> String {
        let mut out = format!("{} {}", self.kind.noun(), self.id);
        out.push_str(&format!(" ({})", self.state.word()));
        if self.kind != OwnerKind::Session {
            out.push_str(&format!(", idle {}", human_millis(self.idle_for(now_ms))));
        }
        if let Some(until) = self.held_until_ms {
            out.push_str(&match until {
                0 => ", held for ever by the operator".to_string(),
                u => format!(
                    ", held by the operator for {}",
                    human_millis((u - now_ms).max(0) as u64)
                ),
            });
        }
        out
    }

    /// How long since this owner last did anything: the lease is the clock an idle TTL is measured
    /// from, so it is also the number a caller asks for.
    pub fn idle_for(&self, now_ms: i64) -> u64 {
        (now_ms - self.lease_ms).max(0) as u64
    }

    /// The single `holds:` line (`docs/lifecycle.md` d.9), which `--status`, `leticode_status` and
    /// `letibot holds` all print: what this owner holds, its sizes, and how long it has been quiet.
    pub fn holds_line(&self, now_ms: i64) -> String {
        let mut parts: Vec<String> = Vec::new();
        for h in &self.holds {
            if matches!(h.state, HoldingState::Gone) {
                continue;
            }
            parts.push(match h.kind.as_str() {
                "scope" => format!("scope {}", h.why.clone().unwrap_or_default()),
                "vm" => format!("vm {} ({})", h.handle, h.size()),
                "daemon" => format!("daemon {}", h.handle),
                // d.9's line names the thing - `wt pk/r193`, not a bare size - because the reader
                // of a `holds:` row is deciding what to settle.
                "worktree" => format!(
                    "wt {} ({})",
                    std::path::Path::new(&h.handle)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| h.handle.clone()),
                    h.size()
                ),
                "branch" => format!("branch {} ({})", h.handle, h.size()),
                other => format!("{other} {}", h.size()),
            });
        }
        let mut line = format!("holds: {}", parts.join(", "));
        if parts.is_empty() {
            line = "holds: nothing on this box".to_string();
        }
        line.push_str(&format!(" · idle {}", human_millis(self.idle_for(now_ms))));
        if self.state.compute_gone() {
            line.push_str(&format!(", {} (no compute)", self.state.word()));
        } else if let Some(ttl) = self.idle_ttl_ms() {
            let left = ttl - self.idle_for(now_ms) as i64;
            if left > 0 && self.kind != OwnerKind::Session {
                line.push_str(&format!(", closes in {}", human_millis(left as u64)));
            }
        }
        line
    }

    /// The idle TTL the owner row names, if any.
    pub fn idle_ttl_ms(&self) -> Option<i64> {
        self.ttl_ms
    }
}

/// A directory or a cache directory that names no owner this store knows: reported, never reaped
/// (`docs/lifecycle.md` d.4 rules 3 and 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stranger {
    pub handle: String,
    pub owner: String,
    pub kind: String,
    pub bytes: Option<u64>,
    pub why: String,
}

impl Stranger {
    pub fn line(&self) -> String {
        format!(
            "{} {} — {}",
            self.kind,
            self.bytes.map(human_bytes).unwrap_or_else(|| "?".into()),
            self.why
        )
    }
}

/// The whole plan: every owner on this box, then what the places hold that no owner claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub owners: Vec<OwnerView>,
    /// Directories and copies that look like letibot's and name no owner the store knows.
    pub looks_like_ours: Vec<Stranger>,
    /// The instant the plan was built.
    pub now_ms: i64,
    /// What the user manager holds, when `letibot gc` asked it: the unit table is G5's, and
    /// `holds` does not ask, because a plan a model reads on every turn must not shell out.
    pub systemd: Option<crate::lifecycle::UnitTable>,
    /// Why the manager could not be asked, when it could not. A box with no user manager is a
    /// fact the plan prints - not a reason for it to fail.
    pub systemd_why: Option<String>,
}

impl Plan {
    /// The bytes each class holds, summed over every owner.
    pub fn totals(&self) -> BTreeMap<&'static str, u64> {
        let mut out: BTreeMap<&'static str, u64> = BTreeMap::new();
        for o in &self.owners {
            for h in &o.holds {
                if matches!(h.state, HoldingState::Gone) {
                    continue;
                }
                *out.entry(h.class.word()).or_default() += h.bytes.unwrap_or(0);
            }
        }
        out
    }

    /// How many holdings the plan would remove, and their bytes: the one number that says whether a
    /// plan is worth acting on, and `0` for the only answer G1 can give about a live session.
    pub fn would_reap(&self) -> (usize, u64) {
        let mut n = 0;
        let mut bytes = 0;
        for o in &self.owners {
            for h in o.holds.iter().filter(|h| h.verdict.reaps()) {
                n += 1;
                bytes += h.bytes.unwrap_or(0);
            }
        }
        (n, bytes)
    }

    /// `letibot holds --top`: owners by the memory they hold, biggest first, and then the box's own
    /// `MemAvailable` - the number that says whether the biggest of them is why the box is short.
    /// Nobody's cgroup is guessed at: an owner with none is `0 B`, which is what it runs.
    pub fn top(&self) -> String {
        let mut rows: Vec<(&str, &str, u64, u64)> = self
            .owners
            .iter()
            .map(|o| {
                let (now, peak) = o.memory();
                (o.kind.word(), o.id.as_str(), now, peak)
            })
            .collect();
        rows.sort_by_key(|(_, _, now, peak)| std::cmp::Reverse((*now, *peak)));
        let mut out = String::new();
        for (kind, id, now, peak) in &rows {
            out.push_str(&format!(
                "{:>10} now  {:>10} peak  {kind} {id}\n",
                human_bytes(*now),
                human_bytes(*peak)
            ));
        }
        match crate::exec::memory::Meminfo::read() {
            Some(m) => out.push_str(&format!(
                "box: {} available of {} · {} owner(s) measured\n",
                human_bytes(m.available),
                human_bytes(m.total),
                rows.len()
            )),
            None => out.push_str(
                "box: `/proc/meminfo` cannot be read here, so this box's free memory is unknown\n",
            ),
        }
        out
    }

    /// The operator's plan, grouped by owner, one line per holding, then the strangers
    /// (`docs/lifecycle.md` d.9). A plan that removed nothing says so in as many words.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.owners.is_empty() {
            out.push_str("no owners in this store; nothing to plan\n");
        }
        for o in &self.owners {
            out.push_str(&format!("\n{}\n", o.heading(self.now_ms)));
            if o.holds.is_empty() {
                out.push_str("  (nothing found on this box)\n");
            }
            for h in &o.holds {
                out.push_str(&format!("  {}\n", h.line()));
            }
        }
        if !self.looks_like_ours.is_empty() {
            out.push_str(
                "\nunrecorded — looks like ours, and nothing here proves whose. Not reaped by \
                 anything; `letibot gc --adopt PATTERN` takes one under an owner:\n",
            );
            for s in &self.looks_like_ours {
                out.push_str(&format!("  {} — {}\n", s.handle, s.line()));
            }
        }
        let (n, bytes) = self.would_reap();
        let totals = self
            .totals()
            .into_iter()
            .map(|(class, b)| format!("{class} {}", human_bytes(b)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\nwould be reaped: {} ({})\nheld: {totals}\n",
            match n {
                0 => "nothing".to_string(),
                1 => "1 holding".to_string(),
                n => format!("{n} holdings"),
            },
            human_bytes(bytes)
        ));
        out.push_str("this is a plan: nothing was removed, and nothing was stopped.\n");
        if let Some(t) = &self.systemd {
            out.push_str(&format!("{}\n", t.line()));
        } else if let Some(why) = &self.systemd_why {
            out.push_str(&format!("user manager: {why}\n"));
        }
        out
    }

    /// The plan as JSON: what a bridge tool answers, so an agent reads the same facts the operator
    /// prints without parsing prose.
    pub fn to_json(&self) -> serde_json::Value {
        let owners: Vec<serde_json::Value> = self
            .owners
            .iter()
            .map(|o| {
                serde_json::json!({
                    "owner": o.id,
                    "kind": o.kind.word(),
                    "state": o.state.word(),
                    "repo": o.repo,
                    "closed_ms": o.closed_ms,
                    "held_until_ms": o.held_until_ms,
                    "idle_ms": o.idle_for(self.now_ms),
                    "bytes": o.bytes(),
                    "holds": o.holds.iter().map(|h| serde_json::json!({
                        "kind": h.kind,
                        "class": h.class.word(),
                        "host": h.host,
                        "handle": h.handle,
                        "state": h.state.word(),
                        "stamp": h.stamp,
                        "bytes": h.bytes,
                        "size": h.size(),
                        "archive": h.archive,
                        "why": h.why,
                        "verdict": h.verdict.word(),
                        "verdict_said": h.verdict.said(),
                        "reap_at_ms": match &h.verdict {
                            Verdict::At(at, _) => Some(*at),
                            _ => None,
                        },
                    })).collect::<Vec<_>>(),
                    "holds_line": o.holds_line(self.now_ms),
                })
            })
            .collect();
        let (reaping, bytes) = self.would_reap();
        serde_json::json!({
            "now_ms": self.now_ms,
            "owners": owners,
            "looks_like_ours": self.looks_like_ours.iter().map(|s| serde_json::json!({
                "handle": s.handle,
                "owner": s.owner,
                "kind": s.kind,
                "bytes": s.bytes,
                "why": s.why,
            })).collect::<Vec<_>>(),
            "would_reap": { "holdings": reaping, "bytes": bytes },
            "totals": self.totals().into_iter()
                .map(|(c, b)| (c.to_string(), b))
                .collect::<BTreeMap<String, u64>>(),
            "systemd": self.systemd.as_ref().map(|t| serde_json::json!({
                "units": t.units,
                "limit": crate::lifecycle::systemd::UNIT_LIMIT,
                "ours_failed": t.ours_failed,
                "other_failed": t.other_failed,
                "line": t.line(),
            })),
            "systemd_why": self.systemd_why,
        })
    }
}

/// Build the plan: every owner in `store` (or the one named), its holdings from the rows merged
/// with a fresh discovery walk, and whatever the places hold that no owner claims. Reads only.
pub fn plan(store: &Store, places: &Places, only: Option<&str>, now_ms: i64) -> StoreResult<Plan> {
    let owners: Vec<OwnerRow> = match only {
        Some(id) => store.owner(id)?.into_iter().collect(),
        None => store.owners()?,
    };
    let known: std::collections::BTreeSet<String> =
        store.owners()?.into_iter().map(|o| o.id).collect();
    let mut out = Plan {
        owners: Vec::new(),
        looks_like_ours: Vec::new(),
        now_ms,
        systemd: None,
        systemd_why: None,
    };
    for owner in &owners {
        let rows = store.holdings_of(&owner.id)?;
        let found: BTreeMap<(String, Option<String>, String), Found> =
            discover_all(store, owner, places)
                .into_iter()
                .map(|f| ((f.kind.clone(), f.host.clone(), f.handle.clone()), f))
                .collect();
        let mut holds: Vec<HoldingView> = Vec::new();
        for row in &rows {
            let key = (row.kind.clone(), row.host.clone(), row.handle.clone());
            let view = match found.get(&key) {
                Some(f) => HoldingView {
                    kind: row.kind.clone(),
                    class: row.class,
                    host: row.host.clone(),
                    handle: row.handle.clone(),
                    state: HoldingState::Held,
                    stamp: f.stamp.clone(),
                    bytes: f.bytes.or(row.bytes),
                    archive: row.archive.clone(),
                    why: f.why.clone().or_else(|| row.why.clone()),
                    memory: f.use_,
                    verdict: Verdict::Keep(String::new()),
                },
                None => HoldingView {
                    kind: row.kind.clone(),
                    class: row.class,
                    host: row.host.clone(),
                    handle: row.handle.clone(),
                    state: HoldingState::Gone,
                    stamp: row.stamp.clone(),
                    bytes: row.bytes,
                    archive: row.archive.clone(),
                    why: row.why.clone(),
                    memory: None,
                    verdict: Verdict::Keep(String::new()),
                },
            };
            holds.push(view);
        }
        // Found and not in the store: a resource the index has not learned yet. A plan reports it
        // as it is rather than hiding it - that is the case `refresh` exists to write down.
        for ((kind, host, handle), f) in &found {
            if rows
                .iter()
                .any(|r| r.kind == *kind && r.host == *host && r.handle == *handle)
            {
                continue;
            }
            holds.push(HoldingView {
                kind: kind.clone(),
                class: f.class,
                host: host.clone(),
                handle: handle.clone(),
                state: HoldingState::Held,
                stamp: f.stamp.clone(),
                bytes: f.bytes,
                archive: None,
                why: f.why.clone(),
                memory: f.use_,
                verdict: Verdict::Keep(String::new()),
            });
        }
        for h in &mut holds {
            h.verdict = judge(owner, h, places, now_ms);
        }
        let view = OwnerView {
            id: owner.id.clone(),
            kind: owner.kind,
            state: owner.state,
            repo: owner.repo.clone(),
            closed_ms: owner.closed_ms,
            held_until_ms: owner.held_until_ms,
            ttl_ms: owner.idle_ttl_ms,
            lease_ms: owner.lease_ms,
            holds,
        };
        out.owners.push(view);
    }
    out.looks_like_ours = unowned(places, &known)
        .into_iter()
        .map(|f| Stranger {
            handle: f.handle,
            owner: f.owner,
            kind: f.kind,
            bytes: f.bytes,
            why: f
                .why
                .unwrap_or_else(|| "it looks like ours and nothing proves whose".into()),
        })
        .collect();
    Ok(out)
}

/// Whether one holding would be reaped, and why. Pure, so every case is asserted without a disk:
/// the owner's state and the holding's own are the only inputs that decide, which is what makes
/// "a live owner loses nothing" a property rather than a hope.
pub fn judge(owner: &OwnerRow, h: &HoldingView, places: &Places, now_ms: i64) -> Verdict {
    if matches!(h.state, HoldingState::Gone) {
        return Verdict::Keep(
            "its row is here and the resource is not on this box: nothing to remove".into(),
        );
    }
    if h.state == HoldingState::Stuck {
        return Verdict::Keep(format!(
            "a reap step failed twice on this {kind}, so nothing further is removed for it until \
             `letibot gc --log {owner}` is read and settled",
            kind = h.kind,
            owner = owner.id,
        ));
    }
    if let Some(host) = &h.host
        && host != &places.box_id
    {
        return Verdict::Keep(format!(
            "it is on `{host}`: that host's own pass reads it (G4), and this box cannot see it"
        ));
    }
    if owner.held(now_ms) {
        return Verdict::Keep(match owner.held_until_ms {
            Some(0) => "the operator holds this owner for ever".to_string(),
            Some(until) => format!(
                "the operator holds this owner for another {}",
                human_millis((until - now_ms).max(0) as u64)
            ),
            None => "the operator holds this owner".to_string(),
        });
    }
    match owner.state {
        OwnerState::Live => Verdict::Keep(format!(
            "its owner is `live`: the {} is in use, and nothing here acts on a live owner",
            h.kind
        )),
        OwnerState::Closing => Verdict::Keep(
            "its owner is `closing`: its compute is being reaped by the daemon that closed it"
                .into(),
        ),
        OwnerState::Stuck => Verdict::Keep(format!(
            "its owner is `stuck` — a reap step failed twice, so nothing further is removed until \
             `letibot gc --log {}` is read and settled",
            owner.id
        )),
        OwnerState::Closed | OwnerState::Settling | OwnerState::Settled | OwnerState::Swept => {
            match h.class {
                HoldingClass::Compute => Verdict::Now(format!(
                    "its owner is `{}`: compute ends at close, and the session can be reopened",
                    owner.state.word()
                )),
                HoldingClass::Cache => Verdict::Now(format!(
                    "its owner is `{}`: a cache is rebuilt, never kept past the owner",
                    owner.state.word()
                )),
                HoldingClass::Work => {
                    let base = owner.closed_ms.unwrap_or(now_ms);
                    let (grace, why) = match &owner.landed_sha {
                        Some(sha) => (
                            LANDED_GRACE_MS,
                            format!("its owner's work landed at {}", short(sha)),
                        ),
                        None => (
                            UNLANDED_GRACE_MS,
                            "its owner's work is not proved landed".to_string(),
                        ),
                    };
                    let at = base + grace;
                    let archive = format!(
                        "its work is archived to `refs/letibot/archive/{}/` and read back first",
                        owner.id
                    );
                    if owner.state == OwnerState::Settled || owner.state == OwnerState::Swept {
                        return Verdict::Now(format!(
                            "its owner is `{}` — {archive}",
                            owner.state.word()
                        ));
                    }
                    if now_ms >= at {
                        Verdict::Now(format!("{why} and the grace is over — {archive}"))
                    } else {
                        Verdict::At(at, format!("{why} — {archive}"))
                    }
                }
                HoldingClass::Record => Verdict::Keep(format!(
                    "a record outlives its owner: `letibot gc` archives one and never deletes it on \
                 its own ({})",
                    owner.state.word()
                )),
            }
        }
    }
}

/// A sha as a line says it: the first eight, which is what `git log --oneline` used first.
fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// An instant as the plan prints it: `2026-11-13 04:12Z`, from unix ms. Written by hand rather
/// than pulled in with a date crate: one format, one place, no dependency.
pub fn stamp_of(at_ms: i64) -> String {
    let secs = at_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let (y, m, d) = civil_of(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60
    )
}

/// Days since the epoch to a civil date, by the one algorithm that needs no table
/// (Howard Hinnant's `civil_from_days`).
fn civil_of(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::Settings;

    fn places() -> Places {
        Places {
            gc_root: "/nonexistent/gc".into(),
            box_id: "lab2x1".into(),
            settings: Settings::default(),
            data_dir: "/nonexistent/data".into(),
            rundir: "/nonexistent/run".into(),
            firecode_runs: None,
            firecode_cache: None,
            proc_root: "/nonexistent/proc".into(),
            cgroup_fs: "/nonexistent/cgroup".into(),
            docker_engine: None,
        }
    }

    fn owner(state: OwnerState) -> OwnerRow {
        OwnerRow {
            state,
            closed_ms: Some(1_000_000),
            ..OwnerRow::live("s-1", OwnerKind::Package, Some("/w".into()), 0)
        }
    }

    fn holding(class: HoldingClass, kind: &str) -> HoldingView {
        HoldingView {
            kind: kind.into(),
            class,
            host: None,
            handle: format!("/w/{kind}"),
            state: HoldingState::Held,
            stamp: "a stamp".into(),
            bytes: Some(1024),
            archive: None,
            why: None,
            memory: None,
            verdict: Verdict::Keep(String::new()),
        }
    }

    /// An owner's memory is its session cgroup plus its VM's run cgroup, both read from the kernel:
    /// `holds --top` orders owners by that sum, biggest first, and prints the box's own available
    /// memory beside it (G5). An owner whose cgroup the walk could not read is not guessed at.
    #[test]
    fn the_top_view_sums_the_cgroups_and_puts_the_biggest_owner_first() {
        let use_ = |now: u64, peak: u64| crate::exec::scope::ScopeUse {
            populated: true,
            procs: 2,
            memory_current: Some(now),
            memory_peak: Some(peak),
            ..Default::default()
        };
        let with = |id: &str, kind: OwnerKind, holds: Vec<HoldingView>| OwnerView {
            id: id.into(),
            kind,
            state: OwnerState::Live,
            repo: None,
            closed_ms: None,
            held_until_ms: None,
            ttl_ms: None,
            lease_ms: 0,
            holds,
        };
        let mut small = holding(HoldingClass::Compute, "scope");
        small.memory = Some(use_(1 << 30, 2 << 30));
        let mut big_scope = holding(HoldingClass::Compute, "scope");
        big_scope.memory = Some(use_(3 << 30, 4 << 30));
        let mut big_vm = holding(HoldingClass::Compute, "vm");
        big_vm.memory = Some(use_(2 << 30, 5 << 30));
        let p = Plan {
            owners: vec![
                with("s-small", OwnerKind::Package, vec![small]),
                with("s-big", OwnerKind::Package, vec![big_scope, big_vm]),
                // A cgroup nobody could read is not a zero it earned, and it is not counted.
                with(
                    "s-blind",
                    OwnerKind::Package,
                    vec![holding(HoldingClass::Cache, "target")],
                ),
            ],
            looks_like_ours: Vec::new(),
            now_ms: 0,
            systemd: None,
            systemd_why: None,
        };
        assert_eq!(
            p.owners[1].memory(),
            (5 << 30, 9 << 30),
            "scope + vm, current and peak"
        );
        assert_eq!(
            p.owners[2].memory(),
            (0, 0),
            "a target directory holds no memory"
        );
        let top = p.top();
        let lines: Vec<&str> = top.lines().collect();
        assert!(
            lines[0].contains("s-big"),
            "the box's biggest owner is first: {top}"
        );
        assert!(lines[1].contains("s-small"), "{top}");
        assert!(lines[2].contains("s-blind"), "{top}");
        assert!(top.contains("5 GiB now"), "{top}");
        assert!(
            top.contains("available") || top.contains("cannot be read here"),
            "the box's own answer is said, or said to be unknown: {top}"
        );
    }

    /// A live owner loses nothing: every class of holding is kept, and each says why in its own
    /// words rather than one shared silence.
    #[test]
    fn a_live_owner_keeps_every_class_of_holding() {
        let o = owner(OwnerState::Live);
        let p = places();
        for class in [
            HoldingClass::Compute,
            HoldingClass::Work,
            HoldingClass::Cache,
            HoldingClass::Record,
        ] {
            let v = judge(&o, &holding(class, "scope"), &p, 2_000_000);
            assert!(!v.reaps(), "{class:?} was reaped under a live owner");
            assert!(v.said().contains("live"), "{v:?}");
        }
    }

    /// A closed owner's compute and caches go now; its work waits out its grace; its records stay.
    #[test]
    fn a_closed_owner_loses_compute_and_caches_but_never_records() {
        let o = owner(OwnerState::Closed);
        let p = places();
        assert!(matches!(
            judge(&o, &holding(HoldingClass::Compute, "scope"), &p, 2_000_000),
            Verdict::Now(_)
        ));
        assert!(matches!(
            judge(&o, &holding(HoldingClass::Cache, "target"), &p, 2_000_000),
            Verdict::Now(_)
        ));
        let work = judge(&o, &holding(HoldingClass::Work, "worktree"), &p, 2_000_000);
        match &work {
            Verdict::At(at, why) => {
                assert_eq!(*at, 1_000_000 + UNLANDED_GRACE_MS);
                assert!(why.contains("archive"), "{why}");
            }
            other => panic!("a closed owner's unlanded work should wait its grace: {other:?}"),
        }
        let record = judge(&o, &holding(HoldingClass::Record, "run"), &p, 2_000_000);
        assert!(!record.reaps(), "a record was reaped: {record:?}");
    }

    /// Landed work waits an hour, not a day; and past its grace the verdict is `reap now`.
    #[test]
    fn the_grace_is_the_owners_and_it_ends() {
        let p = places();
        let mut o = owner(OwnerState::Closed);
        o.landed_sha = Some("0123456789abcdef".into());
        let work = holding(HoldingClass::Work, "worktree");
        let at = match judge(&o, &work, &p, 1_000_000) {
            Verdict::At(at, why) => {
                assert!(why.contains("01234567"), "{why}");
                at
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(at, 1_000_000 + LANDED_GRACE_MS);
        assert!(matches!(judge(&o, &work, &p, at + 1), Verdict::Now(_)));
    }

    /// Being unable to see a resource is a reason to keep it, and a host this box cannot reach is
    /// the same case one machine over.
    #[test]
    fn what_this_box_cannot_see_is_kept_and_said() {
        let p = places();
        let o = owner(OwnerState::Closed);
        let gone = HoldingView {
            state: HoldingState::Gone,
            ..holding(HoldingClass::Cache, "target")
        };
        assert!(!judge(&o, &gone, &p, 2_000_000).reaps());
        let elsewhere = HoldingView {
            host: Some("lubuntu2".into()),
            ..holding(HoldingClass::Work, "home")
        };
        let v = judge(&o, &elsewhere, &p, 2_000_000);
        assert!(!v.reaps());
        assert!(v.said().contains("lubuntu2"), "{v:?}");
    }

    /// The operator's hold is the one opt-out, and an expired one is said rather than acted on.
    #[test]
    fn a_hold_stops_every_trigger_except_their_own_commands() {
        let p = places();
        let mut o = owner(OwnerState::Closed);
        o.held_until_ms = Some(0);
        assert!(!judge(&o, &holding(HoldingClass::Compute, "scope"), &p, 2_000_000).reaps());
        o.held_until_ms = Some(3_000_000);
        assert!(
            !judge(&o, &holding(HoldingClass::Compute, "scope"), &p, 2_000_000).reaps(),
            "a hold that has not run out keeps everything"
        );
        o.held_until_ms = Some(1_500_000);
        let v = judge(&o, &holding(HoldingClass::Compute, "scope"), &p, 2_000_000);
        assert!(v.reaps(), "an expired hold no longer keeps it");
    }

    /// A stuck owner is loud and still: the failure is the operator's to settle, so nothing else
    /// is removed for it either.
    #[test]
    fn a_stuck_owner_keeps_everything_and_names_the_command() {
        let o = owner(OwnerState::Stuck);
        let v = judge(
            &o,
            &holding(HoldingClass::Compute, "scope"),
            &places(),
            2_000_000,
        );
        assert!(!v.reaps());
        assert!(v.said().contains("letibot gc --log"), "{v:?}");
    }

    /// The instant the plan prints is the one a person can compare with `date`.
    #[test]
    fn an_instant_is_written_the_one_way() {
        assert_eq!(stamp_of(0), "1970-01-01 00:00Z");
        assert_eq!(stamp_of(1_700_000_000_000), "2023-11-14 22:13Z");
    }
}
