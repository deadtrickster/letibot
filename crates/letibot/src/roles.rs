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
        // `sudo`, `ssh` and `git` run this one by PATH with their prompt as argv[1]. It is a
        // thin wrapper over the generic helper, so there is one place a secret is asked for.
        Role::Askpass => crate::secret::askpass(args),
        Role::Secret => crate::secret::run(&args_to_strings(args)),
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

// The launcher's TABLE is not in this crate: `scripts/letibot` is what ships as `letibot`, so a
// second copy of its flag list is a copy with no caller - and it had drifted.
/// `letibot-multi` with nothing after it.
pub fn launcher() -> ExitCode {
    eprintln!(
        "letibot-multi: the launcher is not a role of this binary yet.\n\
         \n\
         It is `letibot` (`scripts/letibot`), which decides which daemon this folder\n\
         belongs to, starts one if none is up, and attaches a head to it. Run that,\n\
         or name a role directly:\n\
         \n\
           letibot-multi tui        attach a head to this folder's daemon\n\
           letibot-multi --help     what this binary can be\n"
    );
    ExitCode::from(2)
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
            ("letibot-secret", R::Secret),
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
        assert_eq!(Role::from_subcommand("secret"), Some(R::Secret));
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
}
