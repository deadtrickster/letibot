//! Owners on another machine: the host's own pass, the bundle it leaves, and the lease that says
//! whether the home box is still alive (`docs/lifecycle.md` d.4's last row, d.10's two remote rows).
//!
//! The home box renews a lease; a host-local reaper reads `<root>/.letibot-home/<owner>/owner.json`
//! and, when the lease is older than the owner's idle TTL plus an hour, ends that owner's processes,
//! removes its caches and BUNDLES its work - never deletes it. The bundle is a `work` holding until
//! the home box fetches it into `refs/letibot/archive/<owner>/`, and the fetch removes it.
//!
//! Leases are compared as "seconds since the last renewal by this box's clock", so clock skew
//! between two machines does not matter: what is compared is an age, never two wall clocks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tokencore::store::{HoldingRow, ReapAct};

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
pub fn owner_root(root: &Path, owner: &str) -> PathBuf {
    root.join(crate::remote::HOME_DIR).join(owner)
}

/// The one round trip that lists a host's owner roots: every `owner.json` under `.letibot-home`,
/// with its path on one line and its content on the next.
pub fn list_script(root: &Path) -> String {
    format!(
        "root={root}\n\
         for f in \"$root\"/{home}/*/owner.json; do\n\
         \x20 [ -f \"$f\" ] || continue\n\
         \x20 printf 'F %s\\n' \"$f\"\n\
         \x20 tr -d '\\n' < \"$f\"\n\
         \x20 printf '\\n'\n\
         done\n",
        root = crate::remote::quote(&root.display().to_string()),
        home = crate::remote::HOME_DIR
    )
}

/// The script that ends one stale owner's compute, removes its caches and bundles its work. It
/// prints one `ACT`/`BUNDLE`/`KEPT`/`SAID` line per fact, each after looking, so the home box reads
/// what happened rather than inferring it from an exit status.
pub fn reap_script(root: &Path, owner: &RemoteOwner, now: i64) -> String {
    let home = owner_root(root, &owner.owner);
    let archive = root.join(".letibot-archive");
    let bundle = archive.join(format!("{}.bundle", owner.owner));
    // The same sweep a session's own close runs, by the mark and the ledger: a host has no cgroup,
    // so those two are what prove a process is this owner's.
    let sweep = match &owner.ledger {
        Some(ledger) if !ledger.is_empty() => crate::remote::sweep_command(&owner.owner, ledger),
        _ => {
            String::from("# no ledger on this host: only the environment mark can find its pids\n")
        }
    };
    let work = owner.work.clone().unwrap_or_default();
    format!(
        "root={root}\nhome={home}\narchive={archive}\nbundle={bundle}\nowner={who}\n\
         mkdir -p \"$archive\"\n\
         # 1. compute first, and by the same rule the session's own close uses.\n\
         {sweep}\n\
         # 2. the work, before any cache goes: once the tree is away the bundle is the only copy,\n\
         #    so a host that cannot make one removes no tree at all.\n\
         work={work}\n\
         if [ -n \"$work\" ] && [ -d \"$work\" ] && git -C \"$work\" rev-parse --git-dir >/dev/null 2>&1; then\n\
         \x20 if git -C \"$work\" bundle create \"$bundle\" --all >/dev/null 2>&1 && [ -s \"$bundle\" ]; then\n\
         \x20   printf 'BUNDLE %s %s\\n' \"$owner\" \"$bundle\"\n\
         \x20 else\n\
         \x20   printf 'KEPT the tree at %s could not be bundled, so nothing of it was removed\\n' \"$work\"\n\
         \x20 fi\n\
         fi\n\
         # 3. caches: a home and a /tmp are rebuilt, and a lease {age} ms old is nobody's session.\n\
         for d in \"$home/tmp\" \"$home/.cache\"; do\n\
         \x20 if [ -e \"$d\" ]; then\n\
         \x20   n=$(du -sk \"$d\" 2>/dev/null | awk '{{print $1}}')\n\
         \x20   if rm -rf \"$d\" && [ ! -e \"$d\" ]; then printf 'ACT cache %s removed %s\\n' \"$owner\" \"${{n:-0}}\"; fi\n\
         \x20 fi\n\
         done\n\
         printf 'SAID swept and bundled, lease %s ms old\\n' \"{age}\"\n",
        root = crate::remote::quote(&root.display().to_string()),
        home = crate::remote::quote(&home.display().to_string()),
        archive = crate::remote::quote(&archive.display().to_string()),
        bundle = crate::remote::quote(&bundle.display().to_string()),
        who = crate::remote::quote(&owner.owner),
        sweep = sweep,
        work = crate::remote::quote(&work),
        age = now.saturating_sub(owner.lease_ms),
    )
}

/// One host's own pass. `Err` is a host this box could not ask: the caller marks its holdings
/// `lost` and removes nothing local for it.
pub fn host_pass(host: &str, root: &Path, hands: &dyn Hands, now: i64) -> Result<HostPass, String> {
    let mut pass = HostPass {
        host: host.to_string(),
        ..HostPass::default()
    };
    let listed = hands.host_command(host, &list_script(root))?;
    for owner in parse_stamps(&listed) {
        let ttl = owner.idle_ttl_ms.unwrap_or(DEFAULT_IDLE_TTL_MS);
        let age = now.saturating_sub(owner.lease_ms);
        if age < ttl.saturating_add(LEASE_GRACE_MS) {
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
        let said = hands.host_command(host, &reap_script(root, &owner, now))?;
        absorb(&mut pass, &owner.owner, &said);
    }
    Ok(pass)
}

/// One script's answer, folded into the pass: `ACT`, `BUNDLE`, `KEPT` and `SAID` lines, each a fact
/// the script printed after looking.
fn absorb(pass: &mut HostPass, owner: &str, said: &str) {
    for line in said.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ACT ") {
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

/// Every `owner.json` in a listing: the lines that parse as one, which skips the `F <path>` markers.
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
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match last.get(host) {
            Some(at) if now.saturating_sub(*at) < RENEW_MS => false,
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
