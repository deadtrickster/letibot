//! **The bash an operator's own `!` line is handed to, chosen from what this box has.**
//!
//! # The defect, measured
//!
//! [`crate::exec::console::shell`] is `/bin/bash -ic`, and `-i` is the whole of it: bash
//! sources `~/.bashrc` for an interactive shell and for no other, which is what makes the
//! operator's aliases exist on a `!` line. On a stock Mac `/bin/bash` is **3.2.57 (2007)**,
//! while the rc chain an operator actually has is written for bash 4 or later. MEASURED
//! 2026-10-29 on the operator's Mac: the row for `! cp …` came back as
//!
//! ```text
//! bash: declare: -A: invalid option
//! declare: usage: declare [-afFirtx] [-p] [name[=value] ...]
//! ```
//!
//! and that is `~/.emacs.d/shell/integration.bash`'s `declare -A`, which `~/.bashrc` sources,
//! refused by 3.2 before the operator's own command. The same line under the `bash` on `PATH`
//! — 5.3.20 on that box — loads the rc cleanly.
//!
//! # The program is probed; the flags are not
//!
//! `-ic` is [`crate::exec::console`]'s decision and stays there. What this module answers is
//! *which bash*, and the candidates are ordered by what the person would get if they typed
//! it themselves:
//!
//! | candidate | taken when |
//! |---|---|
//! | `$SHELL` | it is a bash whose major version is 4 or more — the console that started the daemon is the operator's own |
//! | `bash` on `PATH` | its major version is 4 or more |
//! | `/bin/bash` | always, as the last word |
//!
//! **`$SHELL` is not followed when it is not a bash 4 or later**, and this box is why:
//! `$SHELL` is `/bin/zsh` here. The line the operator typed is a bash line — its word
//! splitting, its `~`, its `!` — and `-ic` is bash's and zsh's but not fish's, so following
//! `$SHELL` wherever it points would run a different command than the one that was written,
//! and on a console whose shell is fish would not run at all. A bash 4+ is the smallest
//! change that makes the operator's rc work, and it is what `bash` on their `PATH` already is.
//!
//! **The last word is a bash that may be 3.2, and that is deliberate.** A 3.2 that prints its
//! rc's error still runs `cp`: refusing to start, or refusing the line, would turn a noisy row
//! into a shell escape that cannot run anything, which is worse than the row.
//!
//! # Where it is used, and where it is not
//!
//! [`crate::backend::HostBackend::executable`] — the unconfined exec path, which is the
//! leticode seat and the Mac's boundary-less fallback — sets it. **A confined backend keeps
//! `/bin/bash`**, and the two reasons are one fact: the view's `$HOME` is a tmpfs, so
//! `~/.bashrc` inside it is not the operator's and there is no rc to fail — and a bash found
//! here lives at a path outside that view, which a command inside the boundary cannot exec.

use std::path::Path;
use std::sync::OnceLock;

/// **The major version the operator's rc chain is written in** — `declare -A`, `mapfile` and
/// `${x,,}` are all bash 4.0's.
const FIRST_BASH_THAT_READS_THEIR_RC: u32 = 4;

/// What the last word is when this box has no bash 4 anywhere.
const FALLBACK: &str = "/bin/bash";

/// **The console's shell, with the program chosen for this box.** The flags are
/// [`crate::exec::console::shell`]'s and are not touched: `-ic` is that module's decision.
pub fn shell() -> Vec<String> {
    let mut argv = crate::exec::console::shell();
    match argv.first() {
        Some(program) if is_bash(program) => {}
        // **The flags are this module's input, not its subject.** If `exec::console` ever
        // names a program that is not a bash, that decision wins and nothing here
        // second-guesses it.
        _ => return argv,
    }
    argv[0] = chosen();
    argv
}

/// The program, resolved once per process: the probe is two `fork`s, and a daemon opens many
/// sessions.
fn chosen() -> String {
    static CHOSEN: OnceLock<String> = OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            let console = std::env::var("SHELL").ok();
            let path_bash = on_path("bash");
            choose(console.as_deref(), path_bash.as_deref(), &major_version)
        })
        .clone()
}

/// **The rule, with the box injected** — the whole of it, assertable without a process.
///
/// `major` answers a program's bash major version, and `None` for anything that is not a bash
/// this box can run (the probe is `-c` and non-interactive, so no rc file is read and nothing
/// of the operator's runs).
fn choose(
    console_shell: Option<&str>,
    path_bash: Option<&str>,
    major: &dyn Fn(&str) -> Option<u32>,
) -> String {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(shell) = console_shell.filter(|program| is_bash(program)) {
        candidates.push(shell);
    }
    if let Some(found) = path_bash.filter(|program| is_bash(program)) {
        candidates.push(found);
    }
    for candidate in candidates {
        if major(candidate).is_some_and(|v| v >= FIRST_BASH_THAT_READS_THEIR_RC) {
            return candidate.to_string();
        }
    }
    FALLBACK.to_string()
}

/// A program this module is willing to call a bash: the file's own name, because `$SHELL` and
/// a `PATH` entry can both be a symlink or an absolute path and neither changes what runs.
fn is_bash(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("bash")
}

/// **`bash` on `PATH`, as an absolute path.** Absolute because the program is handed to a
/// process this daemon starts, and a bare name would be resolved against whatever `PATH` that
/// process ends up with rather than the one this choice was measured against.
fn on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// **The major version a program answers with**, or `None` when it is not a bash at all — the
/// probe prints `${BASH_VERSINFO[0]}`, which a dash refuses as a bad substitution and a zsh
/// leaves empty.
pub(crate) fn major_version(program: &str) -> Option<u32> {
    let out = std::process::Command::new(program)
        .arg("-c")
        .arg("printf '%s' \"${BASH_VERSINFO[0]}\"")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `major` that answers from a table, so a test says what the box has without being it.
    fn versions<'a>(pairs: &'a [(&'a str, u32)]) -> impl Fn(&str) -> Option<u32> + 'a {
        move |program| {
            pairs
                .iter()
                .find(|(name, _)| *name == program)
                .map(|(_, major)| *major)
        }
    }

    /// **`$SHELL` wins when it is a bash 4 or later**: the console that started the daemon is
    /// the operator's own, so its shell is the one their rc belongs to.
    #[test]
    fn a_console_shell_that_is_a_bash_four_is_taken() {
        let major = versions(&[("/opt/homebrew/bin/bash", 5), ("/usr/bin/bash", 5)]);
        assert_eq!(
            choose(
                Some("/opt/homebrew/bin/bash"),
                Some("/usr/bin/bash"),
                &major
            ),
            "/opt/homebrew/bin/bash",
            "a console bash 4+ is the operator's own shell and comes first"
        );
    }

    /// **A `$SHELL` that is not a bash is not followed.** This box's is `/bin/zsh`; the line
    /// the operator typed is a bash line, and `-ic` is bash's and zsh's but not fish's.
    #[test]
    fn a_console_shell_that_is_not_a_bash_is_left_alone() {
        let major = versions(&[("/usr/bin/bash", 5)]);
        for theirs in ["/bin/zsh", "/usr/local/bin/fish", "/bin/sh"] {
            assert_eq!(
                choose(Some(theirs), Some("/usr/bin/bash"), &major),
                "/usr/bin/bash",
                "`{theirs}` is not a bash, so the bash on PATH is the answer"
            );
        }
    }

    /// **The measured defect**: a console bash of 3.2 is skipped for a 4+ on `PATH` — that is
    /// what turned `! cp …` into two lines of `declare` usage.
    #[test]
    fn a_console_bash_three_is_skipped_for_a_four_on_the_path() {
        let major = versions(&[("/bin/bash", 3), ("/opt/homebrew/bin/bash", 5)]);
        assert_eq!(
            choose(Some("/bin/bash"), Some("/opt/homebrew/bin/bash"), &major),
            "/opt/homebrew/bin/bash",
            "3.2 cannot read the operator's rc, and the row it prints is not their command"
        );
    }

    /// **And when nothing on the box is a 4+, the last word is `/bin/bash`.** A 3.2 that
    /// prints its rc's error still runs `cp`, and a shell escape that refuses the line is
    /// worse than a noisy row.
    #[test]
    fn nothing_newer_anywhere_still_answers_the_bash_the_box_has() {
        let major = versions(&[("/bin/bash", 3), ("/usr/bin/bash", 3)]);
        assert_eq!(
            choose(Some("/bin/bash"), Some("/usr/bin/bash"), &major),
            FALLBACK
        );
        assert_eq!(
            choose(None, None, &major),
            FALLBACK,
            "a box with no bash on PATH at all still answers a shell"
        );
        assert_eq!(
            choose(Some("/bin/zsh"), Some("/usr/bin/bash"), &major),
            FALLBACK,
            "a non-bash `$SHELL` is never the answer, whatever version anything has"
        );
    }

    /// **The wiring, on whatever box this runs on**: the flags are `exec::console`'s, only the
    /// program is chosen, and the program is a bash that exists here.
    #[test]
    fn the_flags_are_the_consoles_and_only_the_program_is_chosen() {
        let console = crate::exec::console::shell();
        let mine = shell();
        assert_eq!(mine.len(), console.len(), "{mine:?} against {console:?}");
        assert_eq!(mine[1..], console[1..], "the flags are `exec::console`'s");
        assert!(is_bash(&mine[0]), "the program stays a bash: {mine:?}");
        assert!(
            Path::new(&mine[0]).is_file(),
            "the chosen program must exist on this box: {mine:?}"
        );
        // Printed so a green run says what it measured: which bash, and which flags.
        eprintln!("the operator's own run is handed to {mine:?}");
    }

    /// **No 3.2 is chosen while a 4+ is reachable on this box.** The measured defect, as an
    /// assertion about the machine the test is running on rather than about a table.
    #[test]
    fn a_four_this_box_has_is_never_passed_over_for_a_three() {
        let program = shell()[0].clone();
        let four_reachable = on_path("bash")
            .into_iter()
            .chain(
                std::env::var("SHELL")
                    .ok()
                    .filter(|program| is_bash(program)),
            )
            .filter_map(|program| major_version(&program))
            .any(|major| major >= FIRST_BASH_THAT_READS_THEIR_RC);
        if !four_reachable {
            eprintln!(
                "this box has no bash 4 or later anywhere, so `{program}` is the fallback and \
                 the operator's rc is theirs to fix — THIS IS NOT A PASS of the 4+ half"
            );
            assert_eq!(program, FALLBACK);
            return;
        }
        assert!(
            major_version(&program).is_some_and(|v| v >= FIRST_BASH_THAT_READS_THEIR_RC),
            "a bash 4+ is on this box and `{program}` is not it"
        );
    }
}
