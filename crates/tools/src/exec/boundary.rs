//! `[boundary]` in `leticode.toml`: what a command's view holds beyond the built-in toolchain
//! list, as data with its origin, and the one place that refuses what config may never grant.
//!
//! ```toml
//! [boundary]
//! grants_ro = ["~/datasets"]          # readable inside a child's view
//! grants_rw = ["~/scratch"]           # writable too (user file only)
//! hide = ["~/.local/bin/secret-tool"] # denied after every grant, at both levels
//! env = { RUST_LOG = "info" }         # constants handed to every command
//! ```
//!
//! The defaults are [`super::confine::GRANT_CANDIDATES`]. The parser lives in `leticode-config`;
//! this side owns the rules so a seat built without a file obeys them too.

use std::path::{Component, Path, PathBuf};

use super::confine::Grant;
use super::seat::SeatView;

/// Where an entry was written, so a refusal can name the file and line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub line: usize,
    pub user: bool,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<T> {
    pub value: T,
    pub from: Origin,
}

/// The four keys of both levels, unioned. Empty is today's behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundaryConfig {
    pub grants_ro: Vec<Entry<String>>,
    pub grants_rw: Vec<Entry<String>>,
    pub hide: Vec<Entry<String>>,
    pub env: Vec<Entry<(String, String)>>,
}

impl BoundaryConfig {
    /// A project adds to the user's. Nothing is overridden: `hide` wins in the renderer.
    pub fn overridden_by(&self, other: &BoundaryConfig) -> BoundaryConfig {
        fn union<T: Clone + PartialEq>(a: &[Entry<T>], b: &[Entry<T>]) -> Vec<Entry<T>> {
            let mut out = a.to_vec();
            for e in b {
                if !out.iter().any(|o| o.value == e.value) {
                    out.push(e.clone());
                }
            }
            out
        }
        BoundaryConfig {
            grants_ro: union(&self.grants_ro, &other.grants_ro),
            grants_rw: union(&self.grants_rw, &other.grants_rw),
            hide: union(&self.hide, &other.hide),
            env: union(&self.env, &other.env),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == BoundaryConfig::default()
    }
}

/// Home-relative paths no config grants, whatever it says: every store the seat fences plus the
/// letibot store and the shell profiles. One list with the fence, so they cannot drift.
pub fn never_grant() -> Vec<&'static str> {
    super::seatbelt::deny_under_home(false)
}

/// Is `name` a secret or a credential helper? Such a value is never put on a command line, where
/// every user on the box reads it with `ps`.
pub fn is_secret_name(name: &str) -> bool {
    const SECRET_SUFFIX: &[&str] = &[
        "_TOKEN",
        "_SECRET",
        "_PASSWORD",
        "_API_KEY",
        "_ACCESS_KEY",
        "_PRIVATE_KEY",
    ];
    let up = name.to_ascii_uppercase();
    up == "GH_TOKEN"
        || up.starts_with("GIT_CONFIG")
        || SECRET_SUFFIX.iter().any(|s| up.ends_with(s))
}

/// Why `name` may not be handed in by config, or `None`. Secrets travel by the seat's own
/// injection, and loader variables run code in every command. Minimal on purpose: the one list a
/// carry-over of the caller's environment reads too.
pub fn env_refusal(name: &str) -> Option<&'static str> {
    const EXACT: &[&str] = &["SSH_AUTH_SOCK", "BASH_ENV", "ENV", "PATH", "HOME", "TMPDIR"];
    let up = name.to_ascii_uppercase();
    if is_secret_name(name) {
        return Some("a secret or a credential helper travels by the seat's own injection");
    }
    if EXACT.contains(&up.as_str()) || up.starts_with("LD_") || up.starts_with("DYLD_") {
        return Some("it changes what every command runs or where it lives");
    }
    None
}

/// `~` and `$HOME` read as `home`; anything else must already be absolute.
fn expand(raw: &str, home: &Path) -> PathBuf {
    let rest = raw
        .strip_prefix("$HOME")
        .or_else(|| raw.strip_prefix('~'))
        .filter(|r| r.is_empty() || r.starts_with('/'));
    match rest {
        Some(r) => home.join(r.trim_start_matches('/')),
        None => PathBuf::from(raw),
    }
}

/// Lexical `..` and `.` removal, without touching the disk: a grant must mean the path written.
fn clean(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    Some(out)
}

/// The config resolved against a home: absolute paths, each with the origin that wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub grants_ro: Vec<(PathBuf, Origin)>,
    pub grants_rw: Vec<(PathBuf, Origin)>,
    pub hide: Vec<(PathBuf, Origin)>,
    pub env: Vec<(String, String, Origin)>,
}

fn path_of(
    key: &str,
    e: &Entry<String>,
    home: &Path,
    grant: bool,
) -> Result<(PathBuf, Origin), String> {
    let at = |why: String| format!("{}: [boundary] {key} = \"{}\": {why}", e.from, e.value);
    let abs = expand(&e.value, home);
    if !abs.is_absolute() {
        return Err(at(
            "the path is not absolute; start it with `/` or `~/`".into()
        ));
    }
    let Some(path) = clean(&abs) else {
        return Err(at("`..` climbs out of the root".into()));
    };
    if grant {
        if path == Path::new("/") || home.starts_with(&path) {
            return Err(at(
                "this holds the whole home, and the home holds the credential stores; name \
                 the directory the work needs"
                    .into(),
            ));
        }
        for rel in never_grant() {
            if path.starts_with(home.join(rel)) {
                return Err(at(format!(
                    "`~/{rel}` is on the never-grant list, and no config grants it or anything \
                     under it"
                )));
            }
        }
    }
    Ok((path, e.from.clone()))
}

/// Check every entry against the rules and expand it. The first violation is the answer, naming
/// the file, the line and the key.
pub fn resolve(cfg: &BoundaryConfig, home: &Path) -> Result<Resolved, String> {
    let mut r = Resolved::default();
    for e in &cfg.grants_ro {
        r.grants_ro.push(path_of("grants_ro", e, home, true)?);
    }
    for e in &cfg.grants_rw {
        if !e.from.user {
            return Err(format!(
                "{}: [boundary] grants_rw = \"{}\": a project file may not make a path writable. \
                 Widening is the operator's call, so write it in the user file",
                e.from, e.value
            ));
        }
        r.grants_rw.push(path_of("grants_rw", e, home, true)?);
    }
    for e in &cfg.hide {
        r.hide.push(path_of("hide", e, home, false)?);
    }
    for e in &cfg.env {
        let (k, v) = &e.value;
        let valid = !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!(
                "{}: [boundary] env: `{k}` is not a variable name",
                e.from
            ));
        }
        if let Some(why) = env_refusal(k) {
            return Err(format!(
                "{}: [boundary] env: `{k}` is refused, {why}",
                e.from
            ));
        }
        r.env.push((k.clone(), v.clone(), e.from.clone()));
    }
    Ok(r)
}

/// Thread a resolved config into a seat. Grants join a child's view and a wide view's writes
/// (a wide view already reads everything). `hide` masks at both levels and beats any grant.
pub fn apply(mut seat: SeatView, r: &Resolved) -> SeatView {
    let wide = seat.home.is_some();
    if !wide {
        for (p, from) in &r.grants_ro {
            if p.exists() && !seat.grants.iter().any(|g| g.path() == p) {
                seat.grants.push(Grant::ReadOnly {
                    path: p.clone(),
                    why: format!("granted by `[boundary] grants_ro` at {from}"),
                });
            }
        }
    }
    for (p, from) in &r.grants_rw {
        if p.exists() {
            seat.grants.retain(|g| g.path() != p);
            seat.grants.push(Grant::ReadWrite {
                path: p.clone(),
                why: format!("granted by `[boundary] grants_rw` at {from}"),
            });
        }
    }
    seat.masked.extend(r.hide.iter().map(|(p, _)| p.clone()));
    seat.masked.sort();
    seat.masked.dedup();
    seat.hidden = r.hide.clone();
    seat.env
        .extend(r.env.iter().map(|(k, v, _)| (k.clone(), v.clone())));
    seat
}

/// What a command gets, for `/boundary` and `letibot boundary`. Secret values never print.
/// `probe` is the boundary a live session MEASURED; a report with none says so rather than
/// leaving a reader to assume the config was checked.
pub fn report(
    seat: &SeatView,
    r: &Resolved,
    level: &str,
    probe: Option<&super::confine::Boundary>,
) -> Vec<String> {
    let mut out = vec![format!("{level} perimeter")];
    out.push(format!(
        "home: {}",
        match &seat.home {
            Some(h) => format!("{} (the operator's own)", h.display()),
            None => "a throwaway directory".to_string(),
        }
    ));
    out.push(format!(
        "network: {}",
        match &seat.egress {
            super::confine::Egress::Denied => "denied".to_string(),
            super::confine::Egress::Host { why } => format!("the host's, because {why}"),
        }
    ));
    out.push("paths:".into());
    if seat.grants.is_empty() {
        out.push(if seat.home.is_some() {
            "  the whole filesystem is readable; the home is writable".into()
        } else {
            "  none beyond the project".into()
        });
    }
    for g in &seat.grants {
        let mode = match g {
            Grant::ReadOnly { .. } => "ro",
            Grant::ReadWrite { .. } => "rw",
            Grant::AgentSocket { .. } => "socket",
        };
        out.push(format!("  {mode} {} - {}", g.path().display(), g.why()));
    }
    for (p, from) in r.grants_ro.iter().chain(&r.grants_rw) {
        if !p.exists() {
            out.push(format!("  absent {} - named at {from}", p.display()));
        }
    }
    out.push("hidden:".into());
    for p in &seat.masked {
        let named = r.hide.iter().find(|(h, _)| h == p);
        out.push(match named {
            Some((_, from)) => format!("  {} - `[boundary] hide` at {from}", p.display()),
            None => format!("  {} - credential or profile, fixed", p.display()),
        });
    }
    out.push("env:".into());
    for (k, v) in &seat.env {
        out.push(format!("  {k}={v}"));
    }
    for (k, _) in &seat.secret_env.0 {
        out.push(format!("  {k}=<redacted>"));
    }
    out.push(
        "config keys: `[boundary] grants_ro`, `grants_rw`, `hide`, `env` in leticode.toml. \
         Never granted, whatever config says: ~/.ssh and every store under hidden."
            .into(),
    );
    out.push(String::new());
    match probe {
        Some(b) => out.extend(probe_lines(b)),
        None => out.push(
            "probe: not measured here — `/boundary` in a session prints what that session's own \
             probe measured; `letibot boundary` reads the config and starts nothing."
                .into(),
        ),
    }
    out
}

/// What the probe measured, in three lines: the mechanism and when, the namespaces (or the
/// policy's properties), and the seals. The words are [`super::confine::Boundary::describe`]'s,
/// so a report cannot disagree with the disclosure the model is told.
pub fn probe_lines(b: &super::confine::Boundary) -> Vec<String> {
    use super::confine::{Mechanism, Namespace, Prop, PropState, Seal, SealKind};
    let verdict = if b.complete() && !b.partial() {
        "confined"
    } else if b.complete() {
        "confined, PARTIALLY"
    } else {
        "NOT CONFINED"
    };
    let mut out = vec![format!(
        "probe: {verdict}, by {}, measured {}",
        b.mechanism,
        age_of(b.measured_at)
    )];
    let mut measured: Vec<String> = Vec::new();
    match b.platform {
        Mechanism::Namespaces => {
            for n in Namespace::ALL {
                let state =
                    b.ns.get(&n)
                        .cloned()
                        .unwrap_or_else(|| super::confine::NsState::NotEntered {
                            why: "never measured".into(),
                        });
                measured.push(format!("{n} {}", state.line()));
            }
        }
        Mechanism::Policy => {
            for p in Prop::ALL {
                let state = b
                    .props
                    .get(&p)
                    .cloned()
                    .unwrap_or_else(|| PropState::Unverified {
                        why: "never measured".into(),
                    });
                measured.push(format!("{p} {}", state.line()));
            }
        }
    }
    out.push(format!("  {}", measured.join(", ")));
    let seals: Vec<String> = SealKind::ALL
        .iter()
        .map(|k| {
            let seal = b.seals.get(k).cloned().unwrap_or_else(|| Seal::Unverified {
                why: "never tested".into(),
            });
            format!("{k} {}", seal.line())
        })
        .collect();
    out.push(format!("  seals: {}", seals.join(", ")));
    out
}

/// How long ago a measurement was taken, in the unit a reader needs: the age is what says
/// whether the boundary was measured for this session or one that has since gone.
fn age_of(at: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(at)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=90 => format!("{secs}s ago"),
        91..=5400 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::seat::{self, Level};
    use crate::mode::Mode;

    fn origin(user: bool) -> Origin {
        Origin {
            file: PathBuf::from("/h/.config/letibot/leticode.toml"),
            line: 7,
            user,
        }
    }

    fn entry(v: &str, user: bool) -> Entry<String> {
        Entry {
            value: v.to_string(),
            from: origin(user),
        }
    }

    fn ro(v: &str) -> BoundaryConfig {
        BoundaryConfig {
            grants_ro: vec![entry(v, true)],
            ..Default::default()
        }
    }

    /// The operator's rule: `~/.ssh` and everything under it is never granted, by any spelling
    /// and from either level, and the refusal names the file, the line and the key.
    #[test]
    fn ssh_is_never_granted_by_any_spelling() {
        let home = Path::new("/h");
        for spelling in [
            "~/.ssh",
            "~/.ssh/id_ed25519",
            "$HOME/.ssh",
            "/h/.ssh",
            "/h/Projects/../.ssh",
            "/h/.ssh/../.ssh/keys",
            "~/.gnupg",
            "~/.config/letibot",
            "~",
            "/h",
            "/",
        ] {
            let err = resolve(&ro(spelling), home).expect_err(spelling);
            assert!(
                err.starts_with("/h/.config/letibot/leticode.toml:7: [boundary] grants_ro"),
                "{spelling}: {err}"
            );
            let rw = BoundaryConfig {
                grants_rw: vec![entry(spelling, true)],
                ..Default::default()
            };
            let err = resolve(&rw, home).expect_err(spelling);
            assert!(err.contains("grants_rw"), "{spelling}: {err}");
        }
        assert!(resolve(&ro("~/datasets"), home).is_ok());
        assert!(resolve(&ro("/data"), home).is_ok());
    }

    #[test]
    fn a_relative_grant_and_a_project_rw_grant_are_refused() {
        let home = Path::new("/h");
        let err = resolve(&ro("datasets"), home).expect_err("relative");
        assert!(err.contains("not absolute"), "{err}");
        let project_rw = BoundaryConfig {
            grants_rw: vec![entry("~/scratch", false)],
            ..Default::default()
        };
        let err = resolve(&project_rw, home).expect_err("project rw");
        assert!(err.contains("user file") && err.contains(":7:"), "{err}");
    }

    /// The deny-list is one function: a secret, a credential helper and a loader variable are
    /// refused by name, a plain constant passes.
    #[test]
    fn env_refuses_secrets_and_loader_variables() {
        for bad in [
            "GH_TOKEN",
            "gh_token",
            "NPM_TOKEN",
            "DB_PASSWORD",
            "SSH_AUTH_SOCK",
            "GIT_CONFIG_COUNT",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "PATH",
        ] {
            assert!(env_refusal(bad).is_some(), "{bad}");
            let cfg = BoundaryConfig {
                env: vec![Entry {
                    value: (bad.into(), "x".into()),
                    from: origin(true),
                }],
                ..Default::default()
            };
            let err = resolve(&cfg, Path::new("/h")).expect_err(bad);
            assert!(err.contains(bad) && err.contains(":7:"), "{err}");
        }
        assert!(env_refusal("RUST_LOG").is_none());
    }

    /// Today's behaviour: no config changes nothing.
    #[test]
    fn an_empty_config_leaves_the_seat_as_it_was() {
        let home = Path::new("/h");
        let r = resolve(&BoundaryConfig::default(), home).expect("empty");
        let tight = seat::seat_view(&Mode::WRITES_ALLOWED, "here", &[] as &[&str], home);
        assert_eq!(apply(tight.clone(), &r), tight);
        let wide = seat::seat_for_level(
            Level::Wide,
            &Mode::WRITES_ALLOWED,
            "here",
            &[] as &[&str],
            home,
            Some("tok"),
        );
        assert_eq!(apply(wide.clone(), &r), wide);
    }

    /// A grant lands in a child's view naming its key; `hide` masks after it; the wide seat keeps
    /// `gh` readable and its token whatever is hidden.
    #[test]
    fn config_grants_and_hides_reach_the_seat_and_gh_stays() {
        let dir = std::env::temp_dir().join(format!("letibot-boundary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let data = home.join("datasets");
        std::fs::create_dir_all(data.join("private")).expect("data");
        let cfg = BoundaryConfig {
            grants_ro: vec![entry("~/datasets", true), entry("~/absent", true)],
            hide: vec![entry("~/datasets/private", false)],
            env: vec![Entry {
                value: ("RUST_LOG".into(), "info".into()),
                from: origin(false),
            }],
            ..Default::default()
        };
        let r = resolve(&cfg, &home).expect("resolve");
        let tight = apply(
            seat::seat_view(&Mode::WRITES_ALLOWED, "here", &[] as &[&str], &home),
            &r,
        );
        let g = tight
            .grants
            .iter()
            .find(|g| g.path() == data)
            .expect("granted");
        assert!(g.why().contains("grants_ro"), "{}", g.why());
        assert!(tight.grants.iter().all(|g| !g.path().ends_with("absent")));
        assert!(tight.masked.contains(&data.join("private")));
        assert!(
            tight
                .env
                .contains(&("RUST_LOG".to_string(), "info".to_string()))
        );
        let said = report(&tight, &r, "child", None).join("\n");
        assert!(
            said.contains("absent") && said.contains("`[boundary] hide`"),
            "{said}"
        );
        assert!(said.contains("probe: not measured here"), "{said}");

        let wide = apply(
            seat::seat_for_level(
                Level::Wide,
                &Mode::WRITES_ALLOWED,
                "here",
                &[] as &[&str],
                &home,
                Some("ghp_secretsecret"),
            ),
            &r,
        );
        assert!(
            wide.grants.is_empty(),
            "a wide view already reads everything"
        );
        assert!(wide.masked.contains(&data.join("private")));
        assert!(!wide.masked.iter().any(|p| p.ends_with(".config/gh")));
        assert!(wide.secret_env.0.iter().any(|(k, _)| k == "GH_TOKEN"));
        let said = report(&wide, &r, "top-level", None).join("\n");
        assert!(said.contains("GH_TOKEN=<redacted>"), "{said}");
        assert!(!said.contains("ghp_secretsecret"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the probe measured is three lines: the mechanism and the age, the table of
    /// namespaces (or the policy's properties), and the seals. A fixture with nothing in it
    /// says "never measured" rather than printing nothing, and does not panic.
    #[test]
    fn the_probe_lines_say_what_was_measured() {
        use crate::exec::confine::{Boundary, Namespace, NsState, Seal, SealKind, ViewSpec};
        let view = ViewSpec::project_only("/p").wide();
        let mut ns = std::collections::BTreeMap::new();
        for n in Namespace::ALL {
            ns.insert(
                n,
                NsState::Entered {
                    inode: format!("user:[{}]", n.proc_name()),
                },
            );
        }
        let mut seals = std::collections::BTreeMap::new();
        for k in SealKind::ALL {
            seals.insert(
                k,
                Seal::Held {
                    how: "tested".into(),
                },
            );
        }
        let measured = Boundary::namespaces(
            "bubblewrap 0.11.1",
            view.clone(),
            crate::exec::confine::Egress::Denied,
            ns,
            seals,
        );
        let said = probe_lines(&measured).join("\n");
        assert!(said.contains("by bubblewrap 0.11.1"), "{said}");
        assert!(said.contains("measured 0s ago"), "{said}");
        assert!(said.contains("mnt entered"), "{said}");
        assert!(
            said.contains("seals:") && said.contains("held (tested)"),
            "{said}"
        );

        let bare = Boundary::namespaces(
            "nothing",
            view,
            crate::exec::confine::Egress::Denied,
            Default::default(),
            Default::default(),
        );
        let said = probe_lines(&bare).join("\n");
        assert!(said.contains("NOT CONFINED"), "{said}");
        assert!(said.contains("never measured"), "{said}");
    }

    /// `hide` is rendered after the grant it covers, so the view cannot reach what is hidden.
    #[test]
    fn a_hidden_path_inside_a_grant_is_masked_after_the_bind() {
        use crate::exec::confine::{ConfinePlan, Egress, Session, ViewSpec, build_argv_with};
        let dir = std::env::temp_dir().join(format!("letibot-hide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let data = home.join("datasets");
        std::fs::create_dir_all(data.join("private")).expect("data");
        std::fs::create_dir_all(dir.join("project")).expect("project");
        let cfg = BoundaryConfig {
            grants_ro: vec![entry("~/datasets", true)],
            hide: vec![entry("~/datasets/private", true)],
            ..Default::default()
        };
        let r = resolve(&cfg, &home).expect("resolve");
        let s = apply(
            seat::seat_view(&Mode::WRITES_ALLOWED, "here", &[] as &[&str], &home),
            &r,
        );
        let view = s.grants.iter().fold(
            ViewSpec::project_only(dir.join("project")).masking(s.masked.iter().cloned()),
            |v, g| v.granting(g.clone()),
        );
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        )
        .join(" ");
        let bind = argv
            .find(&format!("--ro-bind {0} {0}", data.display()))
            .expect("bound");
        let mask = argv
            .find(&format!("--tmpfs {}", data.join("private").display()))
            .expect("masked");
        assert!(mask > bind, "{argv}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
