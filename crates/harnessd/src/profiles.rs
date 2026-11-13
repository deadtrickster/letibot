//! The daemon's half of a role profile: where [`letibot_tools::Profiles`] meets this box's seats.
//!
//! A profile is a file (`crates/tools/src/profiles.rs`); a SEAT is what a daemon can open (its
//! backend class, its adjudicator need, its plan mode). A profile names the seat it is seated on,
//! so `--role explore` and `task {role: "explore"}` reach the same table and one mechanism answers
//! both. A name that is a seat's own (`--role m1`) resolves to a profile that is nothing but that
//! seat - the behaviour every session had before profiles existed.

use std::path::PathBuf;
use std::sync::Arc;

use letibot_tools::builtins::task::RoleOffer;
use letibot_tools::profiles::{Profile, Profiles};

use crate::config::{Config, Seat};

/// Every profile this box has, read from the session's workspace and the operator's config dir.
///
/// Read per open and per spawn, so a file edited between two children is in force for the second.
/// Ten small files and a `read_dir`; the profile is a fact about the tree, and a session that
/// cached it would be a session working to a policy the operator had replaced.
pub fn table(cfg: &Config) -> Profiles {
    Profiles::load(&cfg.workspace, config_dir().as_deref())
}

/// `~/.config/letibot`, where the operator's own roles live beside `providers.toml`.
fn config_dir() -> Option<PathBuf> {
    let file = letibot_provider::keys::config_file();
    file.parent().map(|p| p.to_path_buf())
}

/// The profile a name resolves to: a file this box has, or - when the name is a seat's own, or one
/// of the seat's aliases (`m1`, `plan`, `keeper`) - a profile that is nothing but that seat.
///
/// The refusal names both tables. A name nobody answers to is a fact about the call, and a model
/// that asked for `reviewer` on a box whose project renamed it needs the list, not a typo report.
pub fn resolve(table: &Profiles, name: &str) -> Result<Profile, String> {
    if let Some(row) = table.find(name)? {
        return Ok(row);
    }
    if let Ok(seat) = Seat::parse(name) {
        return Ok(of_seat(table, seat));
    }
    let profiles: Vec<String> = table.names().iter().map(|n| format!("`{n}`")).collect();
    Err(format!(
        "`{name}` is not a role profile or a seat this build has. The profiles are {}. The seats \
         are orchestrator, planner, researcher, coder, runner, leticode, gatekeeper and \
         notes_keeper (a profile may name another seat in its `seat:`).",
        profiles.join(", ")
    ))
}

/// The profile a seat is seated as: the file of that name when this box has one, and otherwise a
/// profile that is only the seat - no overlay, no narrowing, no list.
pub fn of_seat(table: &Profiles, seat: Seat) -> Profile {
    match table.find(seat.as_str()) {
        Ok(Some(row)) => row,
        // A broken file is not a reason to refuse a session at open: the seat is still a seat, and
        // the breakage is said where a name is asked for (`resolve`).
        Ok(None) | Err(_) => Profile::of_seat(seat.as_str()),
    }
}

/// The profile THIS session is seated as: the one its overlay names, else its seat's own.
pub fn for_session(cfg: &Config) -> Profile {
    match cfg.seat_overlay.as_ref().and_then(|o| o.profile.clone()) {
        Some(profile) => (*profile).clone(),
        None => of_seat(&table(cfg), cfg.seat),
    }
}

/// The word a session's row shows for its role: the profile it was seated as, else its seat's
/// name. The review queue's own session is the one seat whose ROLE table is not its seat's
/// ([`crate::sessions::gatekeeper::SEAT`] says why), so it says what it is seated as. A one-line
/// answer and not a profile read, because a row must not cost a `read_dir`.
pub fn role_name(cfg: &Config) -> String {
    if crate::sessions::gatekeeper::is_the_queue_session(cfg) {
        return "review_queue".into();
    }
    cfg.seat_overlay
        .as_ref()
        .and_then(|o| o.profile.as_ref())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| cfg.seat.as_str().to_string())
}

/// The roles this session's profile lets it start, with the one line each - the list `task`'s
/// description carries and the spawn enforces. A name the box has no profile for is not offered:
/// an allowlist entry that resolves to nothing is a promise the refusal would have to break.
pub fn offers(table: &Profiles, profile: &Profile) -> Vec<RoleOffer> {
    profile
        .subagents()
        .into_iter()
        .filter_map(|name| {
            let row = resolve(table, &name).ok()?;
            Some(RoleOffer {
                name: row.name.clone(),
                purpose: row.purpose.clone(),
            })
        })
        .collect()
}

/// What this seat may reach through `task`: the roles its profile lets it start, with the one
/// line each, and - for the review queue's own session - the children it was handed instead.
///
/// One function, because the description `task` writes and the refusal its spawn makes must be
/// the same list: a seat told about a role it cannot start is a prompt contradicting a tool, and a
/// seat refused a role its description promised is worse.
pub fn reach(cfg: &Config, table: &Profiles) -> letibot_tools::builtins::task::TaskReach {
    use letibot_tools::builtins::task::{RoleOffer, TaskReach};
    if crate::sessions::gatekeeper::is_the_queue_session(cfg) {
        // The queue's clock starts its reviewers and nothing else, and `gatekeeper` is the daemon's
        // own seat - no profile may name it, so this is the one door onto it.
        let gatekeeper = of_seat(table, Seat::Gatekeeper);
        return TaskReach::own_children().with_roles(vec![RoleOffer {
            name: gatekeeper.name.clone(),
            purpose: gatekeeper.purpose.clone(),
        }]);
    }
    TaskReach::anywhere().with_roles(offers(table, &for_session(cfg)))
}

/// The overlay a session is seated with: the profile's, and - for a child - the parent's tool
/// table as a ceiling. `None` when the session is seated on its seat's own profile and has no
/// parent, which is every session that had none before profiles existed.
pub fn overlay(
    profile: Option<Profile>,
    parent_tools: Option<Vec<String>>,
) -> Option<crate::config::SeatOverlay> {
    if profile.is_none() && parent_tools.is_none() {
        return None;
    }
    Some(crate::config::SeatOverlay {
        profile: profile.map(Arc::new),
        parent_tools,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture config with no workspace and no config dir: the built-in table and nothing else.
    fn a_config() -> Config {
        Config::for_this_box(std::env::temp_dir())
    }

    #[test]
    fn every_built_in_resolves_to_a_seat_this_build_has() {
        let table = Profiles::builtins();
        for name in table.names() {
            let profile = resolve(&table, &name).expect("resolves");
            let seat = Seat::parse(profile.seat()).unwrap_or_else(|e| panic!("{name}: {e}"));
            // The three narrowing profiles are seated on a seat a person can have, and the daemon's
            // own two are startable by no profile (the tools crate's own test walks that).
            assert!(
                !matches!(name.as_str(), "explore" | "reviewer") || seat == Seat::Coder,
                "{name} is seated on {}",
                seat.as_str()
            );
            assert!(!profile.purpose.is_empty(), "{name} has no purpose");
        }
    }

    #[test]
    fn a_seat_name_is_a_profile_and_an_alias_reaches_its_seat() {
        let table = Profiles::builtins();
        // `m1` is an alias for orchestrator, and orchestrator's own file is the profile it lands on.
        let m1 = resolve(&table, "m1").expect("an alias resolves");
        assert_eq!(m1.name, "orchestrator");
        assert_eq!(Seat::parse(m1.seat()).expect("a seat"), Seat::Orchestrator);
        // `plan` is the alias the launcher's help uses for the planner seat.
        let plan = resolve(&table, "plan").expect("an alias resolves");
        assert_eq!(plan.name, "planner");
    }

    #[test]
    fn an_unknown_role_is_refused_with_both_lists() {
        let table = Profiles::builtins();
        let why = resolve(&table, "reviewr").expect_err("a typo is not a role");
        assert!(why.contains("`reviewer`"), "{why}");
        assert!(why.contains("not a role profile or a seat"), "{why}");
    }

    /// `explore` and `reviewer` are the two profiles that narrow: read-only, keeping a shell, and
    /// starting nothing of their own.
    #[test]
    fn the_narrowing_profiles_narrow() {
        let table = Profiles::builtins();
        for name in ["explore", "reviewer"] {
            let p = resolve(&table, name).expect("resolves");
            let tools = p.tools.clone().expect("it names its tools");
            assert!(tools.iter().any(|t| t == "read"), "{name}: {tools:?}");
            assert!(tools.iter().any(|t| t == "bash"), "{name}: {tools:?}");
            assert!(!tools.iter().any(|t| t == "write" || t == "edit"), "{name}");
            assert!(p.access.is_some(), "{name} narrows the gate too");
            assert!(p.subagents().is_empty(), "{name} starts nothing");
        }
    }

    #[test]
    fn the_offers_are_the_profiles_parents_may_start_with_their_purpose() {
        let table = Profiles::builtins();
        let coder = of_seat(&table, Seat::Coder);
        let offers = offers(&table, &coder);
        let names: Vec<&str> = offers.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"explore"), "{names:?}");
        assert!(!names.contains(&"gatekeeper"), "{names:?}");
        assert!(
            offers.iter().all(|r| !r.purpose.is_empty()),
            "a role with no purpose is a role a model cannot choose: {offers:?}"
        );
    }

    /// A box with no profile directory and a session with no overlay: the seat's own table, which is
    /// what every session got before this existed.
    #[test]
    fn a_session_with_no_profile_is_still_its_seat() {
        let cfg = a_config();
        let profile = for_session(&cfg);
        assert_eq!(profile.name, "orchestrator");
        assert!(profile.overlay.is_empty());
        assert!(overlay(None, None).is_none());
    }
}
