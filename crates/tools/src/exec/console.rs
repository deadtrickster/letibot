//! What the operator's console gives a command: their interactive shell, so their rc
//! and its aliases apply, and the environment their terminal is described by.
//!
//! [`shell`] is `/bin/bash -ic` because only an interactive shell reads the rc; [`env`] forces every pager
//! to [`PAGER`] so nothing waits for a keystroke that cannot arrive.
//!
//! ```text
//! alias ls='ls --color=auto'
//! ```
//!
//! A model's `bash` call gets none of this: it has no terminal.

/// The console's shell, interactive. `-i` is the whole of it: bash reads `~/.bashrc`
/// for an interactive shell and no other, and a rc that guards itself returns
/// immediately without it, so the flag is what makes their rc run.
pub fn shell() -> Vec<String> {
    vec!["/bin/bash".to_string(), "-ic".to_string()]
}

/// The variables that say what kind of terminal the console is, inherited from the
/// daemon's environment where it has them and absent where it does not. `HOME` is
/// deliberately absent: a pair here is applied last and would beat the run's own.
pub const INHERITED: &[&str] = &["TERM", "COLORTERM", "LS_COLORS"];

/// What the console said about `name`, or `None` when it is absent or set to the empty
/// string: an empty value is not a value, and handing it on is worse than handing
/// nothing, because its reader believes it was told something.
pub fn value<'a>(source: &'a [(String, String)], name: &str) -> Option<&'a str> {
    source
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// The variables a program chooses a pager through, all forced to [`PAGER`]: a class
/// rather than a program, so a table rather than a special case for `git log`.
pub const PAGERS: &[&str] = &["PAGER", "GIT_PAGER", "SYSTEMD_PAGER"];

/// What a pager is when nobody can press a key.
pub const PAGER: &str = "cat";

/// The pairs the operator's own run carries, built from a source environment. Pure, so
/// a test can hold the whole of what the run is given without starting a process, and
/// in the tables' own order so a run is reproducible.
pub fn env_from(source: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs = Vec::with_capacity(INHERITED.len() + PAGERS.len());
    for name in INHERITED {
        if let Some(value) = value(source, name) {
            pairs.push(((*name).to_string(), value.to_string()));
        }
    }
    // Forced, not inherited, and that is the difference from the group above: a
    // console whose `PAGER` is `less` has a person at it, and this run has none.
    for name in PAGERS {
        pairs.push(((*name).to_string(), PAGER.to_string()));
    }
    pairs
}

/// [`env_from`] over this process's own environment, which is the console's.
pub fn env() -> Vec<(String, String)> {
    env_from(&std::env::vars().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, s)| (k.to_string(), s.to_string()))
            .collect()
    }

    /// The whole environment the run is built with, asserted without a process: every
    /// pair at once, so an edit that drops one or starts inheriting a pager fails here.
    /// `HOME` is in the source and not in the answer, which keeps this about the terminal.
    #[test]
    fn the_operators_run_is_given_the_consoles_terminal_and_pagers_that_cannot_page() {
        let console = pairs(&[
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("LS_COLORS", "di=01;34"),
            ("PATH", "/usr/bin"),
            ("HOME", "/home/dead"),
        ]);
        assert_eq!(
            env_from(&console),
            pairs(&[
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("LS_COLORS", "di=01;34"),
                ("PAGER", "cat"),
                ("GIT_PAGER", "cat"),
                ("SYSTEMD_PAGER", "cat"),
            ]),
            "the run gets the console's terminal variables and no others, plus the \
             three pager variables forced to `cat`"
        );
    }

    /// An empty value is not a value: a program reads `TERM=` as a terminal type of the
    /// empty string, fails to find its terminfo and exits at once. The control is in the
    /// same test, a non-empty `TERM` passed through, so this is about emptiness.
    #[test]
    fn a_console_variable_that_is_set_and_empty_is_not_a_value() {
        let hollow = pairs(&[("TERM", ""), ("HOME", ""), ("PATH", "/usr/bin")]);
        assert_eq!(
            env_from(&hollow),
            pairs(&[
                ("PAGER", "cat"),
                ("GIT_PAGER", "cat"),
                ("SYSTEMD_PAGER", "cat"),
            ]),
            "a variable set to the empty string says nothing, and must not be handed on \
             as though it had"
        );
        assert_eq!(value(&hollow, "TERM"), None);
        assert_eq!(value(&hollow, "PATH"), Some("/usr/bin"));
    }

    /// A variable the console does not have is not invented: a default would be a claim
    /// about a screen nobody has seen, where an absent `TERM` is a dumb terminal behaving
    /// like one.
    #[test]
    fn a_console_variable_the_daemon_does_not_have_is_absent_rather_than_defaulted() {
        let bare = pairs(&[("PATH", "/usr/bin")]);
        assert_eq!(
            env_from(&bare),
            pairs(&[
                ("PAGER", "cat"),
                ("GIT_PAGER", "cat"),
                ("SYSTEMD_PAGER", "cat"),
            ]),
            "no TERM, no COLORTERM, no LS_COLORS — and the pagers are still forced, \
             because they are not inherited at all"
        );
    }

    /// The pagers are forced, so a console's own `PAGER=less` does not survive: handing
    /// it to a run nobody can type into is the hang this table exists to close, and it
    /// is the one asymmetry between the two groups.
    #[test]
    fn a_pager_the_console_chose_does_not_reach_the_run() {
        let console = pairs(&[
            ("TERM", "xterm"),
            ("PAGER", "less"),
            ("GIT_PAGER", "less -FRX"),
            ("SYSTEMD_PAGER", "less"),
        ]);
        let got = env_from(&console);
        for name in PAGERS {
            assert_eq!(
                got.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()),
                Some("cat"),
                "`{name}` must be `cat` whatever the console said"
            );
        }
    }

    /// The shell is interactive, and that flag is the whole decision: without `-i` the
    /// operator's `~/.bashrc` returns at its first non-comment line and no alias is ever
    /// defined, so the assertion is on the flag rather than on bash.
    #[test]
    fn the_console_shell_is_interactive_because_that_is_what_reads_the_rc() {
        let shell = shell();
        assert_eq!(shell.first().map(String::as_str), Some("/bin/bash"));
        assert!(
            shell.iter().any(|a| a == "-ic"),
            "the rc is only read for an interactive shell: {shell:?}"
        );
    }
}
