//! What a session OWNS, so automode can tell its own work from a widening.
//!
//! A session owns the worktree it works in, its scratch, the system temp directories, the paths
//! its own commands created (read live off [`crate::view::ViewGrants`], which is where
//! [`crate::created`]'s findings are recorded), and its branches. A call whose every effect lands
//! there is the session doing the job it was opened for, so automode runs it with no card and no
//! model call - the operator approved essentially every card for months, and a card about the
//! session's own `/var/tmp` is the ask this screen exists to stop.
//!
//! This is a screen over layer A's reading and nothing else: it never widens a tier, it refuses
//! `AlwaysAsk` and `Blocked` outright (the always-ask list is the operator's whatever the session
//! owns), and a call it cannot place is not owned.

use std::path::{Component, Path, PathBuf};

use crate::adjudicate::{AdjudicationRequest, Tier};
use crate::intent::{Baseline, Intent, Region, UNRESOLVED_TARGET};

/// The branch prefixes that are a session's work by name: the operator's rule (2026-11-23), and
/// the two they settled on.
pub const OWN_BRANCHES: [&str; 2] = ["ik/", "wt-"];

/// Where an owned call lands, as the row records it and the card's basis reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    /// The worktree the session was opened in.
    Worktree,
    /// The session's own scratch directory.
    SessionTemp,
    /// `/tmp`, `/var/tmp` or `$TMPDIR`.
    Temp,
    /// A path one of this session's own commands created.
    Created,
}

impl Where {
    pub fn as_str(self) -> &'static str {
        match self {
            Where::Worktree => "worktree",
            Where::SessionTemp => "session temp",
            Where::Temp => "temp",
            Where::Created => "created",
        }
    }

    /// The label a row carries. One word, because the corpus counts these with a `GROUP BY`.
    pub fn label(self) -> &'static str {
        match self {
            Where::Worktree => "worktree",
            Where::SessionTemp => "session_temp",
            Where::Temp => "temp",
            Where::Created => "created",
        }
    }
}

/// What one session owns. Cheap to build per call: the roots are the session's, and the created
/// paths are read live so a path a command made a moment ago counts at the next call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owned {
    /// In the order they are checked, which is also the order they are reported in.
    roots: Vec<(Where, PathBuf)>,
    /// Paths this session's own commands created, each the top of a tree.
    created: Vec<PathBuf>,
    /// The branch the worktree has checked out, when it has one.
    branch: Option<String>,
}

/// The temp directories a session writes, which the operator counts as its own: a commit message
/// in `/var/tmp` is the session's work and not a widening.
pub const TEMP_DIRS: [&str; 2] = ["/tmp", "/var/tmp"];

impl Owned {
    /// A session's worktree, and the system temp directories.
    pub fn of(workspace: &Path) -> Owned {
        let mut owned = Owned::default();
        owned
            .roots
            .push((Where::Worktree, crate::view::key(workspace)));
        for t in TEMP_DIRS {
            owned.roots.push((Where::Temp, PathBuf::from(t)));
        }
        owned
    }

    /// The session's own scratch, which is its own whatever it is called.
    pub fn with_scratch(mut self, scratch: &Path) -> Owned {
        self.roots
            .insert(0, (Where::SessionTemp, crate::view::key(scratch)));
        self
    }

    /// [`TEMP_DIRS`] and the `TMPDIR` this session's shell actually holds.
    pub fn with_tmpdir(mut self, tmpdir: Option<PathBuf>) -> Owned {
        if let Some(p) = tmpdir.filter(|p| !p.as_os_str().is_empty()) {
            let p = crate::view::key(&p);
            if !self.roots.iter().any(|(_, r)| *r == p) {
                self.roots.push((Where::Temp, p));
            }
        }
        self
    }

    /// The paths this session's commands created, as the view record holds them.
    pub fn with_created(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Owned {
        for p in paths {
            let p = crate::view::key(&p);
            if !self.created.contains(&p) {
                self.created.push(p);
            }
        }
        self
    }

    /// The branch this worktree has checked out.
    pub fn with_branch(mut self, branch: Option<String>) -> Owned {
        self.branch = branch.filter(|b| !b.trim().is_empty());
        self
    }

    /// Every root, as the ledger keeps it and the card names it.
    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.roots.iter().map(|(_, p)| p)
    }

    /// The places a session owns, as strings, for the row's record.
    pub fn describe(&self) -> String {
        self.roots
            .iter()
            .map(|(w, p)| format!("{} {}", w.as_str(), p.display()))
            .chain(
                self.created
                    .iter()
                    .map(|p| format!("created {}", p.display())),
            )
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Where `path` sits, when this session owns it. The lexical path and its canonical key are
    /// both tried: a symlink into the worktree is still the worktree's.
    pub fn covers(&self, path: &Path) -> Option<Where> {
        let lexical = lexical(path);
        let key = crate::view::key(&lexical);
        // A path a command created is checked first: it is narrower than the temp directory it
        // usually sits in, and the row should say what actually made it the session's.
        if self.created.iter().any(|p| key.starts_with(p)) {
            return Some(Where::Created);
        }
        for at in [&lexical, &key] {
            if let Some((w, _)) = self.roots.iter().find(|(_, r)| at.starts_with(r)) {
                return Some(*w);
            }
        }
        None
    }

    /// Whether `name` is a branch this session's work: the worktree's own, or one of the
    /// operator's two patterns.
    pub fn owns_branch(&self, name: &str) -> bool {
        let name = name.trim();
        self.branch.as_deref() == Some(name) || OWN_BRANCHES.iter().any(|p| name.starts_with(p))
    }
}

/// What the screen answers: where the call lands, and the one line the row carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    pub at: Where,
    /// `None` for a call layer A read, `Some(why)` for one it could not: the operator's data says
    /// an unreadable command in the session's own worktree runs, and the row has to say so.
    pub unread: Option<String>,
}

impl Landed {
    /// The row's word: `worktree`, or `unread, owned cwd`.
    pub fn label(&self) -> String {
        match self.unread {
            None => self.at.label().to_string(),
            Some(_) => "unread_owned_cwd".to_string(),
        }
    }
}

/// `Ok` when every effect this call has lands where the session owns it.
///
/// The tier is not widened here and cannot be: only `MayApprove` is a call the ladder would have
/// put to somebody, so `AlwaysAsk` (destruction outside the project, privilege, credentials, a
/// first host) and `Blocked` (a secret about to move) are refused by name rather than admitted.
pub fn admits(
    req: &AdjudicationRequest,
    owned: &Owned,
    workspace: &Path,
) -> Result<Landed, String> {
    match &req.tier {
        // `Auto` is a call nobody would be asked about anyway; the screen is not what admits it,
        // and the gate's own ordering decides. `AlwaysAsk` and `Blocked` are the two this screen
        // must never reach past, because both are the operator's by construction.
        Tier::Auto | Tier::MayApprove => {}
        Tier::AlwaysAsk { rule, .. } => {
            return Err(format!(
                "`{rule}` is on the always-ask list, which no session owns"
            ));
        }
        Tier::Blocked { rule, .. } => {
            return Err(format!("{} is nobody's to admit", rule.as_str()));
        }
    }
    let Some(reading) = req.reading.as_ref() else {
        return Err("layer A's reading did not travel with the call".into());
    };
    if !reading.flows.is_empty() {
        return Err("it moves a secret, which is never owned".into());
    }
    if !reading.resolved {
        // The operator's own data: a command the grammar could not read that runs in the
        // session's own worktree runs. Recorded as such, so the corpus can count them.
        return owned_cwd(req, owned, workspace, reading);
    }
    effects(reading, workspace, owned).map(|at| Landed { at, unread: None })
}

/// The unread case, on its own: the worktree itself is what the command runs in, so that is what
/// has to be owned. A command whose own `cd` moved it elsewhere is not covered.
fn owned_cwd(
    req: &AdjudicationRequest,
    owned: &Owned,
    workspace: &Path,
    reading: &Baseline,
) -> Result<Landed, String> {
    if reading.intents.contains(&Intent::Network) {
        return Err("it reaches the network, which no session owns".into());
    }
    if matches!(req.class.scope, crate::adjudicate::EffectScope::External) {
        return Err("it lands outside the host".into());
    }
    let at = owned.covers(workspace).ok_or_else(|| {
        format!(
            "it could not be read and {} is not the session's",
            workspace.display()
        )
    })?;
    Ok(Landed {
        at,
        unread: Some(
            reading
                .findings
                .first()
                .cloned()
                .unwrap_or_else(|| "the grammar could not read the command".into()),
        ),
    })
}

/// Every write and destroy target, and where it lands; and, for a call that names no write, what
/// layer A could read about where it goes.
fn effects(reading: &Baseline, workspace: &Path, owned: &Owned) -> Result<Where, String> {
    if reading.intents.contains(&Intent::Network) {
        return Err("it reaches the network, which no session owns".into());
    }
    let mut placed: Option<Where> = None;
    for s in &reading.scoped {
        if !matches!(s.intent, Intent::WriteFile | Intent::Destroy) {
            continue;
        }
        let t = s.target.trim();
        if t.is_empty() || t == UNRESOLVED_TARGET {
            return Err(format!(
                "its {} names no path layer A can place",
                s.intent.as_str()
            ));
        }
        let p = Path::new(t);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(p)
        };
        let at = owned
            .covers(&abs)
            .ok_or_else(|| format!("it writes {t}, and that is not the session's"))?;
        placed = Some(at);
    }
    // A call that names no write this build could place - a read, a build, a commit - is the
    // session's own work only where layer A can say where it goes. Two ways it cannot, and both
    // are the screen refusing rather than the session being trusted:
    //
    // * the program is one this build never heard of, so its effects are unknown and a screen
    //   over them is a screen over nothing (`nmcli`, `ip`, `networkctl`);
    // * it touched a place the session does not own - the host, the operator's home, another
    //   session's tree. A read changes nothing, and it is still a read of something that is not
    //   this session's, which is what the shadow's `reads` stage exists to judge.
    match placed {
        Some(at) => Ok(at),
        None => {
            if reading.intents.contains(&Intent::Unknown) {
                return Err(
                    "layer A does not know this program, so what it does cannot be placed".into(),
                );
            }
            for r in &reading.regions {
                if !matches!(
                    r,
                    Region::Workspace | Region::Scratch | Region::Temp | Region::None
                ) {
                    return Err(format!(
                        "it touches `{}`, which the session does not own",
                        r.as_str()
                    ));
                }
            }
            owned
                .covers(workspace)
                .ok_or_else(|| "it runs outside the session's worktree".into())
        }
    }
}

/// The lexical spelling of `p`: `..` folded, `.` dropped.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests;
