//! One function per role, and an honest answer for the ones that are not here yet.
//!
//! # The state of the merge, said plainly, because a stub that lies is worse
//!
//! This binary is the *dispatcher*. `Askpass` is fully a role of it — that is the
//! role `sudo` reaches by path, so it is the one that had to work first and the one
//! that proves argv[0] dispatch against a real caller.
//!
//! The others still live where they have always lived, as their own binaries:
//! `harnessd` (1626 lines), `letibot-tui` (846), `letibot-m1` (743),
//! `letibot-render` (368) and `letibot-render-qwen` (309). Their logic is in the
//! `main`-carrying FILE, not in the crate's library, so making this binary *be* them
//! means moving each one's body into its crate's lib as a callable `pub fn` — a
//! mechanical but real extraction per role.
//!
//! Until that is done each of those roles prints where it actually is and exits 2.
//! **Deliberately not `exec` of the sibling binary**: that would leave the file count
//! exactly where it is now while looking finished, which is the shape of change that
//! never gets completed.
//!
//! # And `letibot` with no subcommand is the largest remaining piece
//!
//! That is the launcher role, and it is not a convenience wrapper: today it is
//! `scripts/letibot`, 901 tracked lines of shell with 262 more lines of drift on the
//! operator's machine that no repository has (`LETIBOT_HEAD`, the seats). It decides
//! which daemon a folder belongs to, whether one is already up, what a seat's flags
//! are, and it carries at least two incidents in its comments — a stop loop that had
//! to stop printing "stopped" before the process was gone, and a warning for flags
//! that a running daemon silently swallows. Porting it is not transcription; the
//! reasons have to travel with it, into test names.

use std::process::ExitCode;

use crate::Role;

/// Run one role with the arguments that followed it.
///
/// `args` never contains the role's own name: `letibot-askpass PROMPT` arrives as
/// `["PROMPT"]` and `letibot askpass PROMPT` arrives the same way, so a role cannot
/// tell — and must not need to — which of the two ways it was reached.
pub fn run(role: Role, args: &[std::ffi::OsString]) -> ExitCode {
    match role {
        Role::Askpass => askpass(args),
        Role::Daemon => not_yet(role, "crates/harnessd/src/bin/harnessd.rs"),
        Role::Head => not_yet(role, "crates/tui/src/bin/letibot-tui.rs"),
        Role::M1 => not_yet(role, "crates/harnessd/src/bin/letibot-m1.rs"),
        // **Wired.** The renderer's body lives in its crate's library now
        // (`letibot_dialect_glm::cli::run`), so this role and the `letibot-render`
        // binary are ONE implementation rather than two copies. That matters
        // unusually much here: the fidelity gate execs this by PATH and compares the
        // BYTES it produces, so a second copy that drifted by one space would
        // re-prefill every conversation and the gate would report a divergence that
        // was really a fork.
        //
        // `args` passes through unchanged — the dispatcher does not inspect them —
        // because the gate's invocation (`--dialect … --profile faithful FIXTURE…`)
        // must arrive exactly as it does when the binary is called directly.
        Role::Render => exit_code(letibot_dialect_glm::cli::run(&args_to_strings(args))),
        Role::RenderQwen => not_yet(role, "crates/dialect-qwen/src/bin/letibot-render-qwen.rs"),
    }
}

/// The argv a role receives, as `String`.
///
/// Lossy, and named so: these are paths and flags. A role that needed a non-UTF-8
/// argument would have to say so, and none does — the renderer reads FIXTURE paths,
/// the daemon reads flags. One function rather than a repeated `map`, so what happens
/// to a non-UTF-8 byte is decided once.
fn args_to_strings(args: &[std::ffi::OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// An exit code as `ExitCode`, for a role whose own `run` returns one.
///
/// Clamped rather than cast: a role returning something outside 0..=255 would wrap to
/// an unrelated code, and a wrong exit code is worse than a conservative one — see the
/// askpass role, where `sudo` reads the code to decide whether a password was given.
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

/// `letibot` with nothing after it.
pub fn launcher() -> ExitCode {
    eprintln!(
        "letibot: the launcher is not a role of this binary yet.\n\
         \n\
         It is still `scripts/letibot` — which decides which daemon this folder\n\
         belongs to, starts one if none is up, and attaches a head to it. Run that\n\
         for now, or name a role directly:\n\
         \n\
           letibot tui        attach a head to this folder's daemon\n\
           letibot --help     what this binary can be\n"
    );
    ExitCode::from(2)
}

/// A role that is still its own binary, said in one place so the wording cannot drift.
fn not_yet(role: Role, source: &str) -> ExitCode {
    eprintln!(
        "letibot: `{}` is not a role of this binary yet — it still builds and runs on\n\
         its own, from {source}.\n\
         \n\
         Wiring it here means moving that file's `main` body into its crate's library\n\
         as a callable `pub fn`, then calling it from this dispatcher. Until then the\n\
         installed name for this role is the executable, not this one.",
        role.program()
    );
    ExitCode::from(2)
}

/// **The role `sudo` runs.** Attach to the session's daemon, ask the person, print the
/// secret on stdout.
///
/// Ported verbatim from `crates/harnessd/src/bin/letibot-askpass.rs`, whose module
/// docs carry the full account of the protocol and of the forty attempts on this box
/// that died on a prompt with no terminal to read. Two things about the *shape* here
/// are the reason this was the first role ported:
///
///   * it takes the prompt as `args[0]`, and `sudo` passes it as `argv[1]` — so
///     `letibot askpass` could not have served `SUDO_ASKPASS`, which is the whole
///     argument for argv[0] dispatch existing;
///   * it is the smallest role and the only one a *machine* invokes, so it exercises
///     the dispatch end to end without a daemon, a model or a session.
///
/// Exit codes are load-bearing: `sudo` reads stdout as the password and reports "no
/// password was given" on a non-zero exit, so every refusal below is 1 and every
/// refusal says why on stderr.
fn askpass(args: &[std::ffi::OsString]) -> ExitCode {
    use std::io::Write;

    use letibot_sessionlog::client::HeadClient;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};

    let prompt = args
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "[sudo] password:".into());
    let socket = std::env::var("LETIBOT_SOCKET").unwrap_or_default();
    let session = std::env::var("LETIBOT_SESSION").unwrap_or_default();
    let command = std::env::var("LETIBOT_COMMAND").unwrap_or_default();
    if socket.is_empty() || session.is_empty() {
        eprintln!(
            "letibot-askpass: not inside a letibot session (LETIBOT_SOCKET / LETIBOT_SESSION \
             unset), so there is nobody to ask for a password"
        );
        return ExitCode::FAILURE;
    }
    let caps = Caps {
        queue: 8,
        can_decide: false,
        ..Caps::default()
    };
    let (mut client, _hello, mut reader) =
        match HeadClient::attach(&socket, &session, u64::MAX, "askpass", "sudo", caps) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("letibot-askpass: the session's daemon did not take the question: {e}");
                return ExitCode::FAILURE;
            }
        };
    if let Err(e) = client.askpass(&prompt, &command) {
        eprintln!("letibot-askpass: {e}");
        return ExitCode::FAILURE;
    }
    // The daemon may deliver a snapshot or events before the answer; only the
    // `Secret` frame is for us, and it comes on this connection or not at all.
    loop {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Secret { secret: Some(s) }) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(s.as_bytes());
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                let _ = client.detach();
                return ExitCode::SUCCESS;
            }
            Ok(ServerFrame::Secret { secret: None }) => {
                eprintln!(
                    "letibot-askpass: no password was given — no head answered before the \
                     deadline, or the person refused"
                );
                let _ = client.detach();
                return ExitCode::FAILURE;
            }
            Ok(ServerFrame::Bye { reason }) => {
                eprintln!("letibot-askpass: the daemon closed the connection: {reason}");
                return ExitCode::FAILURE;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("letibot-askpass: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role as R;

    /// **The mapping is the interface, and this is the test that says so.**
    ///
    /// Each name is what `install.sh` symlinks and what something else invokes by
    /// path. `letibot` itself must NOT map to a role: it is the dispatcher, and
    /// treating it as the daemon because the string contains "letibot" is the kind of
    /// guess that starts a second daemon.
    #[test]
    fn every_installed_name_maps_to_its_role_and_the_dispatcher_does_not() {
        for (name, role) in [
            ("letibot-askpass", R::Askpass),
            ("harnessd", R::Daemon),
            ("letibot-tui", R::Head),
            ("letibot-m1", R::M1),
            ("letibot-render", R::Render),
            ("letibot-render-qwen", R::RenderQwen),
        ] {
            assert_eq!(
                Role::from_argv0(std::ffi::OsStr::new(name)),
                Some(role),
                "`{name}` must be the {role:?} role — a symlink by this name is how \
                 something that knows a PATH reaches that role"
            );
        }
        assert_eq!(
            Role::from_argv0(std::ffi::OsStr::new("letibot")),
            None,
            "`letibot` is the dispatcher, never a role"
        );
        // The near-misses, so a future edit cannot make these resolve by accident.
        for wrong in ["letibot-askpas", "harness", "Harnessd", "letibot-tui2", ""] {
            assert_eq!(
                Role::from_argv0(std::ffi::OsStr::new(wrong)),
                None,
                "`{wrong}` is not a role"
            );
        }
    }

    /// The typed words, including both spellings of the head.
    #[test]
    fn subcommands_name_the_same_roles_as_the_installed_names() {
        assert_eq!(Role::from_subcommand("askpass"), Some(R::Askpass));
        assert_eq!(Role::from_subcommand("daemon"), Some(R::Daemon));
        assert_eq!(Role::from_subcommand("tui"), Some(R::Head));
        assert_eq!(Role::from_subcommand("head"), Some(R::Head));
        assert_eq!(Role::from_subcommand("m1"), Some(R::M1));
        assert_eq!(Role::from_subcommand("render"), Some(R::Render));
        assert_eq!(Role::from_subcommand("render-qwen"), Some(R::RenderQwen));
        assert_eq!(
            Role::from_subcommand("harnessd"),
            None,
            "the path name is not a word"
        );
        assert_eq!(Role::from_subcommand(""), None);
    }

    /// **`sudo`'s calling convention, which is why argv[0] exists.**
    ///
    /// `SUDO_ASKPASS` is a path and sudo invokes it with the prompt as `argv[1]`, so
    /// the role must take its prompt from the FIRST argument after the name and must
    /// not try to read a subcommand out of it. Outside a session the honest answer is
    /// a refusal, not a hang — this is the case that used to hang for the full
    /// two-minute deadline when a prompt arrived with no session behind it.
    #[test]
    fn askpass_outside_a_session_refuses_rather_than_waiting() {
        // Both must be unset: with either set the role tries to reach a daemon.
        let saved = (
            std::env::var_os("LETIBOT_SOCKET"),
            std::env::var_os("LETIBOT_SESSION"),
        );
        // SAFETY: single-threaded test, and both are restored below.
        unsafe {
            std::env::remove_var("LETIBOT_SOCKET");
            std::env::remove_var("LETIBOT_SESSION");
        }
        let code = askpass(&[std::ffi::OsString::from("[sudo] password for dead: ")]);
        unsafe {
            if let Some(v) = saved.0 {
                std::env::set_var("LETIBOT_SOCKET", v);
            }
            if let Some(v) = saved.1 {
                std::env::set_var("LETIBOT_SESSION", v);
            }
        }
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "no session means exit 1 so sudo reports that no password was given"
        );
    }

    /// The un-ported roles must refuse loudly. Exit 2, never 0: a role that reports
    /// success while doing nothing is the failure mode this file exists to avoid.
    #[test]
    fn the_unported_roles_refuse_loudly_and_say_where_they_live() {
        for role in [R::Daemon, R::Head, R::M1, R::Render, R::RenderQwen] {
            let code = run(role, &[]);
            assert_eq!(
                code,
                ExitCode::from(2),
                "{role:?} is not ported yet and must not report success"
            );
        }
    }
}
