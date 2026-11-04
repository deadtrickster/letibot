//! One function per role, and an honest answer for those not here yet.
//! This binary is the dispatcher: each role's body lives in its own crate and is called from here.
//! The launcher is not a role: `scripts/letibot` ships as `letibot`.

use std::process::ExitCode;

use crate::Role;

/// Run one role with the arguments that followed it. `args` never contains the role's own name,
/// so `letibot-askpass PROMPT` and `letibot askpass PROMPT` arrive identically and a role cannot
/// tell - and must not need to - which way it was reached.
pub fn run(role: Role, args: &[std::ffi::OsString]) -> ExitCode {
    match role {
        Role::Askpass => askpass(args),
        // Wired, and it is the last one. `letibot_harnessd::cli::run` is that binary's body,
        // and this arm mirrors ITS `main` exactly, including the `harnessd: {e}` prefix a person
        // reading a failed start has always seen.
        Role::Daemon => match letibot_harnessd::cli::run(&args_to_strings(args)) {
            Ok(code) => exit_code(code),
            Err(e) => {
                eprintln!("harnessd: {e}");
                ExitCode::FAILURE
            }
        },
        // Wired. `letibot_tui::head::run` is that binary's body, and the flags parse exactly as
        // they did: `scripts/letibot` execs the head with `--socket S --identity U`, and
        // leticl's Common Lisp head speaks that contract.
        Role::Head => exit_code(letibot_tui::head::run_with(
            &args_to_strings(args),
            Some(letibot_bridge::fleet::reader()),
        )),
        // Wired. `letibot_harnessd::m1::run` is that binary's body; a usage error
        // exits 2 from inside it rather than returning, which is named in the module
        // docs there and is why this arm cannot report a code for every path.
        Role::M1 => exit_code(letibot_harnessd::m1::run(&args_to_strings(args))),
        // Wired. The renderer's body lives in its crate's library, so this role and the
        // `letibot-render` binary are ONE implementation: the fidelity gate execs it by PATH and
        // compares the BYTES. `args` passes through unchanged, as the gate typed them.
        Role::Render => exit_code(letibot_dialect_glm::cli::run(&args_to_strings(args))),
        // Wired, same as `Render`: one implementation with the binary, and `args` passes through
        // unchanged so the gate's invocation arrives as it does when the binary is called.
        Role::RenderQwen => exit_code(letibot_dialect_qwen::cli::run(&args_to_strings(args))),
    }
}

/// The argv a role receives, as `String`. Lossy, and named so: these are paths and flags, and a
/// role that needed a non-UTF-8 argument would have to say so. One function rather than a
/// repeated `map`, so what happens to a non-UTF-8 byte is decided once.
fn args_to_strings(args: &[std::ffi::OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// An exit code as `ExitCode`, for a role whose own `run` returns one. Clamped rather than cast:
/// a code outside 0..=255 would wrap to an unrelated one, and `sudo` reads the askpass role's
/// code to decide whether a password was given.
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

// The launcher's EXECUTION is not ported: `scripts/letibot` is what ships as `letibot`, so a
// multicall launcher would have no caller. Its decisions ARE ported and tested.
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

/// The role `sudo` runs. Attach to the session's daemon, ask the person, print the secret on
/// stdout. It takes the prompt as `args[0]` and `sudo` passes it as `argv[1]`, which is the whole
/// argument for argv[0] dispatch. Exit codes are load-bearing: every refusal is 1 and says why.
fn askpass(args: &[std::ffi::OsString]) -> ExitCode {
    askpass_in(
        args,
        &std::env::var("LETIBOT_SOCKET").unwrap_or_default(),
        &std::env::var("LETIBOT_SESSION").unwrap_or_default(),
        &std::env::var("LETIBOT_COMMAND").unwrap_or_default(),
    )
}

/// [`askpass`] with the session handed over rather than read from the environment, so its test
/// says "outside a session" with two empty strings: a `setenv` beside a C-side `getenv` is
/// undefined behaviour, and `cargo test` is not single-threaded.
fn askpass_in(args: &[std::ffi::OsString], socket: &str, session: &str, command: &str) -> ExitCode {
    use std::io::Write;

    use letibot_sessionlog::client::HeadClient;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};

    let prompt = args
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "[sudo] password:".into());
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
        match HeadClient::attach(socket, session, u64::MAX, "askpass", "sudo", caps) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("letibot-askpass: the session's daemon did not take the question: {e}");
                return ExitCode::FAILURE;
            }
        };
    if let Err(e) = client.askpass(&prompt, command) {
        eprintln!("letibot-askpass: {e}");
        return ExitCode::FAILURE;
    }
    // The daemon may deliver a snapshot or events before the answer; only the
    // `Secret` frame is for us, and it comes on this connection or not at all.
    loop {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Secret {
                secret: Some(s), ..
            }) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(s.as_bytes());
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                let _ = client.detach();
                return ExitCode::SUCCESS;
            }
            Ok(ServerFrame::Secret { secret: None, why }) => {
                // The daemon says which failure this was - see the sibling helper in
                // `harnessd/src/bin/letibot-askpass.rs`; an older daemon sends no reason and
                // the old sentence is what is left to say.
                match why {
                    Some(why) => eprintln!("letibot-askpass: no password was given — {why}"),
                    None => eprintln!(
                        "letibot-askpass: no password was given — no head answered before the \
                         deadline, or the person refused"
                    ),
                }
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

    /// The mapping is the interface, and this is the test that says so. Each name is what
    /// `install.sh` symlinks and what something else invokes by path; `letibot` itself must NOT
    /// map to a role, because treating it as the daemon starts a second daemon.
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

    /// `sudo`'s calling convention, which is why argv[0] exists. `SUDO_ASKPASS` is a path and
    /// sudo passes the prompt as `argv[1]`, so the role takes it from the first argument and reads
    /// no subcommand out of it. Outside a session the honest answer is a refusal, not a hang.
    #[test]
    fn askpass_outside_a_session_refuses_rather_than_waiting() {
        // Both empty, which is what an unset `LETIBOT_SOCKET`/`LETIBOT_SESSION` reads as:
        // with either set the role tries to reach a daemon. Handed in, not unset - see
        // `askpass_in`.
        let code = askpass_in(
            &[std::ffi::OsString::from("[sudo] password for dead: ")],
            "",
            "",
            "",
        );
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "no session means exit 1 so sudo reports that no password was given"
        );
    }

    /// The session it asks in is the one HANDED IN rather than one read from the environment: a
    /// socket that names nobody is a refusal at once, which is what keeps a `sudo` prompt from
    /// hanging on a daemon that is not there.
    #[test]
    fn askpass_names_the_socket_it_was_given_and_refuses_when_no_daemon_is_at_it() {
        let nowhere = std::env::temp_dir().join(format!(
            "letibot-no-daemon-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let code = askpass_in(
            &[std::ffi::OsString::from("[sudo] password: ")],
            &nowhere.to_string_lossy(),
            "s-nobody",
            "",
        );
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "a socket with no daemon behind it is a refusal, not a wait"
        );
    }
}
