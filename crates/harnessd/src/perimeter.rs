//! What a command's view holds, from `[boundary]` and `[perimeter.<name>]` in the user's and the
//! project's `leticode.toml`: loaded for a seat, composed for a spawn, and drawn for `/boundary`
//! and `letibot boundary`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tools::exec::boundary::{self, BoundaryConfig, Resolved};
use letibot_tools::exec::confine::Grant;
use letibot_tools::exec::perimeter::{self, Perimeter, PerimeterTable, SUBAGENT, TOP_LEVEL};
use letibot_tools::exec::seat::{self, Level, SeatView};
use letibot_tools::mode::Mode;

use crate::leticode_config::{self, LeticodeConfig};

/// Both files' tables in one read: the `[boundary]` joined, and the `[perimeter.<name>]` tables
/// with the project's over the user's per name. A file that does not parse is an `Err` naming the
/// file and line: the seat refuses to open, never falls back to the unconfigured view.
fn both(workspace: &Path) -> Result<(BoundaryConfig, BTreeMap<String, PerimeterTable>), String> {
    let user_path = leticode_config::user_path();
    let user = LeticodeConfig::load_as(&user_path, leticode_config::Level::User)?;
    let project = match LeticodeConfig::discover(workspace) {
        Some(p) if p != user_path => LeticodeConfig::load(&p)?,
        _ => LeticodeConfig::default(),
    };
    let merged = user.overridden_by(&project);
    Ok((merged.boundary, merged.perimeters))
}

/// The two levels' `[boundary]` joined.
pub fn load(workspace: &Path) -> Result<BoundaryConfig, String> {
    both(workspace).map(|b| b.0)
}

/// [`load`], checked against the rules and expanded under `home`.
pub fn resolved(workspace: &Path, home: &Path) -> Result<Resolved, String> {
    let cfg = load(workspace)?;
    let mut r = boundary::resolve(&cfg, home)?;
    // A launcher a package manager put under the project is not on `PATH`, and is the same
    // program by another name.
    if let Some(first) = cfg.firecode_only.first() {
        r.firecode_only.extend(
            boundary::project_bins(workspace)
                .into_iter()
                .map(|p| (p, first.from.clone())),
        );
    }
    Ok(r)
}

/// The `[perimeter.<name>]` tables of both files.
pub fn tables(workspace: &Path) -> Result<BTreeMap<String, PerimeterTable>, String> {
    both(workspace).map(|b| b.1)
}

/// What the main checkout's grant says, in the words a disclosure prints beside it.
const MAIN_CHECKOUT_WHY: &str = "the MAIN checkout of the repository this worktree belongs to \
     — the canonical source, read-only: its tree, its `target/` and its `.git` are readable and a \
     write below it is refused by this grant, so nothing a child may write has changed";

/// The main checkout a linked worktree was cut from, DERIVED and never guessed: a worktree's git
/// entry names `<main>/.git/worktrees/<name>`, so `--git-common-dir`'s parent is the checkout a
/// person opens at `~/Projects/<org>/<repo>` — and where the `target/` both trees build into lives.
pub fn main_checkout(workspace: &Path) -> Option<PathBuf> {
    // A plain checkout's git entry is a directory: nothing is outside this tree, and a grant on
    // it would only restate the view's own read-write root. Checked first, so a session that is
    // not a worktree costs no process at all.
    if workspace.join(".git").is_dir() {
        return None;
    }
    let common = crate::harness::git_in(
        &workspace.display().to_string(),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let main = Path::new(common.trim()).parent()?.to_path_buf();
    // Canonical, because the view's roots and the path a command names are compared lexically:
    // a `/var` that is really `/private/var` would be a grant that covers nothing.
    let main = main.canonicalize().unwrap_or(main);
    let here = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    // The common dir's parent has to BE a checkout: a bare repository's git dir has no `<dir>/.git`
    // under its parent, and granting that parent would hand a child a directory of mirrors.
    (main != here && main.join(".git").is_dir()).then_some(main)
}

/// The one grant every child holds BY CONSTRUCTION, and no top-level session needs: a read-only
/// view of the main checkout its worktree was cut from. Reading the canonical source cannot
/// corrupt it, so this is not a grant to ask for; writing there stays refused by the same grant.
pub fn main_checkout_grant(level: Level, workspace: &Path) -> Option<Grant> {
    if level != Level::Tight {
        return None;
    }
    Some(Grant::ReadOnly {
        path: main_checkout(workspace)?,
        why: MAIN_CHECKOUT_WHY.to_string(),
    })
}

/// Add [`main_checkout_grant`] to a rendered view: the last thing a seat is given, so every door
/// that asks *what does this session's commands see* answers with it — the shell's view, the file
/// tools' fence and `/boundary` alike.
fn with_main_checkout(mut view: SeatView, level: Level, workspace: &Path) -> SeatView {
    if let Some(g) = main_checkout_grant(level, workspace)
        && !view.grants.iter().any(|had| had.path() == g.path())
    {
        view.grants.push(g);
    }
    view
}

/// The perimeter a session at `level` gets, out of the tables its config carries: the reserved
/// name for that level, defaulted when the file says nothing.
pub fn of_level(
    level: Level,
    tables: &BTreeMap<String, PerimeterTable>,
) -> Result<Perimeter, String> {
    let name = match level {
        Level::Wide => TOP_LEVEL,
        Level::Tight => SUBAGENT,
    };
    Perimeter::named(name, tables.get(name))
}

/// The perimeter for a session's own commands, from its config. A session already holding one
/// keeps it: that is how a spawn's own composition reaches its seat.
pub fn of(cfg: &crate::config::Config) -> Result<Perimeter, String> {
    if let Some(p) = &cfg.perimeter {
        return Ok((**p).clone());
    }
    of_level(
        Level::of(cfg.parent_session_id.as_deref()),
        &cfg.leticode.perimeters,
    )
}

/// The grants a session's perimeter asks for on a view built on another machine: its own table's
/// first, then `[boundary]`'s, the order a seat applies them. A file this box refuses refuses the
/// session; a `~/` path stays as written, because only that machine's shell knows its home.
pub fn grants_for_placement(
    p: &Perimeter,
    workspace: &Path,
    home: &Path,
) -> Result<Vec<boundary::HostGrant>, String> {
    let mut out = p.host_grants(home)?;
    out.extend(boundary::host_grants(&load(workspace)?, home)?);
    Ok(out)
}

/// The perimeter a CHILD of `parent` gets: the parent's at the tight level, narrowed by the table
/// `named` (the `subagent` one when the spawn named none) and the config's tables, widened by
/// what the operator granted for this spawn. A child's own children start from here.
pub fn child_of(
    parent: &Perimeter,
    named: Option<&str>,
    tables: &BTreeMap<String, PerimeterTable>,
    granted: &[perimeter::WidenItem],
    because: Option<&str>,
) -> Result<Perimeter, String> {
    let name = named.unwrap_or(SUBAGENT);
    let Some(table) = tables.get(name) else {
        if named.is_none() {
            // No table for the level: the built-in default, which is every file that says nothing.
            return parent.child_of(None, granted, because);
        }
        let known: Vec<&str> = tables.keys().map(String::as_str).collect();
        return Err(format!(
            "no `[perimeter.{name}]` table in this project's or the user's leticode.toml; the \
             tables here are: {}",
            if known.is_empty() {
                "none - `[perimeter.subagent]` and `[perimeter.top-level]` are built in".to_string()
            } else {
                known.join(", ")
            }
        ));
    };
    // A project's narrowing of `subagent` applies to every child of it, so a spawn that names a
    // table of its own gets that table ON TOP of the level's: naming one may not escape the
    // operator's narrowing.
    let table = match (named, tables.get(SUBAGENT)) {
        (Some(n), Some(base)) if n != SUBAGENT => {
            let mut merged = base.overridden_by(table);
            merged.name = n.to_string();
            merged
        }
        _ => table.clone(),
    };
    parent.child_of(Some(&table), granted, because)
}

/// The seat this session's FILE TOOLS are held to, where no substrate measured a view for its
/// commands: the perimeter's own words are the whole view. No secret is asked of the box, because
/// nothing spawns and only a command ever reads one.
pub fn seat_for_fence(
    p: &Perimeter,
    mode: &Mode,
    workspace: &Path,
    home: &Path,
) -> Result<SeatView, String> {
    let r = resolved(workspace, home)?;
    let view = p.render(
        mode,
        "this machine",
        &seat::present_under(home),
        home,
        &Default::default(),
    )?;
    // No [`with_main_checkout`] here: this seat's only consumer is `ViewSpec::of_seat`, which the
    // caller renders with `confined_grants` — where the grant is added once, not twice.
    Ok(boundary::apply(view, &r))
}

/// The seat for `level` as the config describes it, and the `[boundary]` that was applied. A
/// token is never in here: the secrets a perimeter carries are asked of the box at open, and a
/// report prints their names only.
pub fn seat_for(
    level: Level,
    mode: &Mode,
    workspace: &Path,
    home: &Path,
) -> Result<(SeatView, Resolved, Perimeter), String> {
    let r = resolved(workspace, home)?;
    let p = of_level(level, &tables(workspace)?)?;
    let view = p.render(
        mode,
        "this machine",
        &seat::present_under(home),
        home,
        &p.placeholder_values(),
    )?;
    Ok((
        boundary::apply(with_main_checkout(view, level, workspace), &r),
        r,
        p,
    ))
}

/// What a command gets at `level`, as lines. No real secret is ever resolved here: nothing is
/// spawned, and a report shows the names.
pub fn lines(level: Level, mode: &Mode, workspace: &Path) -> Vec<String> {
    lines_with_probe(level, mode, workspace, None)
}

/// THIS session's perimeter, drawn for `/boundary`: the one it actually got, which is not the
/// level's table when a spawn widened it or a parent narrowed it.
pub fn session_lines(
    p: &Perimeter,
    mode: &Mode,
    workspace: &Path,
    probe: Option<&letibot_tools::exec::Boundary>,
) -> Vec<String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let level = match p.level {
        Level::Wide => "top-level",
        Level::Tight => "subagent",
    };
    let r = match resolved(workspace, &home) {
        Ok(r) => r,
        Err(why) => return vec![format!("boundary config refused: {why}")],
    };
    let view = match p.render(
        mode,
        "this machine",
        &seat::present_under(&home),
        &home,
        &p.placeholder_values(),
    ) {
        Ok(v) => with_main_checkout(v, p.level, workspace),
        Err(why) => return vec![format!("boundary config refused: {why}")],
    };
    let mut out = boundary::report(&boundary::apply(view, &r), &r, level, probe);
    out.extend(p.lines());
    out
}

/// [`lines`] with the boundary a LIVE session measured, for `/boundary`: the config says what a
/// command was meant to get, and the probe says what one got.
pub fn lines_with_probe(
    level: Level,
    mode: &Mode,
    workspace: &Path,
    probe: Option<&letibot_tools::exec::Boundary>,
) -> Vec<String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    match seat_for(level, mode, workspace, &home) {
        Ok((view, r, p)) => {
            let name = match level {
                Level::Wide => "top-level",
                Level::Tight => "subagent",
            };
            let mut out = boundary::report(&view, &r, name, probe);
            out.extend(p.lines());
            out
        }
        Err(why) => vec![format!("boundary config refused: {why}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str, body: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letibot-perimeter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a workspace");
        std::fs::write(dir.join("leticode.toml"), body).expect("a config");
        dir
    }

    /// `/boundary` and `letibot boundary` draw both levels, name the key a hidden path came from
    /// and never print the token's value.
    #[test]
    fn the_view_shows_paths_hidden_and_env_without_the_token() {
        let dir = workspace(
            "view",
            "[boundary]\nhide = [\"/opt/hidden-thing\"]\n[boundary.env]\nRUST_LOG = \"info\"\n",
        );
        let wide = lines(Level::Wide, &Mode::WRITES_ALLOWED, &dir).join("\n");
        assert!(wide.starts_with("top-level perimeter"), "{wide}");
        assert!(
            wide.contains("/opt/hidden-thing - `[boundary] hide` at"),
            "{wide}"
        );
        assert!(wide.contains("RUST_LOG=info"), "{wide}");
        assert!(wide.contains("GH_TOKEN=<redacted>"), "{wide}");
        assert!(
            wide.contains("/.ssh - credential or profile, fixed"),
            "{wide}"
        );
        assert!(
            wide.contains("perimeter: `top-level` (top level)"),
            "{wide}"
        );
        let tight = lines(Level::Tight, &Mode::WRITES_ALLOWED, &dir).join("\n");
        assert!(tight.starts_with("subagent perimeter"), "{tight}");
        assert!(
            !tight.contains("GH_TOKEN"),
            "a child carries no token: {tight}"
        );
        assert!(
            tight.contains("perimeter: `subagent` (subagent level)"),
            "{tight}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A config that does not parse is shown as refused, with the file and line, and no view.
    #[test]
    fn a_refused_config_is_shown_refused_and_not_as_a_view() {
        let dir = workspace("refused", "[boundary]\nhide = []\ngrants = []\n");
        let said = lines(Level::Tight, &Mode::WRITES_ALLOWED, &dir).join("\n");
        assert!(said.starts_with("boundary config refused"), "{said}");
        assert!(said.contains("leticode.toml:3:"), "{said}");
        assert!(!said.contains("paths:"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The report prints what the probe measured, not only what the config
    /// asked for - and says so when nothing was measured.
    #[test]
    fn the_report_shows_what_the_probe_measured() {
        use letibot_tools::exec::confine::{
            Boundary, Egress, Namespace, NsState, Seal, SealKind, ViewSpec,
        };
        let dir = workspace("probe", "[boundary]\ngrants_ro = [\"/opt/data\"]\n");
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
            ViewSpec::project_only(&dir).wide(),
            Egress::Denied,
            ns,
            seals,
        );
        let said =
            lines_with_probe(Level::Wide, &Mode::WRITES_ALLOWED, &dir, Some(&measured)).join("\n");
        assert!(
            said.contains("probe: confined, by bubblewrap 0.11.1, measured"),
            "{said}"
        );
        assert!(said.contains("mnt entered"), "{said}");
        assert!(said.contains("seals:"), "{said}");
        assert!(!said.contains("not measured here"), "{said}");

        let none = lines(Level::Wide, &Mode::WRITES_ALLOWED, &dir).join("\n");
        assert!(none.contains("probe: not measured here"), "{none}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A placement's view is asked for the same grants a seat on this box holds: the perimeter's own
    /// table and `[boundary]`'s, each as written, because a `~/` there is that machine's home. A file
    /// this box refuses refuses the session, rather than opening an unchecked view there.
    #[test]
    fn the_grants_a_placement_is_asked_for_are_the_seats_own() {
        let dir = workspace(
            "placement-grants",
            "[boundary]\ngrants_ro = [\"/opt/data\"]\n",
        );
        let home = std::path::Path::new("/h");
        let top =
            of_level(Level::Wide, &tables(&dir).expect("the tables parse")).expect("the top level");
        let got = grants_for_placement(&top, &dir, home).expect("both are grantable");
        assert!(
            got.iter()
                .any(|g| g.path == "/opt/data" && g.key() == "[boundary] grants_ro"),
            "the operator's own grants travel, unexpanded: {got:?}"
        );

        // A project file may not make a path writable, and it may not reach a credential store: the
        // rules run before anything is handed over.
        let rw = workspace(
            "placement-grants-rw",
            "[boundary]\ngrants_rw = [\"~/scratch\"]\n",
        );
        let p = of_level(Level::Wide, &tables(&rw).expect("the tables parse"));
        let err = p
            .and_then(|p| grants_for_placement(&p, &rw, home))
            .expect_err("a project's rw grant");
        assert!(err.contains("user file"), "{err}");
        let ssh = workspace(
            "placement-grants-ssh",
            "[boundary]\ngrants_ro = [\"~/.ssh/id_ed25519\"]\n",
        );
        let p = of_level(Level::Wide, &tables(&ssh).expect("the tables parse"));
        let err = p
            .and_then(|p| grants_for_placement(&p, &ssh, home))
            .expect_err("a key");
        assert!(err.contains(".ssh"), "{err}");
        for d in [dir, rw, ssh] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// The project's `[perimeter.subagent]` table narrows every child of that project, and a
    /// child composes it with its parent rather than replacing it.
    #[test]
    fn a_project_perimeter_narrows_a_child() {
        let dir = workspace(
            "narrow",
            "[perimeter.subagent]\nhide = [\"/opt/secret-data\"]\n\
             [perimeter.subagent.env]\nRUST_LOG = \"warn\"\n\
             [perimeter.coder]\nhide = [\"/opt/also-hidden\"]\n",
        );
        let tables = tables(&dir).expect("the tables parse");
        let top = of_level(Level::Wide, &tables).expect("the top level");
        let child = child_of(&top, None, &tables, &[], None).expect("a child");
        assert_eq!(child.level, Level::Tight);
        assert!(
            child.hide.iter().any(|e| e.value == "/opt/secret-data"),
            "the project's table is the child's: {child:?}"
        );
        // A named table narrows further, and its hide list is the parent's plus its own.
        let named = child_of(&top, Some("coder"), &tables, &[], None).expect("a named child");
        let hides: Vec<&str> = named.hide.iter().map(|e| e.value.as_str()).collect();
        assert!(hides.contains(&"/opt/secret-data"), "{hides:?}");
        assert!(hides.contains(&"/opt/also-hidden"), "{hides:?}");
        // And the child's own child inherits the same narrowing: a level's table is not re-read
        // wider by the generation below it.
        let grandchild = child_of(&child, None, &tables, &[], None).expect("a grandchild");
        assert!(
            grandchild
                .hide
                .iter()
                .any(|e| e.value == "/opt/secret-data"),
            "{grandchild:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project file may not widen a perimeter: it is a file the sessions it governs can edit.
    #[test]
    fn a_project_perimeter_may_not_grant() {
        let dir = workspace(
            "grant",
            "[perimeter.subagent]\ngrants_ro = [\"~/datasets\"]\n",
        );
        let tables = tables(&dir).expect("the tables parse");
        let top = of_level(Level::Wide, &tables).expect("the top level");
        let err = child_of(&top, None, &tables, &[], None).expect_err("a project grant");
        assert!(err.contains("a project file may not grant"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tables a spawn composes from: the user file at its own level, the project's from its
    /// walk-up, the project's over the user's - the two reads `cli.rs` makes for `cfg.leticode`.
    fn composed_tables(dir: &std::path::Path) -> Result<BTreeMap<String, PerimeterTable>, String> {
        let user = crate::leticode_config::load_user()?;
        let project = match LeticodeConfig::discover(dir) {
            Some(p) => LeticodeConfig::load(&p)?,
            None => LeticodeConfig::default(),
        };
        Ok(user.overridden_by(&project).perimeters)
    }

    /// The user file is the one that may widen: its `[perimeter.subagent] network` reaches the child
    /// a spawn composes and opens its seat, while the same line in a project file is still refused.
    #[test]
    fn a_user_files_network_grant_opens_a_child_and_a_projects_is_refused() {
        use letibot_tools::exec::confine::Egress;
        let mut env = letibot_testenv::EnvGuard::lock();
        let base =
            std::env::temp_dir().join(format!("letibot-perimeter-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("user/letibot")).expect("a config dir");
        std::fs::create_dir_all(base.join("empty")).expect("an empty config dir");
        std::fs::write(
            base.join("user/letibot/leticode.toml"),
            "[perimeter.subagent]\nnetwork = [\"subagents fetch, build and push\"]\n",
        )
        .expect("the user file");
        env.set("XDG_CONFIG_HOME", base.join("user"));

        let dir = workspace(
            "net-user",
            "[perimeter.subagent]\nhide = [\"/opt/hidden\"]\n",
        );
        let tables = composed_tables(&dir).expect("both files parse");
        let top = of_level(Level::Wide, &tables).expect("the top level");
        let child = child_of(&top, None, &tables, &[], None).expect("a child of a wide seat");
        assert!(
            child
                .network
                .iter()
                .any(|e| e.value.contains("subagents fetch") && e.from.user),
            "the user file's grant is not the child's: {child:?}"
        );
        // And it is the seat's, not a word on a struct: the tight view reaches the network.
        let home = std::path::Path::new("/h");
        let view = child
            .render(
                &Mode::WRITES_ALLOWED,
                "this machine",
                &[] as &[&str],
                home,
                &Default::default(),
            )
            .expect("a tight seat with the operator's egress");
        assert!(matches!(view.egress, Egress::Host { .. }), "{view:?}");

        // The same line in the project file, with no user file at all, is the widening a session
        // could edit for itself.
        env.set("XDG_CONFIG_HOME", base.join("empty"));
        let project = workspace(
            "net-project",
            "[perimeter.subagent]\nnetwork = [\"a project says so\"]\n",
        );
        let tables = composed_tables(&project).expect("both files parse");
        let top = of_level(Level::Wide, &tables).expect("the top level");
        let err = child_of(&top, None, &tables, &[], None).expect_err("a project's egress");
        assert!(
            err.contains("a project file may not grant the network"),
            "{err}"
        );
        for d in [dir, project] {
            let _ = std::fs::remove_dir_all(&d);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A git checkout and a worktree cut from it beside it, which is the shape `task_start`
    /// arranges and the one place a child's view has to reach past its own tree. The commit is
    /// given its own identity and no signing, because a box whose git signs commits would
    /// otherwise fail a fixture over a key this test has no business holding.
    fn repo_and_worktree(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "letibot-main-checkout-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("a parent");
        let base = base.canonicalize().expect("a real directory");
        // The fixture's own project file, so `LeticodeConfig::discover` stops here: a temp
        // directory under somebody's tree would otherwise read that tree's config, and this test
        // would measure the box instead of the rule.
        std::fs::write(
            base.join("leticode.toml"),
            "# this fixture's own, so discovery stops here\n",
        )
        .expect("a project file");
        let main = base.join("letibot");
        std::fs::create_dir_all(main.join("crates/tui/src/app")).expect("a source tree");
        std::fs::write(main.join("Cargo.toml"), "[workspace]\n").expect("a manifest");
        std::fs::write(
            main.join("crates/tui/src/app/mod.rs"),
            "// the file a child reads to learn what the tree says\n",
        )
        .expect("a source file");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&main)
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "user.name=fixture",
                ])
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one"]);
        let wt = base.join("agent-x");
        git(&[
            "worktree",
            "add",
            "-q",
            wt.to_str().expect("a utf-8 path"),
            "-b",
            "agent/x",
        ]);
        // Made after the commit, so it is the build cache this repository has and not a file in
        // its history: a child reads the binary, and never writes one here.
        std::fs::create_dir_all(main.join("target/release")).expect("a build cache");
        std::fs::write(main.join("target/release/harnessd"), "#!/bin/sh\n").expect("a binary");
        let home = base.join("home");
        std::fs::create_dir_all(&home).expect("a home");
        (main, wt, home)
    }

    /// The main checkout is DERIVED and never guessed: `--git-common-dir` answers `<main>/.git`
    /// from anywhere in the tree, and a plain checkout is its own main checkout.
    #[test]
    fn the_main_checkout_is_derived_from_git() {
        let (main, wt, _) = repo_and_worktree("derive");
        assert_eq!(main_checkout(&wt).as_deref(), Some(main.as_path()));
        // A plain checkout: the repository IS the workspace, and nothing is added for it.
        assert_eq!(main_checkout(&main), None);
        // A directory git cannot answer about answers nothing rather than guessing a parent. (A
        // directory merely outside every repository is not a shape a fixture can build: a temp dir
        // under a worktree's `target/` resolves to that worktree's main checkout, same rule.)
        assert_eq!(main_checkout(&main.join("nowhere")), None);
        // And a top-level session — the operator's own — never gets the grant at all.
        assert!(main_checkout_grant(Level::Wide, &wt).is_none());
        assert!(main_checkout_grant(Level::Tight, &main).is_none());
        let _ = std::fs::remove_dir_all(main.parent().expect("the parent"));
    }

    /// **A child reads its MAIN checkout and writes only its own tree.** Reading the canonical
    /// source cannot corrupt it, so it is in the view by construction and asks nobody; the SAME
    /// read-only grant is what refuses a write there, so nothing about the write boundary moved.
    #[test]
    fn a_child_reads_its_main_checkout_and_writes_only_its_own_tree() {
        use letibot_tools::exec::confine::ViewSpec;
        use letibot_tools::exec::fence::FileFence;

        let (main, wt, home) = repo_and_worktree("child");
        let (view, _, _) = seat_for(Level::Tight, &Mode::WRITES_ALLOWED, &wt, &home)
            .expect("a tight view for a worktree");
        let granted = view
            .grants
            .iter()
            .find(|g| g.path() == main.as_path())
            .unwrap_or_else(|| panic!("the main checkout is in a child's view: {view:?}"));
        assert!(
            matches!(granted, Grant::ReadOnly { .. }),
            "read-only, never writable: {granted:?}"
        );
        assert!(
            !view
                .grants
                .iter()
                .any(|g| g.path() == main.as_path() && matches!(g, Grant::ReadWrite { .. })),
            "no writable grant on the main checkout: {view:?}"
        );

        // The fence the file tools obey holds the same view, so a read reaches it and a write
        // stops at the same grant — the boundary's answer, not a card's.
        let fence = FileFence::of(&view, &ViewSpec::of_seat(wt.clone(), &view));
        for read in [
            "Cargo.toml",
            "crates/tui/src/app/mod.rs",
            "target/release/harnessd",
        ] {
            assert!(
                fence.refusal(&main.join(read), false).is_none(),
                "a child reads `{read}` with no grant and no card"
            );
        }
        for write in ["Cargo.toml", "target/release/harnessd"] {
            let said = fence
                .refusal(&main.join(write), true)
                .unwrap_or_else(|| panic!("a write to `{write}` is refused"));
            assert!(
                said.contains("read-only"),
                "refused because the view holds it read-only, not because it is absent: {said}"
            );
        }
        // The inverse: its own worktree is writable, including a file that does not exist yet.
        assert!(fence.refusal(&wt.join("Cargo.toml"), true).is_none());
        assert!(fence.refusal(&wt.join("crates/new.rs"), true).is_none());

        // And the operator's own session is untouched: a wide view reads the whole machine and
        // gains nothing from this rule.
        let (wide, _, _) =
            seat_for(Level::Wide, &Mode::WRITES_ALLOWED, &main, &home).expect("a wide view");
        assert!(
            !wide.grants.iter().any(|g| g.path() == main.as_path()),
            "a top-level session's view is what it was: {wide:?}"
        );
        let _ = std::fs::remove_dir_all(main.parent().expect("the parent"));
    }
}
