//! `letibot` - one executable, several roles.
//!
//! A person types `letibot`. `sudo` runs a program path with no arguments. The
//! daemon execs the render helpers by path. Those three callers cannot agree on an
//! interface, so this binary offers both and picks between them:
//!
//! ```text
//!   invoked as                         by whom            example
//!   ─────────────────────────────────  ─────────────────  ────────────────────────────
//!   harnessd                           systemd/operator   …/harnessd --workspace …
//!   letibot-tui                        a person           …/letibot-tui --socket S
//!   letibot-askpass                    sudo, by path      SUDO_ASKPASS=…/letibot-askpass
//!   letibot-m1                         the rig            …/letibot-m1
//!   letibot-render / -render-qwen      the fidelity gate  …/letibot-render <path>
//!
//!   letibot <word> [args…]             a person           letibot daemon --workspace .
//! ```
//!
//! # Why argv[0] and not only subcommands
//!
//! `sudo` is handed a program path and invokes it with no arguments - the prompt
//! is `argv[1]`, so `letibot askpass` cannot work there; there is nowhere to put the
//! word `askpass`. `crates/harnessd/src/sudo.rs` writes `SUDO_ASKPASS=…` pointing at a
//! file and looks for that file beside the daemon by name. So a role has to be
//! reachable from the name the file is installed under, which is what a symlink gives
//! it. The same is true of anything exec'd rather than typed: it knows a path, not a
//! CLI. Subcommands exist for the roles a PERSON starts; both, not either.
//!
//! # Why one binary
//!
//! The installed binaries all link `sessionlog`, `transcript`, `tools` and `provider`.
//! One binary links each once, so `askpass` does not cost a whole Rust runtime, and it is
//! one download, one checksum, one version.
//!
//! # The name table is a REGISTRY, and a duplicate is refused at BUILD time
//!
//! A dispatch table can hold a name twice. Rust does not refuse a duplicate `match` arm:
//! it warns `unreachable pattern`, the build succeeds, and the first arm wins by source
//! order. With the clippy step advisory, nothing would catch it.
//!
//! So there is one table, and the two `const` assertions below make a duplicate a
//! COMPILE ERROR with the offence in the message. Deliberately stronger than a test,
//! because a test that does not gate is the same as no test - and stronger than the
//! `match` this replaced, which could not see across the two kinds.
//!
//! # The symlinks are the interface, so they are load-bearing
//!
//! A packaging bug that drops `letibot-askpass` makes `sudo` fall back silently to a
//! terminal that is not there. `install.sh` creates the symlinks and verifies each one lands.

use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

mod boundary;
mod browser;
mod github;
pub mod launcher;
pub mod migrate;
mod notes;
mod roles;
mod rooms;
mod stats;

/// What this invocation is being asked to be. No catch-all variant: an unknown name is an
/// error rather than a guess, because the guess would be "be the daemon" for a typo, and that is
/// a long-running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `sudo`'s helper: one question over the socket, answer on stdout.
    Askpass,
    /// The session owner: socket, store, model connection.
    Daemon,
    /// A head: the terminal interface, attached to a daemon.
    Head,
    /// The M1 measurement, an offline exit criterion.
    M1,
    /// The GLM renderer, for the fidelity gate.
    Render,
    /// The Qwen renderer, for the fidelity gate.
    RenderQwen,
}

/// Every role, for the compile-time checks and for tests.
pub const ALL_ROLES: &[Role] = &[
    Role::Askpass,
    Role::Daemon,
    Role::Head,
    Role::M1,
    Role::Render,
    Role::RenderQwen,
];

/// How a name reaches a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A file NAME, for `argv[0]` - the symlink `install.sh` creates.
    Program,
    /// A WORD a person types after `letibot`.
    Word,
}

/// THE TABLE. One entry per name: what it means, and how it is reached. Exactly one
/// [`Kind::Program`] entry per role - the name `install.sh` symlinks and the name messages
/// print. Both of those are asserted below.
pub const REGISTRY: &[(&str, Role, Kind)] = &[
    // The names as installed: what a symlink is called, and what `argv[0]` says.
    ("harnessd", Role::Daemon, Kind::Program),
    ("letibot-tui", Role::Head, Kind::Program),
    ("letibot-askpass", Role::Askpass, Kind::Program),
    ("letibot-m1", Role::M1, Kind::Program),
    ("letibot-render", Role::Render, Kind::Program),
    ("letibot-render-qwen", Role::RenderQwen, Kind::Program),
    // The words a person types. `tui` and `head` both map to the head on purpose, because
    // the crate is `crates/tui` and the protocol calls it a head - and they are two entries
    // in one table rather than an alternation in a match arm, which is what can be checked.
    ("daemon", Role::Daemon, Kind::Word),
    ("tui", Role::Head, Kind::Word),
    ("head", Role::Head, Kind::Word),
    ("askpass", Role::Askpass, Kind::Word),
    ("m1", Role::M1, Kind::Word),
    ("render", Role::Render, Kind::Word),
    ("render-qwen", Role::RenderQwen, Kind::Word),
];

// --- the checks, all of them at compile time ------------------------------------

/// `==` for `&str` in a `const fn`: the operator cannot be used in const context.
const fn name_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    let mut i = 0;
    while i < x.len() {
        if x[i] != y[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `==` for `Role` in a `const fn`, for the same reason.
const fn role_eq(a: Role, b: Role) -> bool {
    matches!(
        (a, b),
        (Role::Askpass, Role::Askpass)
            | (Role::Daemon, Role::Daemon)
            | (Role::Head, Role::Head)
            | (Role::M1, Role::M1)
            | (Role::Render, Role::Render)
            | (Role::RenderQwen, Role::RenderQwen)
    )
}

/// Every name is unique - across BOTH kinds, so a word may not collide with a program
/// name either. That cross-kind collision is the one a `match` could never see.
const fn is_disjoint(reg: &[(&str, Role, Kind)]) -> bool {
    let mut i = 0;
    while i < reg.len() {
        let mut j = i + 1;
        while j < reg.len() {
            if name_eq(reg[i].0, reg[j].0) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Every role has exactly one `Program` name, and at least one `Word` - a role nobody
/// can type is a role nobody can use, and a role with two program names is a symlink
/// question with no answer.
const fn every_role_has_one_program_name(reg: &[(&str, Role, Kind)]) -> bool {
    let mut r = 0;
    while r < ALL_ROLES.len() {
        let mut programs = 0;
        let mut words = 0;
        let mut i = 0;
        while i < reg.len() {
            if role_eq(reg[i].1, ALL_ROLES[r]) {
                match reg[i].2 {
                    Kind::Program => programs += 1,
                    Kind::Word => words += 1,
                }
            }
            i += 1;
        }
        if programs != 1 || words == 0 {
            return false;
        }
        r += 1;
    }
    true
}

/// The build refuses a duplicate or an unreachable name. See the module docs: a
/// repeated `match` arm is only a warning, and the first one silently wins.
const REGISTRY_IS_DISJOINT: () = assert!(
    is_disjoint(REGISTRY),
    "a name appears twice in the dispatch table — a name that can mean two roles \
     resolves by source order, which is not a decision anybody made"
);

const EVERY_ROLE_IS_REACHABLE: () = assert!(
    every_role_has_one_program_name(REGISTRY),
    "a role does not have exactly one Program name and at least one Word — the \
     Program name is the symlink install.sh creates and the name messages print"
);

// --- the lookups -----------------------------------------------------------------

impl Role {
    /// The role a program NAME means, for the `argv[0]` path. Takes a file name, not a path, so
    /// this stays a pure string match. `None` for `letibot` itself - that is the dispatcher -
    /// and for anything unrecognised.
    pub fn from_argv0(name: &OsStr) -> Option<Role> {
        lookup(name.to_str()?, Kind::Program)
    }

    /// The role a typed WORD means, for the subcommand path.
    pub fn from_subcommand(word: &str) -> Option<Role> {
        lookup(word, Kind::Word)
    }

    /// The name this role is installed under, for messages and for packaging.
    pub fn program(self) -> &'static str {
        REGISTRY
            .iter()
            .find(|(_, r, k)| *r == self && *k == Kind::Program)
            .map(|(n, _, _)| *n)
            .expect("every role has exactly one Program name, asserted at compile time")
    }

    /// Every name this role answers to, for `--help` and for tests.
    pub fn names(self) -> impl Iterator<Item = (&'static str, Kind)> {
        REGISTRY
            .iter()
            .filter(move |(_, r, _)| *r == self)
            .map(|(n, _, k)| (*n, *k))
    }
}

fn lookup(name: &str, kind: Kind) -> Option<Role> {
    REGISTRY
        .iter()
        .find(|(n, _, k)| *k == kind && *n == name)
        .map(|(_, r, _)| *r)
}

fn main() -> ExitCode {
    // The two const assertions are used here so they cannot be optimised away, and so
    // a reader who greps for `assert` finds the check in the flow rather than only in
    // a const block somewhere above.
    let () = REGISTRY_IS_DISJOINT;
    let () = EVERY_ROLE_IS_REACHABLE;

    let mut argv = std::env::args_os();
    let arg0 = argv.next().unwrap_or_default();
    let rest: Vec<std::ffi::OsString> = argv.collect();

    // The name we were invoked by wins. A symlink called `letibot-askpass` is
    // `sudo` talking, and its first argument is the prompt - reading it as a
    // subcommand would eat the prompt and then refuse an unknown role.
    if let Some(role) = Path::new(&arg0).file_name().and_then(Role::from_argv0) {
        return roles::run(role, &rest);
    }

    // Otherwise this is a person typing. `letibot <word> [args…]`, or bare `letibot`
    // for the launcher.
    match rest.first().and_then(|s| s.to_str()) {
        Some(word) => match Role::from_subcommand(word) {
            Some(role) => roles::run(role, &rest[1..]),
            None => {
                // Verbs over the metrics file , not roles: nothing invokes them by
                // a program name, so they have no symlink and no row in the table.
                let tail = || -> Vec<String> {
                    rest[1..]
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect()
                };
                if word == "stats" {
                    stats::run_stats(&tail())
                } else if word == "boundary" {
                    boundary::run(&tail())
                } else if word == "notes" {
                    notes::run(&tail())
                } else if word == "browser" {
                    browser::run(&tail())
                } else if word == "rooms" {
                    rooms::run(&tail())
                } else if word == "fb" {
                    stats::run_fb(&tail())
                } else if word == "migrate" || word == "--migrate" {
                    // The spelling the operator and the brief use, so the launcher can pass
                    // its own line through unchanged: `letibot --migrate SESSION --to NAME`.
                    migrate::run(&tail())
                } else if word == "attach" {
                    // `letibot attach github <org|owner/repo>`: a source and then a scope, checked
                    // before anything is polled so a typo is not a seat on somebody else's repo.
                    github::run(&tail())
                } else if word == "--help" || word == "-h" || word == "help" {
                    print_usage();
                    ExitCode::SUCCESS
                } else if word == "--version" || word == "-V" {
                    println!("letibot {}", env!("CARGO_PKG_VERSION"));
                    ExitCode::SUCCESS
                } else if migrate::is_far_verb(word) {
                    // The far side of a move runs THIS binary's own sub-verbs: a host's
                    // `letibot --export-session …` is this same executable at the same commit, so
                    // the verb is recognised here and passed on with its flags.
                    let mut line = vec![word.to_string()];
                    line.extend(tail());
                    migrate::run(&line)
                } else {
                    eprintln!("letibot: no such role: {word}");
                    eprintln!("         run `letibot --help` for the roles and the verbs");
                    ExitCode::from(2)
                }
            }
        },
        None => roles::launcher(),
    }
}

/// One line per command, and every line's description at one column. Written as a raw
/// string because the `\` continuation dropped the indent from every wrapped
/// line, and a usage whose columns wander is one nobody finishes reading.
fn print_usage() {
    println!(
        "\
letibot {} — one binary, several roles

USAGE
  letibot                     start or attach to this folder's session
  letibot <role> [args…]      run one role
  letibot <verb> [args…]      read metrics, mark a turn, move a session, seat a reviewer

ROLES
  daemon                      the session owner (also installed as `harnessd`)
  tui | head                  a head, attached to a daemon
  askpass [PROMPT]            what `sudo` runs (also installed as `letibot-askpass`)
  m1                          the M1 measurement
  render | render-qwen        the renderers the fidelity gate runs

VERBS
  stats [VERB]                read metrics.db: today, waits, deliveries, tools, models,
                              turns, feedback, session ID, sql
  boundary [--mode NAME]      what a command gets: paths, network, env, hidden
  notes path|push             the notes checkout, and the one step that pushes it
  browser [VERB]              the Chrome extension's bridge on 127.0.0.1 (docs/browser.md):
                              install, start, stop, restart, off, on, status, logs
  rooms [VERB]                the box's room coordinator (docs/room.md): bare runs it in the
                              foreground; the same verbs as browser
  fb --session S [+|-] NOTE   mark a session's last turn from a shell
  migrate SESSION --to HOST   move a session's whole daemon to a host, or back with
                              `--to local` (`letibot --migrate` is the same verb)
  attach github <scope>       the reviewer seat on an org or `owner/repo`: one session per
                              open pull request, one review per push, drafts until
                              `[github] post` and a token are set

Every verb answers for itself: `letibot <verb> --help` says what it takes, which
flags have defaults, and one example. A role is reachable by its own name through a
symlink, which is how anything that invokes a PATH rather than typing a command
reaches it — `sudo` runs the askpass helper with no arguments but the prompt.
",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role is reachable BOTH ways, which is the ruling: argv[0] for the machine
    /// and a word for the person. A role that can only be reached one way is a role
    /// that is half-wired, and this is the test that would have said so.
    #[test]
    fn every_role_is_reachable_by_name_and_by_word() {
        for role in ALL_ROLES.iter().copied() {
            let names: Vec<(&str, Kind)> = role.names().collect();
            let programs: Vec<&str> = names
                .iter()
                .filter(|(_, k)| *k == Kind::Program)
                .map(|(n, _)| *n)
                .collect();
            let words: Vec<&str> = names
                .iter()
                .filter(|(_, k)| *k == Kind::Word)
                .map(|(n, _)| *n)
                .collect();
            assert_eq!(
                programs.len(),
                1,
                "{role:?} must have exactly one program name"
            );
            assert!(!words.is_empty(), "{role:?} must be typable");
            // And each name round-trips through the lookup that will be used for it.
            assert_eq!(
                Role::from_argv0(OsStr::new(programs[0])),
                Some(role),
                "{} does not round-trip through from_argv0",
                programs[0]
            );
            for w in words {
                assert_eq!(
                    Role::from_subcommand(w),
                    Some(role),
                    "{w} does not round-trip through from_subcommand"
                );
            }
        }
    }

    /// `letibot` is the dispatcher and never a role. Resolving it by substring is
    /// how a second daemon gets started, so this is asserted rather than assumed.
    #[test]
    fn the_dispatchers_own_name_is_not_a_role_in_either_table() {
        for name in ["letibot", "Letibot", "letibot ", ""] {
            assert_eq!(Role::from_argv0(OsStr::new(name)), None, "argv0 {name:?}");
            assert_eq!(Role::from_subcommand(name), None, "word {name:?}");
        }
    }

    /// The near-misses, so a future edit cannot make one resolve by accident.
    #[test]
    fn names_that_are_almost_right_do_not_resolve() {
        for wrong in [
            "letibot-askpas",
            "letibot-TUI",
            "harness",
            "Harnessd",
            "letibot-tui2",
            "tui2",
            "daemon2",
            "render-quen",
        ] {
            assert_eq!(Role::from_argv0(OsStr::new(wrong)), None, "argv0 {wrong:?}");
            assert_eq!(Role::from_subcommand(wrong), None, "word {wrong:?}");
        }
        // The path name is not a word, and a word is not a path name.
        assert_eq!(Role::from_subcommand("harnessd"), None);
        assert_eq!(Role::from_subcommand("letibot-tui"), None);
        assert_eq!(Role::from_argv0(OsStr::new("daemon")), None);
        assert_eq!(Role::from_argv0(OsStr::new("tui")), None);
    }

    /// The table itself, checked as data - the const assertion makes a duplicate
    /// impossible to BUILD, and this makes the failure readable if somebody removes
    /// the assertion.
    #[test]
    fn the_registry_holds_no_duplicate_name_and_every_role_has_a_program_name() {
        let mut seen: Vec<&str> = Vec::new();
        for (name, _, _) in REGISTRY {
            assert!(!seen.contains(name), "{name} appears twice in the registry");
            seen.push(name);
        }
        for role in ALL_ROLES.iter().copied() {
            let programs = REGISTRY
                .iter()
                .filter(|(_, r, k)| *r == role && *k == Kind::Program)
                .count();
            assert_eq!(programs, 1, "{role:?} needs exactly one Program name");
        }
        assert_eq!(
            REGISTRY.len(),
            seen.len(),
            "the registry has fewer entries than names, which means a name was dropped"
        );
    }
}
