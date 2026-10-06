//! **The world the operator's console gives a command** — their shell, with their
//! rc read, and the environment their terminal is described by.
//!
//! # The defect, measured
//!
//! *"the operator types `! ls -la` and folders are plain, while `! ls --color -la`
//! deposits `\u001b[01;34m` in the row."*
//!
//! Both halves are the same fact at two layers, and [`super::pty`] fixed only one
//! of them. The pty makes `isatty(1)` true, which is the question `--color=auto`
//! asks — and that is why `! ls --color -la` colours. But **`--color=auto` is not
//! `ls`'s default**: plain `ls` colourises on this box because the operator's
//! `~/.bashrc` says
//!
//! ```text
//! alias ls='ls --color=auto'
//! ```
//!
//! and an alias is **shell state, not program behaviour**. The daemon hands the
//! line to `/bin/sh -c` — non-interactive and non-login, so no rc file is read,
//! which is `TODO.md` R10's layer 1 and its own reason for the gap being small
//! (*"the gap is small because `exec/host.rs` already spawns `/bin/sh -c`,
//! non-interactive and non-login, so no rc file is read"*). So the line the
//! operator types and the line the daemon runs are **not the same command**, and
//! the difference is a file the daemon never opened.
//!
//! # The decision
//!
//! **The operator's own run is handed to their shell, interactive, so their rc
//! applies.** [`shell`] is `/bin/bash -ic`; [`env`] puts their terminal's own
//! variables and a pager that cannot page in front of it. Their line is sourced
//! and run as their code — inside the daemon's run, on their box, for a command
//! they typed and the admission has already answered
//! ([`crate::runtime::ToolRuntime::invoke_operator`], which consults no gate).
//!
//! # What it changes, said rather than discovered
//!
//! - **Their rc runs, with all of its effects.** Not only aliases: functions,
//!   `shopt`, `LS_COLORS` through `dircolors`, and every variable it exports. `! ls`
//!   colours because their shell says so, `! ll` works because their alias exists,
//!   and a rc that errors prints its error into the row, where it is theirs to see.
//! - **A rc that sets its own `PAGER` wins.** [`env`] is set before the shell
//!   starts and the rc is read after, so the last word is theirs — which is right,
//!   and is the one case where the pager fix below does not hold.
//! - **The line is read through their aliases**, and that is a departure from R10's
//!   pinning **on this path only**. `crates/code/src/shell.rs` names this same
//!   mechanism as a defeat: a surveyed harness replays `alias -p` and `eval`s the
//!   model's command, so `alias ls='rm -rf ~'` turns an always-safe entry into an
//!   `rm`. That defeat is a **gate** reading text the shell will re-read — and
//!   there is no gate here. `invoke_operator` is the ungated entry, the admission
//!   answered before it was called, and so the operator's aliases are the point
//!   rather than a defeat. The model's call keeps `/bin/sh -c` and the pinning,
//!   which is the half a gate reads.
//! - **In a confined session the rc is not theirs.** The view's `$HOME` is a
//!   tmpfs (`super::confine::HomeView`), so `~/.bashrc` inside it does not exist.
//!   The shell is still interactive and the environment is still the console's;
//!   the aliases are the part the boundary hides. Said here because the
//!   alternative is an operator wondering why `! ll` works in one session and not
//!   in another.
//! - **A terminal with no controlling terminal.** The run's terminal is a capture:
//!   stdin is `/dev/null` and the pty is not the child's controlling terminal
//!   ([`super::pty`] says why). An interactive bash in that position prints two
//!   lines of its own about job control before it reads any rc file, so they land on
//!   the row of every operator command — measured, through the host: `! echo hi`
//!   comes back as those two lines and then `hi`. They are bash's and they are true.
//!   Making the pty the child's controlling terminal silences them and was rejected
//!   on [`super::pty`]'s own ruling: it makes `/dev/tty` openable, and a program that
//!   wants a person at the keyboard — reached indirectly, as git's editor and `gpg`'s
//!   pinentry are — would wait for a keystroke that cannot arrive.
//! - **TODO: the console's shell is assumed to be bash.** `$SHELL` is where a
//!   console would say otherwise, and reading it needs a decision about flags
//!   (`-ic` is bash's and zsh's, not fish's) and about a daemon whose environment
//!   outlived the console that started it. A console whose shell is zsh keeps its
//!   aliases in `~/.zshrc`, and this run would not see them.
//!
//! # The environment, and the class the pagers are
//!
//! Two groups, different in kind:
//!
//! | group | pairs | where from |
//! |---|---|---|
//! | the terminal the console is | [`INHERITED`] — `TERM`, `COLORTERM`, `LS_COLORS` | **inherited** from the daemon's own environment, and only where it has them |
//! | the pagers that must not page | [`PAGERS`] — `PAGER`, `GIT_PAGER`, `SYSTEMD_PAGER` | **forced** to `cat` |
//!
//! The daemon is started from the operator's console — `scripts/letibot` execs
//! `harnessd` with its own environment — so its environment *is* the console's.
//! That is already where `PATH` is taken from at construction (`host`'s
//! `pinned_path`), and this is the same reading for the same reason. **A variable
//! the daemon does not have is not invented**: a run whose console never said
//! `TERM` gets no `TERM`, which is a dumb terminal behaving like one rather than a
//! claim about a screen nobody measured.
//!
//! **The pagers are a class, not a program.** The sibling defect: with the pty in
//! place `git log` sees a terminal, execs `less`, and `less` waits for keystrokes
//! that cannot arrive — the run hangs until its deadline. `git` is not special and
//! neither is `less`: **every program that hands its output to a pager chooses it
//! through a variable**, and the variables are a short published set — `PAGER` is
//! the one a program consults when it has no preference of its own, and git and the
//! systemd tools each ship their own. `cat` is the answer to *what is a pager when
//! nobody can press a key*: it prints and exits. [`PAGERS`] is where a fourth goes.
//! A fix for `git log` alone would be a fix for one member of a class whose next
//! member is `systemctl status`.
//!
//! # Why a model's `bash` call gets none of this
//!
//! Because it has no terminal, and every pair here is about a terminal.
//! [`crate::exec::host::SpawnRequest::tty`] is `!gated` and nothing else, so a
//! model's call keeps its pipe: `git log` on a pipe does not page at all (git's
//! `pager.c` asks `isatty(1)`), so `PAGER=cat` there would answer a question
//! nobody asked — and its environment is R10 layer 1, cleared with `PATH` pinned
//! and `BASH_ENV` dropped, so a variable the console carries is exactly what a
//! model's command must not be handed. The same flag decides both, and
//! [`crate::exec::host`] carries the argument for why one flag is right where
//! three would be three answers.

/// **The console's shell, interactive.**
///
/// `-i` is the whole of it: bash sources `~/.bashrc` for an interactive shell and
/// for no other, and a rc that guards itself (`case $- in *i*) ;; *) return;; esac`
/// — which is the first non-comment line of the operator's — returns immediately
/// without it. So the flag is not a preference; it is what makes their rc a file
/// that runs.
pub fn shell() -> Vec<String> {
    vec!["/bin/bash".to_string(), "-ic".to_string()]
}

/// **The variables that say what kind of terminal the console is**, read from the
/// daemon's own environment and given to the operator's run.
///
/// The table is the class: what is here is inherited when the daemon has it and
/// absent when it does not, and a fourth console variable goes here. `LS_COLORS`
/// says *which* colours and never *whether* — the pty is the *whether* — but a
/// console that has chosen a palette should get it back.
///
/// **`HOME` is deliberately NOT on this table, and it was tried.** A run's `HOME` is a
/// decision about *that run* and not about the console: the operator's own run is handed
/// one on the daemon's standing environment (`harness.rs`, set for the askpass shim's
/// sake), and a confined run is handed the **view's** tmpfs home by `super::confine`. A
/// pair on the request's own environment is applied **last** — `confine::plan` says so in
/// as many words — so an inherited `HOME` here would beat both: `crates/tools/tests/exec.rs`
/// caught the first (the rc the run reads stopped being the one the test wrote) and the
/// second is the same mechanism with a boundary on it, which would put the operator's real
/// home inside a view. **A pane has neither a view nor a standing environment** — it is the
/// operator's program on the operator's own box — so it is the one path that names the
/// console's home for itself: see `super::term::env_from`.
pub const INHERITED: &[&str] = &["TERM", "COLORTERM", "LS_COLORS"];

/// **What the console said about `name`** — `None` when it is not there, **and `None`
/// when it is there and empty**.
///
/// *An empty value is not a value* is one rule rather than a special case for `TERM`:
/// a variable set to the empty string says nothing at all, and handing it on is worse
/// than handing nothing, because the reader of it believes it was told something. The
/// case that measured this is a daemon whose environment carries `TERM=` — a launcher
/// that ran `TERM= something`, a unit file with `Environment=TERM=` — which every
/// ncurses program reads as *a terminal type of the empty string* and answers by
/// printing one line and exiting. That is `!term mc`'s flash, and a pane with no `TERM`
/// at all would not have produced it.
pub fn value<'a>(source: &'a [(String, String)], name: &str) -> Option<&'a str> {
    source
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// **The variables a program chooses a pager through**, all set to [`PAGER`].
///
/// See the module doc: this is a class rather than a program, so it is a table
/// rather than a special case for `git log`.
pub const PAGERS: &[&str] = &["PAGER", "GIT_PAGER", "SYSTEMD_PAGER"];

/// What a pager is when nobody can press a key.
pub const PAGER: &str = "cat";

/// **The pairs the operator's own run carries**, built from a source environment.
///
/// Pure, and that is the point: what the run is given can be asserted without
/// starting a process, which is the only way a test can hold the whole of it
/// rather than the one variable it thought to print.
///
/// The order is the tables' own, so a run is reproducible — the same requirement
/// [`crate::exec::host::SpawnRequest::env`] states for the pairs it is handed.
pub fn env_from(source: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs = Vec::with_capacity(INHERITED.len() + PAGERS.len());
    for name in INHERITED {
        if let Some(value) = value(source, name) {
            pairs.push(((*name).to_string(), value.to_string()));
        }
    }
    // **Forced, not inherited, and that is the difference from the group above.**
    // A console whose `PAGER` is `less` is a console with a person at it; this run
    // has none, so the value is not theirs to choose.
    for name in PAGERS {
        pairs.push(((*name).to_string(), PAGER.to_string()));
    }
    pairs
}

/// [`env_from`] over this process's own environment, which is the console's — see
/// the module doc for why that reading is the honest one.
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

    /// **The whole environment the operator's run is built with, asserted without
    /// a process.**
    ///
    /// This is the test the change is for: it holds every pair at once, so a
    /// later edit that drops one — or that starts inheriting a pager from the
    /// daemon — fails here rather than in an operator's row.
    ///
    /// **`HOME` is in the source and NOT in the answer**, which is the assertion that keeps
    /// this table about the terminal: see [`INHERITED`] for the two runs whose own `HOME` an
    /// inherited pair here would beat, and `super::term::env_from` for the pane, which names
    /// the console's home for itself.
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

    /// **An empty value is not a value.**
    ///
    /// The daemon's environment can carry a variable that is *set and empty* — a
    /// launcher that ran `TERM= something`, a unit file with `Environment=TERM=` — and
    /// passing that on is worse than passing nothing: a program reads the empty string
    /// as *a terminal type*, fails to find its terminfo, and prints one line and exits.
    /// That is the flash `!term mc` produced, and the sentence below is the same one a
    /// run with no `TERM` at all does not get.
    ///
    /// The control is in the same test: a *non-empty* `TERM` is the console's and is
    /// passed through untouched, so this is a rule about emptiness and not a rule about
    /// `TERM`.
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

    /// **A variable the console does not have is not invented.**
    ///
    /// The daemon can be started with no `TERM` at all — a service, not a console —
    /// and then the run has none. The alternative is a default (`xterm-256color`)
    /// that is a claim about a screen nobody measured, which is the empty-haystack
    /// bug with a colour attached.
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

    /// **The pagers are forced, so a console's own pager does not survive.**
    ///
    /// `PAGER=less` in the daemon's environment is a console with a person at it.
    /// Handing that to a run nobody can type into is the hang this table exists to
    /// close, so the value is overwritten rather than inherited — the one asymmetry
    /// between the two groups.
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

    /// **The shell is interactive, and that flag is the whole decision.**
    ///
    /// Without `-i` the operator's `~/.bashrc` returns at its first non-comment
    /// line and no alias is ever defined — the measured defect, exactly. So the
    /// assertion is on the flag rather than on bash, and it names what it buys.
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
