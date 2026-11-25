//! Owners on another machine: the host's own pass, the bundle it leaves, and the lease that says
//! whether the home box is still alive (`docs/lifecycle.md` d.4's last row, d.10's two remote rows).
//!
//! The home box renews a lease; a host-local reaper reads `<root>/.letibot-home/<owner>/owner.json`
//! and, when the lease is older than the owner's idle TTL plus an hour, ends that owner's processes,
//! removes its build output and BUNDLES its work - never deletes it. The bundle is a `work` holding
//! until the home box fetches it into `refs/letibot/archive/<owner>/`, and the fetch removes it.
//! The home box then frees what the owner home regenerates and removes its ledgers and fanout dirs
//! once it is settled or reaped ([`sweep_script`], `remove_script`).
//!
//! **A build's output is the bytes.** `target`, `target-<name>`, `build`, `tmp` and `.cache` inside
//! an owner home are [`REGENERABLE`]: they go at a settle and at a reaped lease, and `owner.json`
//! stays so the home is still attributable to an owner. The checkout goes only when every commit in
//! it is somewhere else and its tree is clean, because the branch is the work.
//!
//! **What no home and no row explains is found by listing the root.** A checkout whose home was
//! removed, or one made before the owner scheme, and a `fanout-<call>` dir whose row was never
//! written, are [`Orphan`]s: [`list_script`] names every directory beside the homes, and
//! [`orphans_script`] frees one only where no process's cwd is inside it and nothing has written it
//! for an idle TTL plus an hour - both read on that host's clock. A checkout keeps its sources.
//!
//! Leases are compared as "seconds since the last renewal by this box's clock", so clock skew
//! between two machines does not matter: what is compared is an age, never two wall clocks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tokencore::store::{HoldingRow, OwnerRow, ReapAct};

use super::reap::{Act, Hands};

/// How long past its idle TTL a lease may be before a host's own pass acts on it. An hour, because a
/// home box that is restarting, sleeping or simply slow is not a home box that is gone.
pub const LEASE_GRACE_MS: i64 = 60 * 60 * 1000;

/// How often the home box renews an owner's lease on a host. A minute: often enough that the hour's
/// grace is never spent by a box that is alive, cheap enough to be one `sed` on one file.
pub const RENEW_MS: i64 = 60 * 1000;

/// The idle TTL a stamp with no TTL of its own is judged by: a package's 30 minutes, which is what
/// an owner root written by an older build carries.
pub const DEFAULT_IDLE_TTL_MS: i64 = super::PACKAGE_IDLE_TTL_MS;

/// The names inside an owner's home on a host that a build regenerates: `target`, the operator's
/// `target-<package>`, the `stable_paths` build directory (which is `target` too), `build`, `tmp`
/// and `.cache`. Measured 2026-11-25 on xmg: three stopped sessions' homes held 985G, all of it
/// these names, and nothing removed them.
pub const REGENERABLE: [&str; 4] = ["target", "build", "tmp", ".cache"];

/// Is `name` one of [`REGENERABLE`]? `target-<x>` is the operator's per-package spelling of
/// `CARGO_TARGET_DIR`, which a session's own `$HOME` is the natural place for.
pub fn regenerable(name: &str) -> bool {
    REGENERABLE.contains(&name)
        || name
            .strip_prefix("target-")
            .is_some_and(|rest| !rest.is_empty())
}

/// The plan's and the journal's word for one freed path: the name inside the home it had, or
/// `checkout` for a tree that was not one of the five.
pub fn freed_kind(path: &str) -> &'static str {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match name {
        "build" => "build",
        "tmp" => "tmp",
        ".cache" => "cache",
        other if regenerable(other) => "target",
        _ => "checkout",
    }
}

/// The shell that frees one owner home's regenerable output: a `FREED <kib> <path>` line per
/// directory that is gone afterwards, a `KEPT <why>` for one that is not, and nothing for one that
/// was never there. `home` is a shell EXPRESSION - `"$home"` in the host's own pass, a quoted path
/// at a settle - because that is the one thing the two callers spell differently.
fn sweep_of(home: &str) -> String {
    format!(
        "for d in {home}/target {home}/target-* {home}/build {home}/tmp {home}/.cache; do\n\
         \x20 [ -e \"$d\" ] || [ -L \"$d\" ] || continue\n\
         \x20 n=$(du -sk \"$d\" 2>/dev/null | awk '{{print $1}}')\n\
         \x20 rm -rf \"$d\" 2>/dev/null\n\
         \x20 if [ -e \"$d\" ] || [ -L \"$d\" ]; then chmod -R u+w \"$d\" 2>/dev/null; rm -rf \"$d\" 2>/dev/null; fi\n\
         \x20 if [ -e \"$d\" ] || [ -L \"$d\" ]; then printf 'KEPT %s could not be removed\\n' \"$d\"\n\
         \x20 else printf 'FREED %s %s\\n' \"${{n:-0}}\" \"$d\"; fi\n\
         done\n",
        home = home
    )
}

/// The shell that takes one placement's checkout off a host, when `may_go` says its work is
/// elsewhere: nothing in the tree is uncommitted and no branch of it holds a commit the placement's
/// own branch does not. That branch is `remote/<the checkout's own directory name>`, the rule
/// [`crate::remote::SshBackend::place`] and [`crate::remote::collect`] both name it by.
fn checkout_of(work: &str, may_go: Option<&str>) -> String {
    let Some(why) = may_go else {
        let take = format!(
            "\x20 w={work}\n\
             \x20 ours=\"remote/${{w##*/}}\"\n\
             \x20 dirty=$(git -C \"$w\" status --porcelain 2>/dev/null | head -n 1)\n\
             \x20 others=unknown\n\
             \x20 if git -C \"$w\" rev-parse --verify --quiet \"refs/heads/$ours\" >/dev/null 2>&1; then\n\
             \x20   others=\n\
             \x20   for b in $(git -C \"$w\" for-each-ref --format='%(refname:short)' refs/heads 2>/dev/null); do\n\
             \x20     [ \"$b\" = \"$ours\" ] && continue\n\
             \x20     n=$(git -C \"$w\" rev-list --count \"$b\" --not \"$ours\" 2>/dev/null) || n=unknown\n\
             \x20     case \"$n\" in\n\
             \x20       ''|*[!0-9]*) others=\"$others $b(?)\" ;;\n\
             \x20       *) [ \"$n\" -gt 0 ] && others=\"$others $b($n)\" ;;\n\
             \x20     esac\n\
             \x20   done\n\
             \x20 fi\n\
             \x20 if [ -n \"$dirty\" ]; then\n\
             \x20   printf 'KEPT the checkout at %s holds uncommitted work (%s), so it stays\\n' \"$w\" \"$dirty\"\n\
             \x20 elif [ -n \"$others\" ]; then\n\
             \x20   printf 'KEPT the checkout at %s is not only on %s: %s\\n' \"$w\" \"$ours\" \"$others\"\n\
             \x20 else\n\
             \x20   common=$(git -C \"$w\" rev-parse --git-common-dir 2>/dev/null)\n\
             \x20   case \"$common\" in ''|.) common= ;; /*) ;; *) common=\"$w/$common\" ;; esac\n\
             \x20   n=$(du -sk \"$w\" 2>/dev/null | awk '{{print $1}}')\n\
             \x20   rm -rf \"$w\" 2>/dev/null\n\
             \x20   if [ -e \"$w\" ]; then\n\
             \x20     printf 'KEPT the checkout at %s could not be removed\\n' \"$w\"\n\
             \x20   else\n\
             \x20     [ -z \"$common\" ] || git -C \"$common\" worktree prune >/dev/null 2>&1\n\
             \x20     [ -z \"$common\" ] || git -C \"$common\" update-ref -d \"refs/heads/$ours\" >/dev/null 2>&1\n\
             \x20     printf 'FREED %s %s\\n' \"${{n:-0}}\" \"$w\"\n\
             \x20   fi\n\
             \x20 fi\n",
            work = work
        );
        return format!(
            "if [ -d {work} ]; then\n{take}fi\n",
            work = work,
            take = take
        );
    };
    format!(
        "if [ -d {work} ]; then printf 'KEPT the checkout at %s stays: {why}\\n' {work}; fi\n",
        work = work,
        why = why
    )
}

/// What a settle knows about a placement's checkout: whether everything committed in it is
/// somewhere else. The path is never passed in - it is read from the owner's own stamp, the one
/// place the two sides agree on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    /// Its branch is home (or a bundle of it was just written): it may go, if its tree is clean.
    MayGo,
    /// It stays, and this is why.
    Stays(&'static str),
}

/// The script that frees a settled owner's build output on a host, and takes its checkout when the
/// caller proves the branch is home: refused whole while a process of it lives, and every removal
/// checked by looking. Answers `LIVE <pids>` first, then `FREED <kib> <path>` and `KEPT <why>`
/// lines - the shape [`parse_sweep`] reads.
pub fn sweep_script(
    owner: &str,
    home: &str,
    ledgers: &[String],
    checkout: Option<Checkout>,
) -> String {
    let tree = match checkout {
        None => String::new(),
        Some(want) => {
            let block = match want {
                Checkout::MayGo => checkout_of("\"$work\"", None),
                Checkout::Stays(why) => checkout_of("\"$work\"", Some(why)),
            };
            // The checkout's own build output too: a tree kept for the work a bundle cannot carry
            // still holds the `target` that filled the disk, and nothing regenerates it from there.
            format!(
                "work=$(sed -n 's/.*\"work\":\"\\([^\"]*\\)\".*/\\1/p' \"$home/owner.json\" \
                 2>/dev/null)\nif [ -n \"$work\" ]; then\n{block}{free}fi\n",
                free = sweep_of("\"$work\"")
            )
        }
    };
    format!(
        "{live}\n\
         if [ -n \"$(echo $live)\" ]; then printf 'LIVE %s\\n' \"$(echo $live)\"; exit 0; fi\n\
         home={home}\n\
         {sweep}\n\
         {tree}",
        live = live_check(owner, ledgers),
        home = crate::remote::quote(home),
        sweep = sweep_of("\"$home\""),
        tree = tree,
    )
}

/// A directory at a host root that no owner home explains, and the rule that frees it. No em dash
/// of an owner's: a checkout keeps its sources unless this box can prove the branch is home, and a
/// fanout's work dir is scratch that holds no branch at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orphan {
    /// A fanout call's work dir, `<root>/fanout-<call>`: where its branches ran, not what they made.
    Work { path: String },
    /// A placement's checkout: its build output goes, and the tree itself only when this box proved
    /// the branch is home.
    Checkout {
        path: String,
        may_go: Checkout,
        why: &'static str,
    },
}

impl Orphan {
    pub fn path(&self) -> &str {
        match self {
            Orphan::Work { path } | Orphan::Checkout { path, .. } => path,
        }
    }

    /// What the journal calls what went: `fanout` for a work dir whole, and the name of the output
    /// for a checkout, whose tree may have gone whole as `checkout`.
    fn kind(&self, freed: &str) -> &'static str {
        match self {
            Orphan::Work { .. } => "fanout",
            Orphan::Checkout { .. } => freed_kind(freed),
        }
    }

    fn why(&self) -> &'static str {
        match self {
            Orphan::Work { .. } => {
                "no process works in it and nothing has written it for the least a pass waits: a \
                 fanout's work dir is where its branches ran, not what they made"
            }
            Orphan::Checkout { why, .. } => why,
        }
    }
}

/// How long a directory no owner record explains is left alone: the idle TTL every stamp names plus
/// the same hour's grace a lease gets. Read on the HOST's own clock (`date` against `stat`), so two
/// machines' clocks are never subtracted from each other.
pub const ORPHAN_FLOOR_S: i64 = (DEFAULT_IDLE_TTL_MS + LEASE_GRACE_MS) / 1000;

/// The script that frees what nobody owns on a host: every `/proc/<pid>/cwd` read once for the
/// whole pass, then each candidate kept while a process stands in it or while anything has written
/// it inside [`ORPHAN_FLOOR_S`], and otherwise its build output - or, for a checkout this box could
/// prove the branch of, the tree. A checkout it could not stays, and the line says why.
pub fn orphans_script(root: &Path, orphans: &[Orphan]) -> String {
    let mut s = format!(
        "root={root}\n\
         # Every cwd a process holds, read once: a tree no process stands in is a tree nothing is\n\
         # building in, and a 250-directory pass must not walk /proc 250 times.\n\
         cwds=$(mktemp 2>/dev/null || echo \"${{TMPDIR:-/tmp}}/letibot-cwds-$$\")\n\
         : > \"$cwds\"\n\
         for f in /proc/[0-9]*/cwd; do\n\
         \x20 t=$(readlink \"$f\" 2>/dev/null) || continue\n\
         \x20 [ -n \"$t\" ] || continue\n\
         \x20 k=${{f#/proc/}}\n\
         \x20 printf '%s\\t%s\\n' \"${{k%/cwd}}\" \"$t\"\n\
         done >> \"$cwds\" 2>/dev/null\n",
        root = crate::remote::expand_root(&root.display().to_string())
    );
    for o in orphans {
        let action = match o {
            Orphan::Work { .. } => "n=$(du -sk \"$p\" 2>/dev/null | awk '{print $1}')\n\
                 chmod -R u+w \"$p\" 2>/dev/null\n\
                 rm -rf \"$p\" 2>/dev/null\n\
                 if [ -e \"$p\" ]; then printf 'KEPT %s could not be removed\\n' \"$p\"\n\
                 else printf 'FREED %s %s\\n' \"${n:-0}\" \"$p\"; fi\n"
                .to_string(),
            Orphan::Checkout { may_go, .. } => {
                let take = match may_go {
                    Checkout::MayGo => checkout_of("\"$p\"", None),
                    Checkout::Stays(why) => checkout_of("\"$p\"", Some(why)),
                };
                format!("{take}{}", sweep_of("\"$p\""))
            }
        };
        s.push_str(&format!(
            "p={p}\n\
             if [ -d \"$p\" ]; then\n\
             \x20 rp=$(cd \"$p\" 2>/dev/null && pwd -P)\n\
             \x20 held=$(awk -F'\\t' -v p=\"$p\" -v r=\"$rp\" \
\
             '$2==p || index($2, p \"/\")==1 || (r != \"\" && ($2==r || index($2, r \"/\")==1)) {{print $1; exit}}' \
\
             \"$cwds\" 2>/dev/null)\n\
             \x20 newest=$(stat -c %Y \"$p\" 2>/dev/null)\n\
             \x20 if [ -n \"$held\" ]; then\n\
             \x20   printf 'KEPT %s a process works in it (pid %s)\\n' \"$p\" \"$held\"\n\
             \x20 elif [ -z \"$newest\" ]; then\n\
             \x20   printf 'KEPT %s its own last write could not be read, and not being able to tell \
             is a reason to keep\\n' \"$p\"\n\
             \x20 else\n\
             \x20   for c in \"$p\"/target \"$p\"/target-* \"$p\"/build \"$p\"/tmp \"$p\"/.cache; do\n\
             \x20     [ -e \"$c\" ] || [ -L \"$c\" ] || continue\n\
             \x20     n=$(stat -c %Y \"$c\" 2>/dev/null) || continue\n\
             \x20     [ -n \"$n\" ] && [ \"$n\" -gt \"$newest\" ] && newest=$n\n\
             \x20   done\n\
             \x20   age=$(( $(date +%s) - newest ))\n\
             \x20   if [ \"$age\" -lt {floor} ]; then\n\
             \x20     printf 'KEPT %s nothing has written it for %s s, and {floor} s is the least\\n' \
             \"$p\" \"$age\"\n\
             \x20   else\n\
             {action}\x20   fi\n\
             \x20 fi\n\
             fi\n",
            p = crate::remote::quote(o.path()),
            floor = ORPHAN_FLOOR_S,
            action = action,
        ));
    }
    s.push_str("rm -f \"$cwds\" 2>/dev/null\n");
    s
}

/// What one sweep answered: what it freed, each path with the bytes it was worth, and what it kept
/// with the reason. `Err(pids)` from [`parse_sweep`] is an owner a process of which still runs there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sweep {
    pub freed: Vec<(String, u64)>,
    pub kept: Vec<String>,
}

impl Sweep {
    /// What it freed, all together.
    pub fn bytes(&self) -> u64 {
        self.freed.iter().map(|(_, b)| b).sum()
    }
}

/// A sweep's answer.
pub fn parse_sweep(said: &str) -> Result<Sweep, String> {
    let mut sweep = Sweep::default();
    for line in said.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("LIVE ") {
            return Err(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("FREED ") {
            // `FREED <kib> <path>`: a directory `du -sk` measured, removed, and found absent.
            let (kib, path) = rest.split_once(' ').unwrap_or((rest, ""));
            let path = path.trim();
            if !path.is_empty() {
                sweep
                    .freed
                    .push((path.to_string(), kib.parse::<u64>().unwrap_or(0) * 1024));
            }
        } else if let Some(rest) = line.strip_prefix("KEPT ") {
            sweep.kept.push(rest.to_string());
        }
    }
    Ok(sweep)
}

/// One bundle a host's pass wrote, and the owner whose work is inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub owner: String,
    /// The path on that host.
    pub bundle: String,
}

/// What one host's own pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostPass {
    pub host: String,
    pub acts: Vec<Act>,
    pub kept: Vec<String>,
    pub bundles: Vec<Bundle>,
    /// Owners whose stale lease this pass acted on: swept, cached removed, work bundled.
    pub reaped: Vec<String>,
}

impl HostPass {
    /// Did this pass touch anything of `owner`? What the home box's row for it becomes `gone` on.
    pub fn touched(&self, owner: &str) -> bool {
        self.acts.iter().any(|a| a.owner == owner)
    }
}

/// The root a host's owner roots sit under: the parent of `<root>/.letibot-home/<owner>`, which is
/// the one absolute path a holding row carries from the placement that made it.
pub fn root_of(rows: &[HoldingRow]) -> Option<PathBuf> {
    for row in rows {
        for at in Path::new(&row.handle).ancestors() {
            if at.file_name().is_some_and(|n| n == crate::remote::HOME_DIR) {
                return at.parent().map(Path::to_path_buf);
            }
        }
    }
    None
}

/// The stamp a remote owner root carries, as `owner.json` holds it: the local stamp's shape, with
/// the two facts a host's own pass needs and a local one does not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteOwner {
    pub owner: String,
    pub box_id: String,
    pub kind: String,
    pub created_ms: i64,
    pub lease_ms: i64,
    /// The idle TTL the home box named, so a lease is judged by the owner's own clock.
    #[serde(default)]
    pub idle_ttl_ms: Option<i64>,
    /// The checkout on that host whose work a bundle preserves.
    #[serde(default)]
    pub work: Option<String>,
    /// The ledger its pids are written in.
    #[serde(default)]
    pub ledger: Option<String>,
}

impl RemoteOwner {
    /// The line it is written as: one line of JSON, because a listing prints it between two markers
    /// and a pretty one would need a parser rather than a reader.
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The owner root a remote placement writes on its host: `<root>/.letibot-home/<owner>/`, with the
/// `owner.json` in it - the one place the two sides agree on a path.
#[cfg(test)]
pub fn owner_root(root: &Path, owner: &str) -> PathBuf {
    root.join(crate::remote::HOME_DIR).join(owner)
}

/// The one round trip that lists a host's owner roots and what sits beside them: every `owner.json`
/// under `.letibot-home`, with its path on one line and its content on the next, then every
/// directory a placement or a fanout left at the root - `<root>/<repo>/<short>` and
/// `<root>/fanout-<call>`. The second half is the whole point: a checkout whose home was removed,
/// or which was made before the owner scheme, is named here or by nothing at all.
pub fn list_script(root: &Path) -> String {
    format!(
        "root={root}\n\
         for f in \"$root\"/{home}/*/owner.json; do\n\
         \x20 [ -f \"$f\" ] || continue\n\
         \x20 printf 'F %s\\n' \"$f\"\n\
         \x20 tr -d '\\n' < \"$f\"\n\
         \x20 printf '\\n'\n\
         done\n\
         # A root this box could not name is no root: `root=` would glob the far side's `/`.\n\
         if [ -n \"$root\" ]; then\n\
         for d in \"$root\"/*/ \"$root\"/*/*/; do\n\
         \x20 [ -d \"$d\" ] || continue\n\
         \x20 p=${{d%/}}\n\
         # The homes, the ledgers, the archive, the mirrors, the merge gate's own caches and the\n\
         # askpass helpers are not a placement's tree, and none of them is a candidate.\n\
         \x20 case \"$p\" in \"$root\"/.*|\"$root\"/*.git|\"$root\"/gate/*|\"$root\"/askpass/*) continue ;; esac\n\
         \x20 name=${{p##*/}}\n\
         \x20 case \"$name\" in\n\
         \x20   fanout-?*) printf 'D {work} %s\\n' \"$p\" ;;\n\
         \x20   *) if [ -e \"$p/.git\" ]; then printf 'D {checkout} %s\\n' \"$p\"; fi ;;\n\
         \x20 esac\n\
         done\n\
         fi\n",
        root = crate::remote::expand_root(&root.display().to_string()),
        home = crate::remote::HOME_DIR,
        work = DirShape::Work.word(),
        checkout = DirShape::Checkout.word(),
    )
}

/// A directory at a host root that is not an owner home: a fanout call's work dir, or a placement's
/// checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDir {
    pub path: String,
    pub shape: DirShape,
}

/// The two things a placement or a fanout leaves at a host root, each reaped by a different rule: a
/// fanout's work dir is scratch a call made, and a checkout is a branch of somebody's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirShape {
    Work,
    Checkout,
}

impl DirShape {
    fn word(self) -> &'static str {
        match self {
            DirShape::Work => "work",
            DirShape::Checkout => "checkout",
        }
    }

    fn of(word: &str) -> Option<DirShape> {
        match word {
            "work" => Some(DirShape::Work),
            "checkout" => Some(DirShape::Checkout),
            _ => None,
        }
    }
}

/// The `D <shape> <path>` lines of a listing: what a host root holds besides its homes.
pub fn parse_dirs(listed: &str) -> Vec<RootDir> {
    let mut out = Vec::new();
    for line in listed.lines() {
        let Some(rest) = line.trim().strip_prefix("D ") else {
            continue;
        };
        let Some((shape, path)) = rest.split_once(' ') else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if let Some(shape) = DirShape::of(shape) {
            out.push(RootDir {
                path: path.to_string(),
                shape,
            });
        }
    }
    out
}

/// The name a directory at a host root is known by: a checkout's short id, or `fanout-<call>`. An
/// orphan's journal line carries this where a recorded dir's carries its owner's id.
fn label_of(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// What this box can prove about one owner's checkout on `host`: its branch came home as
/// `remote/<host>/<short>`, or the checkout is the only copy of that work and stays.
pub fn owner_checkout(host: &str, owner: &OwnerRow) -> Checkout {
    let Some(repo) = owner.repo.as_deref() else {
        return Checkout::Stays(
            "this owner names no repository on this box to prove its branch in",
        );
    };
    if crate::remote::branch_home(Path::new(repo), host, &owner.id) {
        Checkout::MayGo
    } else {
        Checkout::Stays("its branch is not home here yet, so the checkout is the only copy")
    }
}

/// The same question about a checkout named only by its short id, which is all a checkout whose home
/// is gone leaves to ask with. No row naming it is a checkout this box cannot prove anything about.
fn checkout_verdict(owners: &[OwnerRow], host: &str, label: &str) -> Checkout {
    match owners
        .iter()
        .find(|o| crate::remote::short_branch_id(&o.id) == label)
    {
        Some(owner) => owner_checkout(host, owner),
        None => Checkout::Stays(
            "no owner row on this box names that checkout, so its branch cannot be proved home",
        ),
    }
}

/// Is this owner's lease still young enough to be somebody's? The one rule both halves of a host
/// pass use, so a checkout and the home beside it are never judged by two different clocks.
fn lease_fresh(owner: &RemoteOwner, now: i64) -> bool {
    let ttl = owner.idle_ttl_ms.unwrap_or(DEFAULT_IDLE_TTL_MS);
    now.saturating_sub(owner.lease_ms) < ttl.saturating_add(LEASE_GRACE_MS)
}

/// The script that ends one stale owner's compute, frees its build output, takes its checkout when
/// the bundle holds everything in it, and bundles its work. It prints one `ACT`/`BUNDLE`/`FREED`/
/// `KEPT`/`SAID` line per fact, each after looking, so the home box reads what happened rather than
/// inferring it from an exit status.
/// `home` is the directory the listing found the stamp in: a placement names it after the short id.
pub fn reap_script(root: &Path, home: &str, owner: &RemoteOwner, now: i64) -> String {
    // The same sweep a session's own close runs, by the mark and the ledger: a host has no cgroup,
    // so those two are what prove a process is this owner's.
    let sweep = match &owner.ledger {
        Some(ledger) if !ledger.is_empty() => crate::remote::sweep_command(&owner.owner, ledger),
        _ => {
            String::from("# no ledger on this host: only the environment mark can find its pids\n")
        }
    };
    let work = owner.work.clone().unwrap_or_default();
    // The checkout goes only when this pass's own bundle holds every ref in it: `--all` carries no
    // uncommitted file, so the tree is read first and the bundle is the permission.
    let checkout = format!(
        "if [ -n \"$bundled\" ]; then\n{take}elif [ -d \"$work\" ]; then\n\
         \x20 printf 'KEPT the checkout at %s stays: it could not be bundled, so it is the only copy\\n' \
         \"$work\"\n\
         fi\n",
        take = checkout_of("\"$work\"", None),
    );
    format!(
        "root={root}\nhome={home}\narchive=\"$root\"/.letibot-archive\n\
         bundle=\"$archive\"/{name}\nowner={who}\nbundled=\n\
         mkdir -p \"$archive\"\n\
         # 1. compute first, and by the same rule the session's own close uses.\n\
         {sweep}\n\
         # 2. the work, before any cache goes: once the tree is away the bundle is the only copy,\n\
         #    so a host that cannot make one removes no tree at all.\n\
         work={work}\n\
         if [ -n \"$work\" ] && [ -d \"$work\" ] && git -C \"$work\" rev-parse --git-dir >/dev/null 2>&1; then\n\
         \x20 if git -C \"$work\" bundle create \"$bundle\" --all >/dev/null 2>&1 && [ -s \"$bundle\" ]; then\n\
         \x20   printf 'BUNDLE %s %s\\n' \"$owner\" \"$bundle\"\n\
         \x20   bundled=1\n\
         \x20 else\n\
         \x20   printf 'KEPT the tree at %s could not be bundled, so the tree stays\\n' \"$work\"\n\
         \x20 fi\n\
         fi\n\
         # 3. the owner home's build output: what a build regenerates, and the bytes that filled\n\
         #    xmg. `owner.json` and everything else in the home stay, so it is still attributable.\n\
         {free}\n\
         # 4. the checkout, when the bundle above is the copy of it, and its own build output when\n\
         #    it stays for work a bundle cannot carry.\n\
         {checkout}\n\
         # 5. the checkout's own build output, when the tree stayed for work a bundle cannot carry.\n\
         if [ -n \"$work\" ] && [ -d \"$work\" ]; then\n\
         {workfree}fi\n\
         if [ -n \"$bundled\" ]; then printf 'SAID swept and bundled, lease %s ms old\\n' \"{age}\"; fi\n",
        root = crate::remote::expand_root(&root.display().to_string()),
        home = crate::remote::quote(home),
        name = crate::remote::quote(&format!("{}.bundle", owner.owner)),
        who = crate::remote::quote(&owner.owner),
        sweep = sweep,
        work = crate::remote::quote(&work),
        free = sweep_of("\"$home\""),
        checkout = checkout,
        workfree = sweep_of("\"$work\""),
        age = now.saturating_sub(owner.lease_ms),
    )
}

/// One host's own pass. `Err` is a host this box could not ask: the caller marks its holdings
/// `lost` and removes nothing local for it.
///
/// `known` is what this store already records on that host: a recorded dir is its owner's business,
/// and the orphan half never touches one. `owners` is the store's owner rows, or `None` on a pass
/// that is one owner's - a checkout's branch can only be proved home against a row, so a one-owner
/// pass names none and sweeps nothing of nobody's.
pub fn host_pass(
    host: &str,
    root: &Path,
    hands: &dyn Hands,
    known: &[String],
    owners: Option<&[OwnerRow]>,
    now: i64,
) -> Result<HostPass, String> {
    let mut pass = HostPass {
        host: host.to_string(),
        ..HostPass::default()
    };
    let listed = hands.host_command(host, &list_script(root))?;
    let homes = parse_listing(&listed);
    for (home, owner) in &homes {
        let age = now.saturating_sub(owner.lease_ms);
        if lease_fresh(owner, now) {
            pass.kept.push(format!(
                "{}: its lease was renewed {} ago, so the home box is alive and this is its business",
                owner.owner,
                super::human_millis(age.max(0) as u64)
            ));
            continue;
        }
        if owner.box_id != super::box_id() {
            // Another box's owner, reaped here because this is the disk it fills: exactly what the
            // lease is for. Said, so a reader knows whose work was bundled.
            pass.kept.push(format!(
                "{}: its home box is `{}`, and this host judges it by its lease",
                owner.owner, owner.box_id
            ));
        }
        let said = hands.host_command(host, &reap_script(root, home, owner, now))?;
        absorb(&mut pass, &owner.owner, &said);
        pass.reaped.push(owner.owner.clone());
    }
    if let Some(owners) = owners {
        // What this host shares with every session on it is refused by name here too, exactly as
        // the rows are: a directory nobody owns is still not the machine's cache to lose.
        let mut refused = known.to_vec();
        refused.extend(hands.shared_paths(host));
        let orphans = orphan_dirs(&homes, &listed, &refused, owners, host, now, &mut pass);
        sweep_orphans(host, root, hands, &orphans, &mut pass);
    }
    Ok(pass)
}

/// What a host root holds that no owner home explains, and that nothing here records: a checkout
/// whose home is gone or whose lease is stale, and a fanout call's work dir with no row. A dir a
/// home's fresh lease covers is kept with the reason, and so is a checkout this box cannot prove.
fn orphan_dirs(
    homes: &[(String, RemoteOwner)],
    listed: &str,
    known: &[String],
    owners: &[OwnerRow],
    host: &str,
    now: i64,
    pass: &mut HostPass,
) -> Vec<Orphan> {
    let mut out = Vec::new();
    for dir in parse_dirs(listed) {
        let recorded = known
            .iter()
            .any(|k| dir.path == *k || dir.path.starts_with(&format!("{k}/")));
        if recorded {
            continue;
        }
        let label = label_of(&dir.path);
        match dir.shape {
            DirShape::Work => out.push(Orphan::Work { path: dir.path }),
            DirShape::Checkout => {
                // A home explains a checkout by the tree its stamp NAMES, and by its name: a
                // placement names the home after the short id, and an older one may not.
                let home = homes.iter().find(|(h, owner)| {
                    owner.work.as_deref() == Some(dir.path.as_str())
                        || label_of(h) == label
                        || crate::remote::short_branch_id(&owner.owner) == label
                });
                let why = match home {
                    Some((_, owner)) if lease_fresh(owner, now) => {
                        pass.kept.push(format!(
                            "{label}: its checkout {} stays, its lease on that host was renewed {} \
                             ago",
                            dir.path,
                            super::human_millis(now.saturating_sub(owner.lease_ms).max(0) as u64)
                        ));
                        continue;
                    }
                    Some(_) => "its lease on that host is `idle_ttl + 1 h` old",
                    None => "no owner home on that host names it",
                };
                let may_go = checkout_verdict(owners, host, label);
                if let Checkout::Stays(why) = may_go {
                    pass.kept.push(format!(
                        "{label}: the checkout at {} keeps its sources - {why}",
                        dir.path
                    ));
                }
                out.push(Orphan::Checkout {
                    path: dir.path,
                    may_go,
                    why,
                });
            }
        }
    }
    out
}

/// Free what nobody owns on that host, one removal at the disk's own budget, and fold what it
/// answers into the pass. A host that did not answer is said, never guessed at.
fn sweep_orphans(
    host: &str,
    root: &Path,
    hands: &dyn Hands,
    orphans: &[Orphan],
    pass: &mut HostPass,
) {
    if orphans.is_empty() {
        return;
    }
    let said = match hands.host_removal(host, &orphans_script(root, orphans)) {
        Ok(said) => said,
        Err(e) => {
            pass.kept.push(format!(
                "{host}: the dirs nobody owns could not be swept ({e}), so nothing of them was \
                 touched"
            ));
            return;
        }
    };
    match parse_sweep(&said) {
        Ok(sweep) => {
            for (path, bytes) in sweep.freed {
                // A freed path is a candidate's own dir (a fanout work dir whole) or one of its
                // build names, so the longest candidate it sits under is the one it belongs to.
                let owner = orphans
                    .iter()
                    .filter(|o| path == o.path() || path.starts_with(&format!("{}/", o.path())))
                    .max_by_key(|o| o.path().len());
                let Some(o) = owner else { continue };
                pass.acts.push(Act {
                    owner: label_of(o.path()).to_string(),
                    kind: o.kind(&path).to_string(),
                    host: Some(host.to_string()),
                    handle: path.clone(),
                    act: ReapAct::Remove,
                    said: o.why().to_string(),
                    bytes,
                });
            }
            pass.kept.extend(sweep.kept);
        }
        Err(pids) => pass.kept.push(format!(
            "{host}: the dirs nobody owns were left alone: a process was reported there (pid {pids})"
        )),
    }
}

/// One script's answer, folded into the pass: `ACT`, `BUNDLE`, `FREED`, `KEPT` and `SAID` lines,
/// each a fact the script printed after looking.
fn absorb(pass: &mut HostPass, owner: &str, said: &str) {
    for line in said.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("FREED ") {
            // `FREED <kib> <path>`: a build directory inside the owner's home, or its checkout.
            let (kib, path) = rest.split_once(' ').unwrap_or((rest, ""));
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            pass.acts.push(Act {
                owner: owner.to_string(),
                kind: freed_kind(path).to_string(),
                host: Some(pass.host.clone()),
                handle: path.to_string(),
                act: ReapAct::Remove,
                said: "the host's own pass freed it: a build regenerates it".to_string(),
                bytes: kib.parse::<u64>().unwrap_or(0) * 1024,
            });
        } else if let Some(rest) = line.strip_prefix("ACT ") {
            let mut f = rest.splitn(4, ' ');
            let kind = f.next().unwrap_or("").to_string();
            let who = f.next().unwrap_or(owner).to_string();
            let what = f.next().unwrap_or("").to_string();
            let tail = f.next().unwrap_or("").trim().to_string();
            // The script measures a directory in KiB, as `du -sk` does.
            let bytes = tail.parse::<u64>().unwrap_or(0) * 1024;
            pass.acts.push(Act {
                owner: who,
                said: format!("the host's own pass: {what}"),
                kind,
                host: Some(pass.host.clone()),
                handle: tail,
                act: if what == "removed" {
                    ReapAct::Remove
                } else {
                    ReapAct::Keep
                },
                bytes,
            });
        } else if let Some(rest) = line.strip_prefix("BUNDLE ") {
            let (who, bundle) = rest.split_once(' ').unwrap_or((owner, rest));
            pass.bundles.push(Bundle {
                owner: who.to_string(),
                bundle: bundle.trim().to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("KEPT ") {
            pass.kept.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("SAID ") {
            pass.kept.push(format!("{owner}: {rest}"));
        }
    }
}

/// Every `owner.json` in a listing with the directory it sits in, from the `F <path>` line before it.
pub fn parse_listing(listed: &str) -> Vec<(String, RemoteOwner)> {
    let mut out = Vec::new();
    let mut dir: Option<String> = None;
    for line in listed.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("F ") {
            dir = Path::new(path).parent().map(|p| p.display().to_string());
        } else if let (Some(d), Ok(o)) = (dir.take(), serde_json::from_str::<RemoteOwner>(line)) {
            out.push((d, o));
        }
    }
    out
}

/// Every `owner.json` in a listing: the lines that parse as one, which skips the `F <path>` markers.
#[cfg(test)]
pub fn parse_stamps(listed: &str) -> Vec<RemoteOwner> {
    listed
        .lines()
        .filter_map(|l| serde_json::from_str::<RemoteOwner>(l.trim()).ok())
        .collect()
}

/// Bring one bundle home, fetch it into the archive namespace, and only then remove it from the
/// host: the home box's half of a host pass, and the point at which a bundle stops being `work`.
pub fn collect_bundle(
    hands: &dyn Hands,
    host: &str,
    repo: &Path,
    bundle: &Bundle,
) -> Result<String, String> {
    let bytes = hands.host_read(host, &bundle.bundle)?;
    if bytes.is_empty() {
        return Err(format!("{} came back empty", bundle.bundle));
    }
    let local = std::env::temp_dir().join(format!(
        "letibot-bundle-{}-{}.bundle",
        std::process::id(),
        bundle.owner
    ));
    std::fs::write(&local, &bytes).map_err(|e| format!("{}: {e}", local.display()))?;
    let kept = match super::archive::fetch_bundle(repo, &local, &bundle.owner, "remote") {
        Ok(k) => k,
        Err(e) => {
            let _ = std::fs::remove_file(&local);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&local);
    let said = format!(
        "refs/letibot/archive/{}/remote/ holds {} ref(s) from the bundle",
        bundle.owner,
        kept.refs.len()
    );
    match hands.host_command(
        host,
        &format!("rm -f {}", crate::remote::quote(&bundle.bundle)),
    ) {
        Ok(_) => Ok(format!("{said}, and the bundle is gone")),
        Err(e) => Ok(format!(
            "{said}; the bundle on {host} could not be removed yet: {e}"
        )),
    }
}

/// The script that renews one owner's lease on a host: one `sed` on the field, so the rest of the
/// stamp - the box it belongs to, the kind, its work - is untouched. The file is FOUND by the owner
/// id it carries rather than by its directory's name, because the placement names that directory
/// after the session's short id and the stamp after the session.
pub fn renew_script(root: &Path, owner: &str, now: i64) -> String {
    // The one needle the file must carry: the owner id, as the stamp writes it.
    let needle = crate::remote::quote(&format!("\"owner\":\"{owner}\""));
    // A root that came from `hosts.toml` as `~/…` is that machine's home: it is spelled `$HOME` and
    // must stay OUTSIDE the quoting, or the shell reads a literal dollar and the loop matches
    // nothing.
    let root = root.display().to_string();
    let (prefix, rest) = match root.strip_prefix("$HOME/") {
        Some(r) => ("\"$HOME\"/", r.to_string()),
        None => ("", root.clone()),
    };
    // The glob has to reach the shell with its ONE star unquoted: quoting the whole pattern - which
    // is what `quote` does to a path holding a `*` - makes the loop iterate over a literal star that
    // matches nothing, and a renewal that silently renews nothing is worse than none.
    let glob = format!(
        "{prefix}{base}/*/owner.json",
        base = crate::remote::quote(&format!("{rest}/{home}", home = crate::remote::HOME_DIR))
    );
    format!(
        "for f in {glob}; do\n\
         \x20 [ -f \"$f\" ] || continue\n\
         \x20 grep -qF {needle} \"$f\" || continue\n\
         \x20 sed -i 's/\"lease_ms\":[0-9]*/\"lease_ms\":{now}/' \"$f\"\n\
         \x20 grep -q '\"lease_ms\":{now}' \"$f\" || echo \"{owner}: the lease could not be renewed in $f\"\n\
         done\n"
    )
}

/// How often each host's leases are worth renewing, so a supervisor that ticks every second is one
/// `sed` a minute and not sixty. Keyed by host, because two placements on one machine are one call.
#[derive(Debug, Default)]
pub struct LeaseKeeper {
    last: std::sync::Mutex<BTreeMap<String, i64>>,
}

impl LeaseKeeper {
    pub fn new() -> LeaseKeeper {
        LeaseKeeper::default()
    }

    /// Is this host's renewal due at `now`? A due host is remembered as renewed here, so the caller
    /// may take the round trip without a second bookkeeping step.
    pub fn due(&self, host: &str, now: i64) -> bool {
        self.due_every(host, now, RENEW_MS)
    }

    /// [`LeaseKeeper::due`] on a clock of `every_ms`.
    pub fn due_every(&self, host: &str, now: i64, every_ms: i64) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match last.get(host) {
            Some(at) if now.saturating_sub(*at) < every_ms => false,
            _ => {
                last.insert(host.to_string(), now);
                true
            }
        }
    }
}

/// What the home box does to a host's owner stamps on a tick: renew the lease of every session it
/// still has a placement on (`docs/lifecycle.md` d.3's "lease renewal on the supervisor's per-host
/// tick"). A trait so the daemon's supervisor holds one object and a test holds another.
pub trait RemoteLeases: Send + Sync {
    /// Renew `sessions`' leases on `host`, and answer one line per failure - empty when all of them
    /// were renewed or the tick was not due.
    fn renew(&self, host: &str, sessions: &[String], now: i64) -> Vec<String>;
}

/// The production renewal: one `sed` per host a minute, over the same [`Hands`] the reap pass uses.
pub struct LeaseRenewer {
    hands: std::sync::Arc<dyn Hands>,
    keeper: LeaseKeeper,
    /// The host's root, as `hosts.toml` declares it: `~/letibot-remote` is expanded by the far
    /// shell, because it is that machine's home and not this one's.
    roots: BTreeMap<String, String>,
}

impl LeaseRenewer {
    /// The hosts this box's `hosts.toml` declares, with their roots.
    pub fn for_this_box(hands: std::sync::Arc<dyn Hands>) -> LeaseRenewer {
        let hosts = crate::remote::Hosts::load();
        let roots = hosts
            .names()
            .into_iter()
            .filter_map(|n| hosts.get(&n).map(|h| (n.clone(), h.root.clone())))
            .collect();
        LeaseRenewer {
            hands,
            keeper: LeaseKeeper::new(),
            roots,
        }
    }

    /// A renewal over the roots a caller names, which is what a test has instead of `hosts.toml`.
    pub fn with_roots(
        hands: std::sync::Arc<dyn Hands>,
        roots: BTreeMap<String, String>,
    ) -> LeaseRenewer {
        LeaseRenewer {
            hands,
            keeper: LeaseKeeper::new(),
            roots,
        }
    }
}

impl RemoteLeases for LeaseRenewer {
    fn renew(&self, host: &str, sessions: &[String], now: i64) -> Vec<String> {
        if sessions.is_empty() || !self.keeper.due(host, now) {
            return Vec::new();
        }
        let Some(root) = self.roots.get(host) else {
            // Not a declared host: there is nothing this box could renew on it, and that is not a
            // failure of the tick.
            return Vec::new();
        };
        let root = PathBuf::from(expand_in_shell(root));
        let mut said = Vec::new();
        for owner in sessions {
            if let Err(why) = self
                .hands
                .host_command(host, &renew_script(&root, owner, now))
            {
                said.push(format!(
                    "{owner} on {host}: its lease was not renewed ({why})"
                ));
            }
        }
        said
    }
}

/// `~/x` in a declared root is that machine's home, so the far shell expands it: a quoted `~` is a
/// literal tilde, and a root the shell cannot expand is a `sed` over no file at all.
fn expand_in_shell(root: &str) -> String {
    match root.strip_prefix("~/") {
        Some(rest) => format!("$HOME/{rest}"),
        None => root.to_string(),
    }
}

/// Is `handle` one of the three shapes a placement or a fanout writes on a host? The removal is
/// refused for anything else, so a row with a wrong handle cannot aim `rm -rf` at a home or a root.
pub fn removable_shape(handle: &str) -> bool {
    let path = Path::new(handle);
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    match parent {
        Some(crate::remote::HOME_DIR) | Some(crate::remote::JOBS_DIR) => !name.starts_with('.'),
        _ => name.starts_with("fanout-") && name.len() > "fanout-".len(),
    }
}

/// The paths on a host that are the MACHINE's and never one session's: `cargo_home`, `sccache_dir`
/// and the directory the wrapper itself lives in. Every session there shares them, so no pass
/// removes one - a reaped cargo home is a whole machine's registry, and a reaped cache dir is
/// rebuilt by everybody's next build.
pub fn shared_paths(host: &crate::remote::Host) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for at in [host.cargo_home.as_deref(), host.sccache_dir.as_deref()] {
        if let Some(at) = at.filter(|p| !p.is_empty()) {
            out.push(at.to_string());
        }
    }
    if let Some(at) = host.rustc_wrapper.as_deref().filter(|p| !p.is_empty()) {
        out.push(match at.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => at[..i].to_string(),
            None => at.to_string(),
        });
    }
    out
}

/// A path with the machine's home taken off the front: `~/.cargo`, `$HOME/.cargo` and
/// `/home/dead/.cargo` name one place, and a shared path is compared by this tail because
/// `hosts.toml` declares the first spelling while a row carries the third.
fn tail_after_home(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    for lead in ["$HOME/", "~/"] {
        if let Some(rest) = path.strip_prefix(lead) {
            return rest;
        }
    }
    match path.strip_prefix("/home/").and_then(|r| r.split_once('/')) {
        Some((_, rest)) => rest,
        None => path,
    }
}

/// Is `path` one of `shared`? A doubt here keeps the path, which is why the home is taken off
/// either side rather than compared literally.
pub fn is_shared(path: &str, shared: &[String]) -> bool {
    let tail = tail_after_home(path);
    shared.iter().any(|s| {
        path.trim_end_matches('/') == s.trim_end_matches('/') || tail_after_home(s) == tail
    })
}

/// The far side's liveness check for one owner: a process carrying its session mark, or a pid in
/// one of its ledgers whose launcher is still running. Prints `LIVE <pids>` when there is one.
fn live_check(owner: &str, ledgers: &[String]) -> String {
    let ledgers: Vec<String> = ledgers
        .iter()
        .map(|l| crate::remote::expand_root(l))
        .collect();
    format!(
        "mark={mark}\n\
         live=$(grep -lF -- \"$mark\" /proc/[0-9]*/environ 2>/dev/null | while read -r f; do\n\
         \x20 tr '\\0' '\\n' < \"$f\" 2>/dev/null | grep -qxF -- \"$mark\" && {{ p=${{f#/proc/}}; echo \"${{p%/environ}}\"; }}\n\
         done | tr '\\n' ' ')\n\
         for l in {ledgers}; do\n\
         \x20 for f in \"$l\"/*.pid; do\n\
         \x20   [ -f \"$f\" ] || continue\n\
         \x20   k=$(tr -d ' ' < \"$f\" 2>/dev/null)\n\
         \x20   [ -n \"$k\" ] || continue\n\
         \x20   case \"$(ps -p \"$k\" -o args= 2>/dev/null)\" in *\"${{f%.pid}}.sh\"*) live=\"$live $k\";; esac\n\
         \x20 done\n\
         done\n",
        mark = crate::remote::quote(&format!("{}={owner}", crate::remote::SESSION_MARK)),
        ledgers = if ledgers.is_empty() {
            "''".to_string()
        } else {
            ledgers.join(" ")
        },
    )
}

/// The script that removes a settled owner's dirs on a host: refused whole while any process of
/// it lives, a home whose stamp names another owner is kept, and each removal is checked by looking.
/// Go's module cache is read-only, so `chmod -R u+w` runs before `rm -rf`. Answers one
/// `LIVE`/`GONE <i> <kib>`/`KEPT <i> <why>` line per fact, `i` indexing `paths`.
pub fn remove_script(owner: &str, ledgers: &[String], paths: &[String]) -> String {
    let mut s = live_check(owner, ledgers);
    s.push_str(
        "if [ -n \"$(echo $live)\" ]; then printf 'LIVE %s\\n' \"$(echo $live)\"; exit 0; fi\n",
    );
    let needle = crate::remote::quote(&format!("\"owner\":\"{owner}\""));
    for (i, path) in paths.iter().enumerate() {
        if !removable_shape(path) {
            s.push_str(&format!(
                "printf 'KEPT {i} not a home, ledger or fanout dir\\n'\n"
            ));
            continue;
        }
        s.push_str(&format!(
            "p={p}\n\
             if [ -f \"$p/owner.json\" ] && ! grep -qF {needle} \"$p/owner.json\"; then\n\
             \x20 printf 'KEPT {i} its owner.json names another owner\\n'\n\
             elif [ -e \"$p\" ]; then\n\
             \x20 n=$(du -sk \"$p\" 2>/dev/null | awk '{{print $1}}')\n\
             \x20 chmod -R u+w \"$p\" 2>/dev/null\n\
             \x20 rm -rf \"$p\" 2>/dev/null\n\
             \x20 if [ -e \"$p\" ]; then printf 'KEPT {i} rm -rf left it there\\n'; else printf 'GONE {i} %s\\n' \"${{n:-0}}\"; fi\n\
             else\n\
             \x20 printf 'GONE {i} 0\\n'\n\
             fi\n",
            p = crate::remote::expand_root(path),
        ));
    }
    s
}

/// The script that sizes recorded dirs on a host: `SIZE <i> <kib>` or `MISSING <i>` per path.
pub fn measure_script(paths: &[String]) -> String {
    let mut s = String::new();
    for (i, path) in paths.iter().enumerate() {
        s.push_str(&format!(
            "p={p}\n\
             if [ -e \"$p\" ]; then printf 'SIZE {i} %s\\n' \"$(du -sk \"$p\" 2>/dev/null | awk '{{print $1}}')\"; \
             else printf 'MISSING {i}\\n'; fi\n",
            p = crate::remote::expand_root(path),
        ));
    }
    s
}

/// What a removal or a measurement said about one path, by its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSaid {
    Gone { bytes: u64 },
    Kept(String),
    Size(u64),
    Missing,
}

/// A script's answer: `Err(pids)` when the owner has a live process there, else what each path did.
pub fn parse_paths(said: &str) -> Result<BTreeMap<usize, PathSaid>, String> {
    let mut out = BTreeMap::new();
    for line in said.lines() {
        let mut f = line.trim().splitn(3, ' ');
        let word = f.next().unwrap_or("");
        if word == "LIVE" {
            return Err(f.collect::<Vec<_>>().join(" "));
        }
        let Some(i) = f.next().and_then(|i| i.parse::<usize>().ok()) else {
            continue;
        };
        let rest = f.next().unwrap_or("").trim();
        let kib = || rest.parse::<u64>().unwrap_or(0) * 1024;
        let said = match word {
            "GONE" => PathSaid::Gone { bytes: kib() },
            "KEPT" => PathSaid::Kept(rest.to_string()),
            "SIZE" => PathSaid::Size(kib()),
            "MISSING" => PathSaid::Missing,
            _ => continue,
        };
        out.insert(i, said);
    }
    Ok(out)
}

/// The holdings this store records on other hosts and has not seen go, per host: what `letibot
/// holds` lists under its remote section, with the size the last pass measured.
pub fn remote_holdings(
    store: &letibot_tokencore::store::Store,
    box_id: &str,
) -> letibot_tokencore::store::Result<BTreeMap<String, Vec<HoldingRow>>> {
    let mut out: BTreeMap<String, Vec<HoldingRow>> = BTreeMap::new();
    for row in store.holdings()? {
        let Some(host) = row.host.clone() else {
            continue;
        };
        if host == box_id || row.state == letibot_tokencore::store::HoldingState::Gone {
            continue;
        }
        out.entry(host).or_default().push(row);
    }
    Ok(out)
}

/// [`remote_holdings`] in a person's words: one line per host with its total, one per holding.
pub fn remote_holdings_lines(by_host: &BTreeMap<String, Vec<HoldingRow>>) -> Vec<String> {
    let size = |b: Option<u64>| match b {
        Some(b) => super::human_bytes(b),
        None => "not measured yet".to_string(),
    };
    let mut out = Vec::new();
    for (host, rows) in by_host {
        let total: u64 = rows.iter().filter_map(|r| r.bytes).sum();
        out.push(format!(
            "{host}: {} holding(s), {}",
            rows.len(),
            super::human_bytes(total)
        ));
        for r in rows {
            out.push(format!(
                "    {} {} {} ({}, {})",
                r.owner_id,
                r.kind,
                r.handle,
                size(r.bytes),
                r.state.word()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing is read as the stamps it holds, and the `F` markers are not parsed as one.
    #[test]
    fn a_listing_is_the_stamps_it_holds() {
        let one = RemoteOwner {
            owner: "s-1".into(),
            box_id: "lab2x1".into(),
            kind: "package".into(),
            created_ms: 1,
            lease_ms: 2,
            idle_ttl_ms: Some(1_800_000),
            work: Some("/r/repo/abc".into()),
            ledger: Some("/r/.letibot-jobs/abc".into()),
        };
        let listing = format!("F /r/.letibot-home/s-1/owner.json\n{}\nF /x\n", one.line());
        assert_eq!(parse_stamps(&listing), vec![one]);
        assert!(parse_stamps("F /only/a/marker\n").is_empty());
    }

    /// A lease is due once a minute per host and not once a tick, which is what keeps a
    /// second-by-second supervisor from spending a round trip every second.
    #[test]
    fn a_lease_is_renewed_once_a_minute_per_host() {
        let keeper = LeaseKeeper::new();
        assert!(keeper.due("lubuntu2", 0));
        assert!(!keeper.due("lubuntu2", RENEW_MS - 1));
        assert!(keeper.due("lubuntu2", RENEW_MS));
        assert!(
            keeper.due("lubuntu3", RENEW_MS),
            "each host has its own clock"
        );
    }

    /// The names a sweep frees, and the ones it never does: a home's build output is five names,
    /// and `owner.json`, a ledger or a stranger's file is not one of them.
    #[test]
    fn only_a_builds_output_is_regenerable() {
        for name in ["target", "target-pkg", "build", "tmp", ".cache"] {
            assert!(regenerable(name), "{name} is a build's output");
        }
        for name in [
            "owner.json",
            "target-",
            ".cargo",
            "registry",
            "home",
            "out",
            "wt",
        ] {
            assert!(!regenerable(name), "{name} is not a build's output");
        }
        assert_eq!(freed_kind("/r/.letibot-home/s-1/target-pkg"), "target");
        assert_eq!(freed_kind("/r/.letibot-home/s-1/.cache"), "cache");
        assert_eq!(freed_kind("/r/repo/abc"), "checkout");
    }

    /// The listing names what sits beside the homes and nothing that is not a placement's tree: a
    /// mirror, the merge gate's own caches, an askpass helper and a home are never candidates, and a
    /// checkout is one only because of the `.git` in it.
    #[test]
    fn the_listing_names_a_checkout_and_a_fanout_dir_and_no_mirror() {
        let dir = std::env::temp_dir().join(format!("letibot-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        let at = |rel: &str, file: Option<&str>| {
            let p = match file {
                Some(name) => root.join(rel).join(name),
                None => root.join(rel),
            };
            std::fs::create_dir_all(p).unwrap();
        };
        at(".letibot-home/s-1", None);
        let stamp = RemoteOwner {
            owner: "s-1".into(),
            box_id: "lab2x1".into(),
            kind: "session".into(),
            created_ms: 1,
            lease_ms: 2,
            idle_ttl_ms: None,
            work: Some(root.join("repo/abc").display().to_string()),
            ledger: None,
        };
        std::fs::write(root.join(".letibot-home/s-1/owner.json"), stamp.line()).unwrap();
        at("repo/abc", None);
        std::fs::write(root.join("repo/abc/.git"), "gitdir: ../repo.git\n").unwrap();
        at("fanout-c1", None);
        at("repo.git/objects", None);
        at("gate/repo-abc/tree", None);
        std::fs::write(root.join("gate/repo-abc/tree/.git"), "gitdir: ../src.git\n").unwrap();
        at("askpass/abc", Some("nested"));
        std::fs::write(root.join("askpass/abc/nested/.git"), "gitdir: x\n").unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(list_script(&root))
            .output()
            .expect("a shell");
        assert!(out.status.success(), "{out:?}");
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(parse_listing(&listed).len(), 1, "the home: {listed}");
        let mut dirs = parse_dirs(&listed);
        dirs.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            dirs,
            vec![
                RootDir {
                    path: root.join("fanout-c1").display().to_string(),
                    shape: DirShape::Work,
                },
                RootDir {
                    path: root.join("repo/abc").display().to_string(),
                    shape: DirShape::Checkout,
                },
            ],
            "{listed}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One sweep of a fixture's home: the build output goes, the stamp and everything else stay,
    /// and the sizes it reports are what it freed.
    #[test]
    fn sweeping_a_home_frees_the_build_output_and_keeps_the_stamp() {
        let dir = std::env::temp_dir().join(format!("letibot-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("root/.letibot-home/s-1");
        for (at, bytes) in [
            ("target/debug/blob", 8192),
            ("target-pkg/blob", 4096),
            ("build/rano/x.c", 2048),
            ("tmp/worker.tmp", 1024),
            (".cache/x", 512),
            ("work-in-progress/notes.md", 256),
        ] {
            let file = home.join(at);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, vec![b'x'; bytes]).unwrap();
        }
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("owner.json"), "{}").unwrap();
        let script = sweep_script("s-1", &home.display().to_string(), &[], None);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("a shell");
        assert!(out.status.success(), "{out:?}");
        let sweep = parse_sweep(&String::from_utf8_lossy(&out.stdout)).expect("no live pid");

        for at in ["target", "target-pkg", "build", "tmp", ".cache"] {
            assert!(
                !home.join(at).exists(),
                "{at} is still there: {:?}",
                sweep.freed
            );
        }
        assert!(home.join("owner.json").exists(), "the stamp is not a cache");
        assert!(
            home.join("work-in-progress/notes.md").exists(),
            "nothing a sweep does not name is touched"
        );
        assert!(sweep.kept.is_empty(), "{sweep:?}");
        assert!(
            sweep.bytes() >= 8192 + 4096 + 2048 + 1024 + 512,
            "{sweep:?}"
        );
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&dir)
            .status();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A shared path is compared by its tail, because `hosts.toml` says `~/.cargo` and a row says
    /// `/home/dead/.cargo`: two spellings of one place, and a doubt here keeps the path.
    #[test]
    fn a_shared_path_is_recognised_in_either_spelling() {
        let shared = vec!["~/.cargo".to_string(), "/build/sccache".to_string()];
        for spelled in ["~/.cargo", "$HOME/.cargo", "/home/dead/.cargo"] {
            assert!(is_shared(spelled, &shared), "{spelled}");
        }
        assert!(is_shared("/build/sccache/", &shared));
        assert!(!is_shared("/home/dead/.cargo-home", &shared));
        assert!(!is_shared("/r/.letibot-home/s-1", &shared));
    }

    /// The renewal changes the one field and leaves the rest of the stamp where it was.
    #[test]
    fn renewing_a_lease_edits_one_field() {
        let dir = std::env::temp_dir().join(format!("letibot-renew-{}", std::process::id()));
        let root = dir.join("root");
        let home = owner_root(&root, "s-1");
        std::fs::create_dir_all(&home).unwrap();
        let stamp = RemoteOwner {
            owner: "s-1".into(),
            box_id: "lab2x1".into(),
            kind: "package".into(),
            created_ms: 10,
            lease_ms: 10,
            idle_ttl_ms: Some(1_800_000),
            work: None,
            ledger: None,
        };
        let file = home.join("owner.json");
        std::fs::write(&file, stamp.line()).unwrap();
        let script = renew_script(&root, "s-1", 99);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("a shell");
        assert!(out.status.success(), "{out:?}");
        let back = parse_stamps(&std::fs::read_to_string(&file).unwrap());
        assert_eq!(back[0].lease_ms, 99);
        assert_eq!(back[0].created_ms, 10, "the rest of the stamp is untouched");
        assert_eq!(back[0].box_id, "lab2x1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
