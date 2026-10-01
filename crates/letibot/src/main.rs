//! `letibot` — one executable, several roles.
//!
//! A person types `letibot`. `sudo` runs a program path with no arguments. The
//! daemon execs the render helpers by path. Those three callers cannot agree on an
//! interface, so this binary offers both and picks between them:
//!
//! ```text
//!   argv[0]                            invoked by        example
//!   ─────────────────────────────────  ────────────────  ──────────────────────────
//!   letibot-askpass                    sudo, by path     SUDO_ASKPASS=…/letibot-askpass
//!   harnessd                           systemd/operator  …/harnessd --workspace …
//!   letibot-tui                        a person          …/letibot-tui --socket S
//!   letibot-m1                         the rig           …/letibot-m1
//!   letibot-render / -render-qwen      the gate          …/letibot-render <path>
//!
//!   letibot                            a person          letibot askpass "pw:"
//!   letibot <role> [args…]                               letibot daemon --workspace .
//! ```
//!
//! # Why argv[0] and not only subcommands
//!
//! **`sudo` is handed a program path and invokes it with no arguments** — the prompt
//! is `argv[1]`, so `letibot askpass` cannot work there; there is nowhere to put the
//! word `askpass`. `crates/harnessd/src/sudo.rs` writes `SUDO_ASKPASS=…` pointing at a
//! file and looks for that file beside the daemon by name. So the role has to be
//! reachable from the NAME the file was installed under, which is what a symlink and
//! this dispatch give it. The same is true of anything else that is exec'd rather than
//! typed: it knows a path, not a CLI.
//!
//! Subcommands exist for the roles a PERSON starts, because `letibot daemon` reads
//! better than a symlink nobody can see. Both, not either.
//!
//! # Why one binary
//!
//! MEASURED before the merge: `harnessd` 33.2 MB, `letibot-tui` 22.7 MB,
//! `letibot-askpass` 2.5 MB — the three the installer ships, **58.4 MB**, and all
//! three link `sessionlog`, `transcript`, `tools` and `provider`. A single binary
//! statically links each of those once, so `askpass` stops costing a whole Rust
//! runtime to ask for a password. It is also one download, one checksum, one version.
//!
//! # The symlinks are the interface, so they are load-bearing
//!
//! A packaging bug that drops `letibot-askpass` does not produce a missing banner: it
//! produces `sudo` silently falling back to a terminal that is not there, which is the
//! failure `SUDO_USE.md` records at length. `install.sh` creates them and verifies
//! each one lands.

use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

mod roles;

/// What this invocation is being asked to be.
///
/// One variant per role, and **no catch-all**: an unknown name is an error rather
/// than a guess, because the guess would be "be the daemon" or "be the head" for a
/// typo, and both of those are long-running processes.
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

impl Role {
    /// The role a program NAME means, for the argv[0] path.
    ///
    /// Takes a file name, not a path: `Path::file_name` is the caller's job so this
    /// stays a pure string match and can be tested with literals. Returns `None` for
    /// `letibot` itself — that is not a role, it is the dispatcher — and for anything
    /// unrecognised.
    pub fn from_argv0(name: &OsStr) -> Option<Role> {
        match name.to_str()? {
            "letibot-askpass" => Some(Role::Askpass),
            // Kept under its historical name. It is the daemon's name in a socket
            // path, in `--status` output, and in the launcher's check that a pid is
            // the daemon and not something else, so renaming it is a coordinated
            // change rather than a tidy-up.
            "harnessd" => Some(Role::Daemon),
            "letibot-tui" => Some(Role::Head),
            "letibot-m1" => Some(Role::M1),
            "letibot-render" => Some(Role::Render),
            "letibot-render-qwen" => Some(Role::RenderQwen),
            _ => None,
        }
    }

    /// The role a typed WORD means, for the subcommand path.
    pub fn from_subcommand(word: &str) -> Option<Role> {
        match word {
            "askpass" => Some(Role::Askpass),
            "daemon" => Some(Role::Daemon),
            // `tui` and `head` are the same thing and both are worth accepting: the
            // crate is `crates/tui`, the role in the protocol is `head`, and a person
            // reading either will type the word they just read.
            "tui" | "head" => Some(Role::Head),
            "m1" => Some(Role::M1),
            "render" => Some(Role::Render),
            "render-qwen" => Some(Role::RenderQwen),
            _ => None,
        }
    }

    /// The name this role is installed under, for messages and for packaging.
    pub fn program(self) -> &'static str {
        match self {
            Role::Askpass => "letibot-askpass",
            Role::Daemon => "harnessd",
            Role::Head => "letibot-tui",
            Role::M1 => "letibot-m1",
            Role::Render => "letibot-render",
            Role::RenderQwen => "letibot-render-qwen",
        }
    }
}

fn main() -> ExitCode {
    let mut argv = std::env::args_os();
    let arg0 = argv.next().unwrap_or_default();
    let rest: Vec<std::ffi::OsString> = argv.collect();

    // **The name we were invoked by wins.** A symlink called `letibot-askpass` is
    // `sudo` talking, and its first argument is the prompt — reading it as a
    // subcommand would eat the prompt and then refuse an unknown role.
    if let Some(role) = Path::new(&arg0).file_name().and_then(Role::from_argv0) {
        return roles::run(role, &rest);
    }

    // Otherwise this is a person typing. `letibot <role> [args…]`, or bare `letibot`
    // for the launcher.
    match rest.first().and_then(|s| s.to_str()) {
        Some(word) => match Role::from_subcommand(word) {
            Some(role) => roles::run(role, &rest[1..]),
            None => {
                if word == "--help" || word == "-h" || word == "help" {
                    print_usage();
                    ExitCode::SUCCESS
                } else if word == "--version" || word == "-V" {
                    println!("letibot {}", env!("CARGO_PKG_VERSION"));
                    ExitCode::SUCCESS
                } else {
                    eprintln!("letibot: no such role: {word}");
                    eprintln!(
                        "         run `letibot --help` for the roles, or see {}",
                        arg0.to_string_lossy()
                    );
                    ExitCode::from(2)
                }
            }
        },
        None => roles::launcher(),
    }
}

fn print_usage() {
    println!(
        "letibot {} — one binary, several roles\n\n\
         USAGE\n  \
           letibot                     start or attach to this folder's session\n  \
           letibot <role> [args…]      run one role\n\n\
         ROLES\n  \
           daemon                      the session owner (also installed as `harnessd`)\n  \
           tui | head                  a head, attached to a daemon\n  \
           askpass [PROMPT]            what `sudo` runs (also installed as `letibot-askpass`)\n  \
           m1                          the M1 measurement\n  \
           render | render-qwen        the renderers the fidelity gate runs\n\n\
         A role is reachable by its own name through a symlink, which is how anything\n\
         that invokes a PATH rather than typing a command reaches it — `sudo` runs the\n\
         askpass helper with no arguments but the prompt.\n",
        env!("CARGO_PKG_VERSION")
    );
}
