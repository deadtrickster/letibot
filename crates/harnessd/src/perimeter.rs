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
/// injected when `gh auth token` answers.
pub fn lines(level: Level, mode: &Mode, workspace: &Path) -> Vec<String> {
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
            let mut out = boundary::report(&view, &r, name);
            if level == Level::Wide {
                out.push("GH_TOKEN is set when `gh auth token` answers at session start.".into());
            }
            out
        }
        Err(why) => vec![format!("boundary config refused: {why}")],
    }
}
