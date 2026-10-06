//! **A command that wants the terminal, refused by name, with the reason and the
//! remedy** — because this harness has no terminal to give it.
//!
//! # The defect, in the operator's own words
//!
//! *"what if I do `! sudo ls /root` … and we also have to think about things like
//! nano. what happens when I run something long and running and that wants to own
//! everything"*
//!
//! The operator's own run gets a **pty** (see [`super::pty`], wired by
//! `InvokeCtx::tty: !gated`) so that `ls --color=auto` colourises the way it does in
//! their console. That is right for `ls` and it is a lie to everything else. The pty
//! is a *capture*: its output is folded into **one transcript row** and its input is
//! `/dev/null`. So a program that asks `isatty(1)` — `ls`, `cargo`, `grep` — gets the
//! answer it wanted and behaves better for it, and a program that wants to **own**
//! the terminal gets the same yes, believes it, and then draws its screen as a stream
//! of cursor-addressing escapes into that one row and waits for a keystroke that
//! cannot arrive. `nano`, `vim`, `top`, `less`, `ssh`, a bare `python`: worse than
//! plain, and worse than plain is not a state to leave a person in.
//!
//! # What this module is, and what it is not
//!
//! It is a **refusal, said**: the command does not run, the row carries the program's
//! name, what the rule looked at, and what to do instead. It is not a terminal, and
//! it is not the fix — see [the TODO](#what-is-deliberately-not-built-here) below for
//! the fix, which is a `!term` verb and a head that is a terminal emulator.
//!
//! # The rule
//!
//! **It looks at the command text, not at the program's own request.** The honest
//! first candidate would be to ask the program — `isatty(1)` is precisely the question
//! `ls` asks — and it cannot be asked without *running* the thing, which is the hang
//! this exists to prevent. What is left is the text, and the text answers three
//! shapes:
//!
//! | class | refused when | why that is the condition |
//! |---|---|---|
//! | [`Class::OwnsTheScreen`] | the name is in [`OWNS_THE_SCREEN`], always | it draws on `/dev/tty` and reads keys from `/dev/tty` whatever its stdin and stdout are — `vim file > /tmp/x` still takes the screen |
//! | [`Class::PagesToTheTerminal`] | the name is in [`PAGES_TO_THE_TERMINAL`] **and its stdout is the terminal** | a pager's whole behaviour is that question: piped or redirected it *cats*, which is why `ls \| cat` is not a wall of escapes and why `man ls \| cat` here is fine |
//! | [`Class::ReadsTheKeyboard`] | the name is in [`READS_THE_KEYBOARD`] **and it was given nothing and nothing feeds its stdin** | a program given nothing is asking for a prompt; `python script.py`, `bash -c '…'` and `cat x.py \| python` are batch runs of the same binary |
//!
//! "Its stdout is the terminal" is [`Stage::stdout_surfaces`] — the grammar's existing,
//! tested answer to *do these bytes reach the caller* — plus one correction: a stage
//! inside `$(…)` writes into a pipe the shell made, so it is not on the screen whatever
//! the outer pipeline says.
//!
//! **Wrappers are unwrapped, so `sudo nano` is a `nano`.** [`crate::intent::unwrap_wrapper`]
//! is the one table of "whose real program is a later argument" in this tree and it is
//! reused rather than copied, which is what makes `! sudo nano /etc/fstab` refuse and
//! `! sudo ls /root` — the operator's own example — run.
//!
//! **Every stage of the line is judged, not the first**, so `ls | less` is refused for
//! the `less`. That is a property of the rule rather than a special case for pipelines:
//! the stages come from `letibot_code::shell`, which already splits them.
//!
//! # What it will therefore miss
//!
//! Stated here because a rule a reader cannot argue with is a blacklist wearing a
//! function's name. Each of these is a real hole and each is the price of a rule that
//! reads the text:
//!
//! 1. **A program nobody listed.** The set is curated by hand, so `nc host port`,
//!    `docker exec -it c sh`, `kubectl exec -it`, `gdb ./prog` (gdb is listed, but only
//!    its given-nothing form is refused) and anything of the operator's own that calls
//!    `isatty` are missed. There is no way to recognise a program by asking it.
//! 2. **A program that starts a pager itself.** `git log`, `git diff`,
//!    `systemctl status`, `journalctl` — the rule sees `git`, which is not a screen
//!    program, and cannot see the `less` git execs a moment later. **This is the
//!    ordinary command most likely to hang on the operator's own line**, and it is
//!    now answered from the other side rather than by this rule: [`super::console`]
//!    sets [`super::console::PAGERS`] to `cat` on the operator's own run, so the
//!    class does not page at all. The rule still cannot see the program; what changed
//!    is that the pager it would have started prints and exits.
//! 3. **A wrapper's own interactive form.** `sudo -s` and `sudo -i` are root shells;
//!    after unwrapping there is nothing left to read, so they are not refused.
//! 4. **A name reached through a shell the rule does not read.** `sh -c 'nano'` and
//!    `$EDITOR file` are both missed — the first because the inner command is a string
//!    rather than a stage, the second because an unresolved program is skipped rather
//!    than guessed at.
//! 5. **A REPL with arguments.** `bash -l`, `bash -i`, `gdb ./prog`, `sqlite3 mydb.db`
//!    and `psql -U me` are all interactive and all have a word after the program.
//!    Telling them from `mysql -e 'select 1'` and `sqlite3 db '.tables'` — which are
//!    batch and must run — needs each program's own option table, and forty option
//!    tables is the blacklist this was supposed to avoid. The rule refuses the form
//!    with **nothing after the name**, which is the form a person types when they mean
//!    *give me a prompt*.
//! 6. **`R` is deliberately not on any list.** `R --version` is a batch command and `R`
//!    alone is a REPL, one letter apart; the rule would rather miss the REPL than
//!    refuse a version check. `lynx`, `w3m` and `links` are off the lists for the same
//!    reason — their non-interactive form is `-dump` and the rule cannot tell the two
//!    apart.
//! 7. **A model's call.** This is consulted only where a pty is handed out, which is
//!    the operator's own run (`InvokeCtx::tty`). A model's `bash` call runs `nano` on a
//!    pipe — stdin `/dev/null`, stdout a pipe — where nano reads EOF and exits rather
//!    than hanging. That is a different defect and it is not this rule's.
//!
//! # What it will refuse that is actually batch — the other side, said too
//!
//! A rule that only listed its misses would be advertising. These are **batch commands
//! this rule refuses**, and each is a consequence of a condition above rather than an
//! accident:
//!
//! - **`ssh host uptime`.** `ssh` is on [`OWNS_THE_SCREEN`], because `ssh host` with no
//!   command is a session. ssh's own rule is *everything after the destination is the
//!   command*, and finding the destination needs ssh's option table — which of `-p`,
//!   `-i`, `-l`, `-o` takes a value — so the rule does not try to find it. A refusal here
//!   costs one sentence telling the operator to run it in another window; guessing the
//!   other way costs a session that hangs.
//! - **`bat -p file` and `man -P cat ls`.** Both are on [`PAGES_TO_THE_TERMINAL`] and
//!   both are asked not to page by a flag the rule does not read, so the condition —
//!   *its stdout is the terminal* — says yes and the program would have said no. `man ls
//!   | cat` and `man ls > file` are the spellings that are not refused.
//! - **`tmux ls`, `screen -ls`.** Listing sessions is not owning a screen, and the names
//!   are on the list because the names are all the rule has.
//!
//! # What is deliberately NOT built here
//!
//! - **DONE (see [`super::term`]): the `!term` verb.** Its own verb on the `!` line, which runs
//!   the command in a pty the *daemon* owns, hands the master to the head, and lets the head be
//!   the terminal emulator: keystrokes down, screen up. That was the real answer to `nano`, and
//!   it is a session with a lifetime, a size and an end — not a tool call that returns a string.
//!   The mechanism is [`super::term`] (`setsid` + `TIOCSCTTY`, a raw byte stream both ways, the
//!   pane's own cgroup), the renderer is `letibot_vt::Screen` painted by `letibot_ui::ansi`,
//!   and the head's half is the
//!   pane in `letibot-tui` — the conversation's rectangle given to the program, the composer
//!   keeping its rows, `ctrl-\` leaving it (a detach that ends nothing) and `!term close`
//!   ending it. **This refusal stays**, and that is not an
//!   oversight: a plain `! nano` is still a program that would draw cursor-addressing escapes
//!   into one transcript row and wait for a keystroke that cannot arrive. What changed is that
//!   every sentence below now names the verb that works instead of filing it as a TODO.
//! - **TODO: the VT features a pane's program may still want.** An alternate screen
//!   (`?1049h`/`l`), cursor addressing (`CUP`, `ED`, `EL`) and `SIGWINCH` on a resize are all
//!   **built** — they are in `letibot_vt` and on `ClientFrame::TermResize`. What is not is
//!   scrollback: a program that scrolls *off* the pane's rectangle is gone, and the decision
//!   that waits is whether the ring is the screen's (a rendering question) or the daemon's (a
//!   storage one). It is filed in [`super::term`] and in the head's `TermPane`.
//! - **DONE (see [`super::console`]): `GIT_PAGER=cat` and friends for the operator's
//!   run.** The cheapest fix for miss 2, and a different change from this one: it is
//!   about the *environment* rather than about refusing a command. It is a table
//!   rather than a special case for `git log`, because the class is every program that
//!   chooses a pager through a variable.

use letibot_code::shell::{self, Context, Stage, Word};

use crate::intent::unwrap_wrapper;

/// **Programs that own the screen.** Each one puts the terminal into raw mode, draws
/// with cursor addressing, and reads the keyboard from `/dev/tty` — which is why the
/// condition is the name alone and not what its stdin or stdout are pointed at.
///
/// Curated by hand. See the module doc's *what it will therefore miss*, item 1.
const OWNS_THE_SCREEN: &[&str] = &[
    // Editors.
    "nano",
    "pico",
    "vi",
    "vim",
    "nvim",
    "view",
    "vimdiff",
    "ex",
    "ed",
    "emacs",
    "emacsclient",
    "micro",
    "helix",
    "hx",
    "mcedit",
    "joe",
    // Full-screen monitors, pickers and menus.
    "top",
    "htop",
    "btop",
    "atop",
    "glances",
    "watch",
    "tmux",
    "screen",
    "byobu",
    "mc",
    "ranger",
    "vifm",
    "nnn",
    "lf",
    "fzf",
    "dialog",
    "whiptail",
    // Full-screen clients, where the session IS the program.
    "mutt",
    "neomutt",
    "alpine",
    "irssi",
    "weechat",
    "ssh",
    "mosh",
    "telnet",
    "sftp",
    "ftp",
];

/// **Pagers.** Refused only when their stdout is the terminal, because that is the
/// condition the program itself applies: a pager writing into a pipe cats.
const PAGES_TO_THE_TERMINAL: &[&str] =
    &["less", "more", "most", "pg", "man", "info", "bat", "batcat"];

/// **Programs that read the keyboard when they are given nothing.** A REPL, a shell
/// prompt, a database prompt, a remote session. Refused only in that form: with a
/// script to run, a command string, or a pipe on stdin they are ordinary batch
/// programs.
const READS_THE_KEYBOARD: &[&str] = &[
    // Interpreters.
    "python",
    "python2",
    "python3",
    "node",
    "deno",
    "bun",
    "ruby",
    "irb",
    "perl",
    "php",
    "lua",
    "luajit",
    "julia",
    "ghci",
    "ocaml",
    "bc",
    "dc",
    "gdb",
    // Shells.
    "sh",
    "bash",
    "zsh",
    "ksh",
    "dash",
    "ash",
    "fish",
    "csh",
    "tcsh",
    "su",
    // Prompts that are not shells.
    "psql",
    "mysql",
    "mariadb",
    "sqlite3",
    "mongosh",
    "redis-cli",
];

/// Which shape of *wants the terminal* the rule recognised.
///
/// Three, and not one, because the three conditions are different facts about the
/// text. Collapsing them would mean one of the two guards that keep the rule off the
/// ordinary commands — `Stage::stdout_surfaces` and *given nothing* — had to go, and
/// with it `man ls | cat` or `python script.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// It takes the screen whatever its stdin and stdout are.
    OwnsTheScreen,
    /// It pages only when its stdout is the terminal.
    PagesToTheTerminal,
    /// It is a prompt only when it was given nothing to run.
    ReadsTheKeyboard,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::OwnsTheScreen => "owns_the_screen",
            Class::PagesToTheTerminal => "pages_to_the_terminal",
            Class::ReadsTheKeyboard => "reads_the_keyboard",
        }
    }

    /// **What the rule looked at for this class** — the half of the refusal that makes
    /// it arguable rather than a list of names.
    fn because(self) -> &'static str {
        match self {
            Class::OwnsTheScreen => {
                "it puts the terminal into raw mode, draws with cursor addressing and reads the \
                 keyboard from `/dev/tty`, whatever its stdin and stdout are pointed at. The name \
                 alone is what this rule looks at, and a name is all it has: whether a program \
                 wants a terminal is the program's own question — `isatty(1)` is exactly what `ls` \
                 asks — and asking it means running the thing, which is the hang this exists to \
                 prevent."
            }
            Class::PagesToTheTerminal => {
                "a pager's behaviour IS the question of where its stdout goes: writing into a pipe \
                 or a file it cats instead of paging, which is why `ls | cat` is not a wall of \
                 escapes. This rule looks at the name and at `Stage::stdout_surfaces` — whether \
                 these bytes reach the caller, which on your own run means the pty — and it \
                 refuses only when both say so."
            }
            Class::ReadsTheKeyboard => {
                "with nothing to run it is a prompt: a loop over a terminal this harness does not \
                 have. This rule looks at the name, at whether the invocation was given any word \
                 after it at all, and at whether anything feeds its stdin — because `python \
                 script.py`, `bash -c '…'` and `cat x.py | python` are batch runs of the same \
                 binary and are not refused."
            }
        }
    }

    /// What to do instead. Short, because it sits inside the row's one-line reason.
    fn instead(self) -> &'static str {
        match self {
            Class::OwnsTheScreen => "run it in the pane: `!term <command>`",
            Class::PagesToTheTerminal => {
                "pipe or redirect its output so it is not writing to a terminal, or read it \
                 in the pane: `!term <command>`"
            }
            Class::ReadsTheKeyboard => {
                "give it a script to run, or run it in the pane: `!term <command>`"
            }
        }
    }
}

/// One refusal: the program the rule read, and the class that refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The program name **after wrappers were unwrapped**, as `Stage::program_name`
    /// reads it. `sudo nano` refuses `nano`, which is the name the operator needs to
    /// see.
    pub program: String,
    pub class: Class,
}

impl Refusal {
    /// The row's reason: one line, and it carries the remedy.
    pub fn why(&self) -> String {
        format!(
            "`{}` needs the terminal and letibot cannot hand you one — {}",
            self.program,
            self.class.instead()
        )
    }

    /// The payload: the reason, what the rule looked at, what the run actually is, and
    /// the TODO that would make it possible. Never silent, and never only a word.
    pub fn body(&self) -> String {
        format!(
            "{}\n\n`{}` was refused because {} The command was NOT run, and nothing is \
             hanging.\n\n{}",
            self.why(),
            self.program,
            self.class.because(),
            WHAT_A_PTY_HERE_IS
        )
    }
}

/// **What the operator's own run actually is**, said once because it is the same fact
/// for all three classes and it is the fact the operator asked about.
const WHAT_A_PTY_HERE_IS: &str = "\
Your `!` line does run on a pty — that is what makes `ls --color=auto` colourise, and it \
is worth keeping. What it is not is a terminal you can type at: the pty's output is \
captured into ONE transcript row and its input is `/dev/null`, so a program that takes \
the screen draws cursor-addressing escapes into that row and then waits for a keystroke \
that can never arrive. That is why this is refused rather than run — the run would not \
hang the session, it would hang itself, in a row you cannot answer. \
**`!term <command>` is the way to run it**: the daemon owns a pty for the pane, the head \
is the terminal emulator that draws it in the conversation's rectangle, `ctrl-\\` \
leaves it running and `!term close` ends it. `!term nano notes.txt`, `!term mc`, `!term top`.";

/// **Given a command line: is it refused, by which program, and why.**
///
/// A pure function of the text — no exec, no backend, no host — which is what makes the
/// rule testable without a process ever starting. `None` means the command runs.
///
/// Every stage of the line is judged, so a pipeline is judged member by member: `ls |
/// less` is refused for the `less`, and `ls | cat` is not refused at all.
pub fn wants_the_terminal(command: &str) -> Option<Refusal> {
    let normalised = shell::normalise(command);
    normalised.stages.iter().find_map(stage_wants_the_terminal)
}

/// One stage, unwrapped, judged against the three conditions.
fn stage_wants_the_terminal(stage: &Stage) -> Option<Refusal> {
    // A stage inside a function body runs only if something calls the function, and
    // whether anything does is a question about the whole script rather than about
    // this stage. The grammar says so; the rule believes it.
    if stage
        .context
        .iter()
        .any(|c| matches!(c, Context::FunctionBody(_)))
    {
        return None;
    }
    // `None` for an unresolved program — `$EDITOR file` — and a program nobody can
    // name is not a program this rule may guess at. See the module doc, item 4.
    let mut name = stage.program_name()?.to_string();
    let mut argv: &[Word] = &stage.argv;
    loop {
        // **The screen is decided first, and before any unwrapping.** `watch` is in
        // both this list and the wrapper table — it runs another program and it owns
        // the screen — and `watch ls` owns the screen whatever `ls` turns out to be.
        if OWNS_THE_SCREEN.contains(&name.as_str()) {
            return Some(Refusal {
                program: name,
                class: Class::OwnsTheScreen,
            });
        }
        // `sudo nano` is a `nano`. `unwrap_wrapper` is `crate::intent`'s one table of
        // whose real program is a later argument, reused rather than copied: a second
        // copy is how `sudo nano` would come to be refused by one half of this tree and
        // not the other.
        let Some((inner, at)) = unwrap_wrapper(&name, argv) else {
            break;
        };
        argv = &argv[(at + 1).min(argv.len())..];
        name = inner;
    }
    if PAGES_TO_THE_TERMINAL.contains(&name.as_str()) && stdout_is_the_terminal(stage) {
        return Some(Refusal {
            program: name,
            class: Class::PagesToTheTerminal,
        });
    }
    if READS_THE_KEYBOARD.contains(&name.as_str()) && argv.is_empty() && !fed_from_elsewhere(stage)
    {
        return Some(Refusal {
            program: name,
            class: Class::ReadsTheKeyboard,
        });
    }
    None
}

/// **Do this stage's bytes reach the terminal?**
///
/// [`Stage::stdout_surfaces`] is the grammar's own tested answer to *do these bytes
/// reach the caller* — false for a `> file` and false mid-pipeline, true for the last
/// member of a pipeline with no output redirection, which on the operator's run is the
/// pty. One correction on top of it: a stage inside `$(…)` or `<(…)` writes into a
/// pipe the shell made, so it is not on the screen whatever the outer pipeline says.
fn stdout_is_the_terminal(stage: &Stage) -> bool {
    let captured_by_the_shell = stage.context.iter().any(|c| {
        matches!(
            c,
            Context::CommandSubstitution | Context::ProcessSubstitution
        )
    });
    !captured_by_the_shell && stage.stdout_surfaces()
}

/// **Is something other than a keyboard feeding this stage's stdin?**
///
/// The other half of *given nothing*: `cat x.py | python` and `python < x.py` are
/// batch runs of a binary that is otherwise a REPL, and a rule that refused them would
/// be refusing the ordinary case.
fn fed_from_elsewhere(stage: &Stage) -> bool {
    if stage.pipe_in {
        return true;
    }
    // `fd` is `None` when the redirection was written without a descriptor, and a bare
    // `<` is stdin. `reads()` covers `<`, `<>`, `<<` and `<<<`.
    stage
        .redirects
        .iter()
        .any(|r| r.op.reads() && matches!(r.fd, None | Some(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal for a command, or `None`. The whole point of the shape: a test
    /// says *refused or not, by which program, for which reason* and never starts a
    /// process.
    fn refused(command: &str) -> Option<(String, Class)> {
        wants_the_terminal(command).map(|r| (r.program, r.class))
    }

    fn owns(command: &str) -> bool {
        refused(command).is_some_and(|(_, c)| c == Class::OwnsTheScreen)
    }

    fn pages(command: &str) -> bool {
        refused(command).is_some_and(|(_, c)| c == Class::PagesToTheTerminal)
    }

    fn reads(command: &str) -> bool {
        refused(command).is_some_and(|(_, c)| c == Class::ReadsTheKeyboard)
    }

    // ------------------------------------------------ the operator's own examples

    /// The two programs the operator named, and the reason for each is a different
    /// one: `nano` owns the screen, `less` is a pager.
    #[test]
    fn the_operators_two_examples_are_refused_and_the_rule_says_which_class() {
        assert_eq!(
            refused("nano /etc/fstab"),
            Some(("nano".to_string(), Class::OwnsTheScreen))
        );
        assert_eq!(
            refused("less /etc/fstab"),
            Some(("less".to_string(), Class::PagesToTheTerminal))
        );
    }

    /// **The operator's own `!` line, and it must NOT be refused.** `! sudo ls /root`
    /// is the example they wrote in the same sentence as `nano`: the wrapper is
    /// unwrapped, the program is `ls`, and `ls` is not on any list.
    #[test]
    fn sudo_ls_root_is_not_refused() {
        assert_eq!(refused("sudo ls /root"), None);
    }

    /// **A wrapper does not hide the program inside it.** `sudo nano` is a `nano`, and
    /// the refusal names `nano` rather than `sudo`, because `nano` is what the operator
    /// has to act on.
    #[test]
    fn a_wrapper_does_not_hide_the_program_and_the_refusal_names_the_inner_one() {
        assert_eq!(
            refused("sudo nano /etc/fstab"),
            Some(("nano".to_string(), Class::OwnsTheScreen))
        );
        // The option-taking form, which is the one a table of flags exists for.
        assert_eq!(
            refused("sudo -u root vim /etc/hosts"),
            Some(("vim".to_string(), Class::OwnsTheScreen))
        );
        // Two wrappers deep, because `unwrap_wrapper` is applied until it stops
        // answering.
        assert_eq!(
            refused("sudo env FOO=1 nano x"),
            Some(("nano".to_string(), Class::OwnsTheScreen))
        );
        // And the same unwrapping with the *inner* program ordinary.
        assert_eq!(refused("sudo -u root git status"), None);
    }

    // ------------------------------------------------ the ordinary commands

    /// **The rule must not misfire on the ordinary cases**, which is a list the
    /// operator gave: `ls`, `grep`, `cargo`, `git`, and an interpreter handed a script.
    /// A rule that refused any of these would be worse than the defect it fixes.
    #[test]
    fn the_ordinary_commands_of_the_operators_list_are_not_refused() {
        for command in [
            "ls",
            "ls -la",
            "ls --color=auto",
            "grep -rn tty crates/",
            "cargo test --workspace",
            "cargo build --release 2>&1 | tee /tmp/build.log",
            "git status",
            "git diff --stat",
            "python script.py",
            "python3 -c 'import os; print(os.getcwd())'",
            "bash script.sh",
            "sh -c 'ls -la'",
            "node app.js",
            "cat big.txt | python",
            "python < script.py",
            "echo 1 + 1 | bc",
            "ls | cat",
            "man ls | cat",
            "less /etc/fstab > /tmp/copy",
        ] {
            assert_eq!(
                refused(command),
                None,
                "`{command}` is an ordinary command and must not be refused"
            );
        }
    }

    /// The list of names is not a list of *substrings*: `ls` must not match `less`, a
    /// program called `nano-banana` is not `nano`, and a name that merely contains a
    /// listed one is left alone.
    #[test]
    fn a_name_that_only_contains_a_listed_one_is_not_refused() {
        for command in [
            "ls -la",
            "lesser /tmp/x",
            "nano-banana --help",
            "vimrc-dump",
        ] {
            assert_eq!(
                refused(command),
                None,
                "`{command}` is not a listed program"
            );
        }
    }

    /// A listed name is matched on the **program name with the directory stripped**, so
    /// an absolute path to a listed program is still refused — `Stage::program_name`
    /// does that, and a rule that did not would be defeated by `/usr/bin/vim`.
    #[test]
    fn an_absolute_path_to_a_listed_program_is_still_refused() {
        assert!(owns("/usr/bin/vim /etc/hosts"));
    }

    // ------------------------------------------------ the pager's own condition

    /// **`ls | less` is refused, and for the `less`** — the pipeline is judged member
    /// by member, which is what makes this a rule about stages rather than about the
    /// first word of a line.
    #[test]
    fn a_pipeline_containing_a_pager_is_refused_for_the_pager() {
        assert_eq!(
            refused("ls | less"),
            Some(("less".to_string(), Class::PagesToTheTerminal))
        );
        assert_eq!(
            refused("cargo build 2>&1 | less"),
            Some(("less".to_string(), Class::PagesToTheTerminal))
        );
        // And the same pipeline with a screen program rather than a pager.
        assert_eq!(
            refused("ls | fzf"),
            Some(("fzf".to_string(), Class::OwnsTheScreen))
        );
    }

    /// **A pager is refused only where it would actually page.** This is the guard that
    /// keeps the rule off `man ls | cat`, and it is the program's own condition rather
    /// than a courtesy: a pager writing into a pipe cats.
    #[test]
    fn a_pager_is_refused_only_when_its_stdout_is_the_terminal() {
        assert!(pages("less /etc/fstab"));
        assert!(pages("man ls"));
        assert!(!pages("less /etc/fstab | cat"));
        assert!(!pages("man ls > /tmp/man.txt"));
        assert!(!pages("man ls | head -50"));
        // Inside a command substitution the bytes go into a pipe the shell made, so
        // the pager is not on the screen however the outer pipeline reads.
        assert!(!pages("echo $(less /etc/fstab)"));
    }

    /// A screen program is refused **whatever** its stdout is, and the asymmetry with
    /// the pager is the reason the two are separate classes: `vim` opens `/dev/tty`.
    #[test]
    fn a_screen_program_is_refused_even_when_its_stdout_is_redirected() {
        assert!(owns("vim /etc/hosts > /tmp/out"));
        assert!(owns("top -b -n 1 | head"));
    }

    // ------------------------------------------------ the prompt's own condition

    /// **A bare interpreter is refused; the same binary with something to run is not.**
    /// This is the second guard, and the operator's list names it: `python script.py`.
    #[test]
    fn an_interpreter_is_refused_only_with_nothing_to_run_and_nothing_feeding_it() {
        assert!(reads("python"));
        assert!(reads("bash"));
        assert!(reads("sqlite3"));
        assert!(reads("sudo -u root python"));

        assert!(!reads("python script.py"));
        assert!(!reads("python -c 'print(1)'"));
        assert!(!reads("bash -c 'ls'"));
        assert!(!reads("cat x.py | python"));
        assert!(!reads("python < x.py"));
        assert!(!reads("python <<'EOF'\nprint(1)\nEOF"));
        assert!(!reads("echo '1+1' | bc"));
    }

    /// **The misses, asserted as misses.** A test that only pinned what the rule
    /// catches would let the module doc's list of holes drift into a claim. Each of
    /// these is interactive and each is *not* refused, and the reason is in the doc.
    #[test]
    fn the_documented_misses_are_misses() {
        // 2. A program that starts a pager itself: the rule sees `git`.
        assert_eq!(refused("git log"), None);
        assert_eq!(refused("systemctl status nginx"), None);
        // 3. A wrapper's own interactive form: after unwrapping there is nothing left.
        assert_eq!(refused("sudo -s"), None);
        // 4. A name reached through a shell the rule does not read.
        assert_eq!(refused("sh -c 'nano'"), None);
        assert_eq!(refused("$EDITOR /etc/fstab"), None);
        // 5. A REPL with arguments — the price of not keeping forty option tables.
        assert_eq!(refused("bash -l"), None);
        assert_eq!(refused("sqlite3 mydb.db"), None);
        assert_eq!(refused("psql -U me"), None);
        assert_eq!(refused("gdb ./prog"), None);
        // 6. `R` is deliberately off every list.
        assert_eq!(refused("R"), None);
        assert_eq!(refused("lynx https://example.com"), None);
        // 1. A program nobody listed.
        assert_eq!(refused("nc example.com 80"), None);
        assert_eq!(refused("docker exec -it c sh"), None);
    }

    /// **The other side of the ledger: batch commands this rule refuses.** Asserted
    /// rather than left in prose, because a documented misfire that nothing pins is a
    /// sentence that will quietly stop being true. Each one is a consequence of a
    /// condition in the rule, and the module doc says which.
    #[test]
    fn the_documented_misfires_are_misfires() {
        // ssh's own rule is "everything after the destination is the command", and
        // finding the destination needs ssh's option table.
        assert_eq!(
            refused("ssh host uptime"),
            Some(("ssh".to_string(), Class::OwnsTheScreen))
        );
        // A flag that says "do not page" is a flag this rule does not read.
        assert!(pages("bat -p src/lib.rs"));
        assert!(pages("man -P cat ls"));
        // `-ls` is not owning a screen, and the name is all the rule has.
        assert!(owns("tmux ls"));
    }

    /// A stage that runs **only if a function is called** is not a stage that runs, so
    /// a `nano` inside a function definition that nothing calls is not a reason to
    /// refuse the line.
    #[test]
    fn a_screen_program_inside_a_function_body_is_not_a_refusal() {
        assert_eq!(refused("f() { nano x; }; echo done"), None);
        // And the call itself is a stage of its own, which the rule does not follow:
        // `f` is not a listed program. Stated rather than pretended.
        assert_eq!(refused("f() { nano x; }; f"), None);
    }

    // ------------------------------------------------ the sentence itself

    /// **The refusal is SAID.** Not a bare word, not an empty payload: the program's
    /// name, the fact that nothing ran, what the rule looked at, what the run actually
    /// is, and the remedy. This is the hard requirement, so it is asserted on the text
    /// rather than on the variant.
    #[test]
    fn the_refusal_says_what_ran_and_what_to_do_instead() {
        let r = wants_the_terminal("nano /etc/fstab").expect("nano is refused");
        assert_eq!(r.program, "nano");
        assert_eq!(r.class, Class::OwnsTheScreen);

        let why = r.why();
        assert!(why.contains("nano"), "{why}");
        assert!(why.contains("cannot hand you one"), "{why}");
        // **The remedy names the verb that works**, which is the half of this test that
        // changed when `!term` was built: *"run it in another window"* was the answer while
        // there was no pane, and it sent the operator out of the harness for a program this
        // harness now runs itself. See `super::term`.
        assert!(why.contains("`!term <command>`"), "{why}");

        let body = r.body();
        assert!(body.contains("`nano` was refused because"), "{body}");
        assert!(body.contains("The command was NOT run"), "{body}");
        assert!(
            body.contains("nothing is hanging"),
            "the row must say that nothing is left running: {body}"
        );
        // What the run actually is, which is the fact the operator asked about.
        assert!(body.contains("ONE transcript row"), "{body}");
        assert!(body.contains("/dev/null"), "{body}");
        // And the way to run it, so the row does not read as a final answer.
        assert!(body.contains("`!term <command>`"), "{body}");
        assert!(
            body.contains("ctrl-\\"),
            "the row must name the way out of the pane it recommends: {body}"
        );

        // The other two classes carry their own remedy and their own account of what
        // the rule looked at, rather than `nano`'s.
        let pager = wants_the_terminal("less /etc/fstab").expect("less is refused");
        assert!(pager.why().contains("pipe or redirect"), "{}", pager.why());
        assert!(
            pager.body().contains("stdout_surfaces"),
            "the pager's refusal must say what it looked at: {}",
            pager.body()
        );

        let repl = wants_the_terminal("python").expect("python is refused");
        assert!(repl.why().contains("give it a script"), "{}", repl.why());
        assert!(
            repl.body().contains("`python` was refused because"),
            "{}",
            repl.body()
        );
        assert!(
            repl.body().contains("nothing to run"),
            "the prompt's refusal must say what it looked at: {}",
            repl.body()
        );
    }

    /// An empty or whitespace command is not a refusal: it is not a program, and the
    /// caller's own empty-command guard owns that case.
    #[test]
    fn an_empty_command_is_not_this_rules_business() {
        assert_eq!(refused(""), None);
        assert_eq!(refused("   "), None);
    }

    /// The class names are the ones a caller logs, so they are pinned: a rename is a
    /// change to a corpus row rather than to a word.
    #[test]
    fn the_class_names_are_stable() {
        assert_eq!(Class::OwnsTheScreen.as_str(), "owns_the_screen");
        assert_eq!(Class::PagesToTheTerminal.as_str(), "pages_to_the_terminal");
        assert_eq!(Class::ReadsTheKeyboard.as_str(), "reads_the_keyboard");
    }
}
