//! What a command's view holds, from `[boundary]` in the user's and the project's
//! `leticode.toml`: loaded for a seat, and drawn for `/boundary` and `letibot boundary`.

use std::path::Path;

use letibot_tools::exec::boundary::{self, BoundaryConfig, Resolved};
use letibot_tools::exec::seat::{self, Level, SeatView};
use letibot_tools::mode::Mode;

use crate::leticode_config::{self, LeticodeConfig};

/// The two levels' `[boundary]` joined. A file that does not parse is an `Err` naming the file
/// and line: the seat refuses to open, never falls back to the unconfigured view.
pub fn load(workspace: &Path) -> Result<BoundaryConfig, String> {
    let user_path = leticode_config::user_path();
    let user = LeticodeConfig::load_as(&user_path, leticode_config::Level::User)?;
    let project = match LeticodeConfig::discover(workspace) {
        Some(p) if p != user_path => LeticodeConfig::load(&p)?,
        _ => LeticodeConfig::default(),
    };
    Ok(user.boundary.overridden_by(&project.boundary))
}

/// [`load`], checked against the rules and expanded under `home`.
pub fn resolved(workspace: &Path, home: &Path) -> Result<Resolved, String> {
    boundary::resolve(&load(workspace)?, home)
}

/// The seat for `level` with the config applied. `token` is the GitHub token for a top-level
/// seat; the view only ever prints its name.
pub fn seat_for(
    level: Level,
    mode: &Mode,
    workspace: &Path,
    home: &Path,
    token: Option<&str>,
) -> Result<(SeatView, Resolved), String> {
    let r = resolved(workspace, home)?;
    let view = seat::seat_for_level(
        level,
        mode,
        "this machine",
        &seat::present_under(home),
        home,
        token,
    );
    Ok((boundary::apply(view, &r), r))
}

/// What a command gets at `level`, as lines. No token is asked for: the wide view says it is
/// injected when `gh auth token` answers. No probe either: nothing here has a session to measure.
pub fn lines(level: Level, mode: &Mode, workspace: &Path) -> Vec<String> {
    lines_with_probe(level, mode, workspace, None)
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
    let token = (level == Level::Wide).then_some("");
    match seat_for(level, mode, workspace, &home, token) {
        Ok((view, r)) => {
            let name = match level {
                Level::Wide => "top-level",
                Level::Tight => "subagent",
            };
            let mut out = boundary::report(&view, &r, name, probe);
            if level == Level::Wide {
                out.push("GH_TOKEN is set when `gh auth token` answers at session start.".into());
            }
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
        let tight = lines(Level::Tight, &Mode::WRITES_ALLOWED, &dir).join("\n");
        assert!(tight.starts_with("subagent perimeter"), "{tight}");
        assert!(
            !tight.contains("GH_TOKEN"),
            "a child carries no token: {tight}"
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
}
