//! What a command's view holds, from `[boundary]` and `[perimeter.<name>]` in the user's and the
//! project's `leticode.toml`: loaded for a seat, composed for a spawn, and drawn for `/boundary`
//! and `letibot boundary`.

use std::collections::BTreeMap;
use std::path::Path;

use letibot_tools::exec::boundary::{self, BoundaryConfig, Resolved};
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
    Ok((boundary::apply(view, &r), r, p))
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
        Ok(v) => v,
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

    /// Item 3's last clause: the report prints what the probe MEASURED, not only what the config
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
}
