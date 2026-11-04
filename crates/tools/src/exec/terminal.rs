//! A command that wants the terminal, refused by name with reason and remedy: an operator's `!` run
//! is a pty folded into one transcript row with `/dev/null` input. The rule reads the command text,
//! never the program: three classes, each with the condition that separates it from batch form.

use letibot_code::shell::{self, Context, Stage, Word};

use crate::intent::unwrap_wrapper;

/// Programs that own the screen: each puts the terminal into raw mode, draws with cursor
/// addressing and reads the keyboard from `/dev/tty`, so the name alone is the condition.
/// Hand-curated, which is this rule's stated limit.
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

/// Pagers. Refused only when their stdout is the terminal, because that is the condition
/// the program itself applies: a pager writing into a pipe cats.
const PAGES_TO_THE_TERMINAL: &[&str] =
    &["less", "more", "most", "pg", "man", "info", "bat", "batcat"];

/// Programs that read the keyboard when they are given nothing: a REPL, a shell prompt, a
/// database prompt. With a script, a command string or a pipe on stdin they are ordinary
/// batch programs.
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

/// Which shape of wanting the terminal the rule recognised. Three, not one, because each
/// condition is a different fact about the text and collapsing them would cost one of the
/// two guards that keep the rule off the ordinary commands.
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

    /// What the rule looked at for this class, which is the half of the refusal that makes it
    /// arguable rather than a list of names.
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
    /// The program name after wrappers were unwrapped, as `Stage::program_name` reads it, so
    /// `sudo nano` refuses `nano` - the name the operator has to act on.
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

    /// The payload: the reason, what the rule looked at, and what the run actually is.
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

/// What an operator's own run actually is, said once because it is the same fact for all
/// three classes.
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

/// Given a command line: is it refused, by which program, and why. A pure function of the
/// text, so the rule is testable without a process ever starting; every stage is judged, so
/// a pipeline is judged member by member.
pub fn wants_the_terminal(command: &str) -> Option<Refusal> {
    let normalised = shell::normalise(command);
    normalised.stages.iter().find_map(stage_wants_the_terminal)
}

/// One stage, unwrapped, judged against the three conditions.
fn stage_wants_the_terminal(stage: &Stage) -> Option<Refusal> {
    // A stage inside a function body runs only if something calls the function, which is a
    // question about the whole script, so the rule does not judge it here.
    if stage
        .context
        .iter()
        .any(|c| matches!(c, Context::FunctionBody(_)))
    {
        return None;
    }
    // `None` for an unresolved program: a program nobody can name is not one this rule may
    // guess at.
    let mut name = stage.program_name()?.to_string();
    let mut argv: &[Word] = &stage.argv;
    loop {
        // The screen is decided first, before any unwrapping: `watch` runs another program and
        // owns the screen whatever that program turns out to be.
        if OWNS_THE_SCREEN.contains(&name.as_str()) {
            return Some(Refusal {
                program: name,
                class: Class::OwnsTheScreen,
            });
        }
        // `sudo nano` is a `nano`. `unwrap_wrapper` is `crate::intent`'s one table of whose
        // real program is a later argument, reused so the two halves cannot disagree.
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

/// Do this stage's bytes reach the terminal? [`Stage::stdout_surfaces`] is the grammar's own
/// answer; one correction on top of it, because a stage inside `$(...)` or `<(...)` writes
/// into a pipe the shell made and is not on the screen.
fn stdout_is_the_terminal(stage: &Stage) -> bool {
    let captured_by_the_shell = stage.context.iter().any(|c| {
        matches!(
            c,
            Context::CommandSubstitution | Context::ProcessSubstitution
        )
    });
    !captured_by_the_shell && stage.stdout_surfaces()
}

/// Is something other than a keyboard feeding this stage's stdin? `cat x.py | python` and
/// `python < x.py` are batch runs of a binary that is otherwise a REPL.
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

    /// The refusal for a command, or `None`: a test says refused or not, by which program,
    /// for which reason, and never starts a process.
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

    /// Two refusals for two different reasons: `nano` owns the screen, `less` is a pager.
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

    /// A wrapper is unwrapped, so `sudo ls /root` is an `ls`, and `ls` is on no list.
    #[test]
    fn sudo_ls_root_is_not_refused() {
        assert_eq!(refused("sudo ls /root"), None);
    }

    /// A wrapper does not hide the program inside it: the refusal names `nano` rather than
    /// `sudo`, because `nano` is what has to be acted on.
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
        // And the same unwrapping with the inner program ordinary.
        assert_eq!(refused("sudo -u root git status"), None);
    }

    /// The rule must not misfire on the ordinary cases: `ls`, `grep`, `cargo`, `git`, and an
    /// interpreter handed a script. A rule that refused one of these would be worse than the
    /// defect it answers.
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

    /// The list of names is not a list of substrings: `ls` must not match `less`, and
    /// `nano-banana` is not `nano`.
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

    /// A listed name is matched with the directory stripped, so an absolute path to a listed
    /// program is still refused - otherwise `/usr/bin/vim` would defeat the rule.
    #[test]
    fn an_absolute_path_to_a_listed_program_is_still_refused() {
        assert!(owns("/usr/bin/vim /etc/hosts"));
    }

    /// `ls | less` is refused, and for the `less`: the pipeline is judged member by member,
    /// which is what makes this a rule about stages rather than about the first word.
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

    /// A pager is refused only where it would actually page, which is the guard that keeps the
    /// rule off `man ls | cat`: a pager writing into a pipe cats.
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

    /// A screen program is refused whatever its stdout is, and that asymmetry with the pager
    /// is why the two are separate classes: `vim` opens `/dev/tty` itself.
    #[test]
    fn a_screen_program_is_refused_even_when_its_stdout_is_redirected() {
        assert!(owns("vim /etc/hosts > /tmp/out"));
        assert!(owns("top -b -n 1 | head"));
    }

    /// A bare interpreter is refused; the same binary with something to run is not.
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

    /// The misses, asserted as misses, so the holes this rule states cannot drift into a
    /// claim. Each of these is interactive and none is refused.
    #[test]
    fn the_documented_misses_are_misses() {
        // A program that starts a pager itself: the rule sees `git` and not the pager.
        assert_eq!(refused("git log"), None);
        assert_eq!(refused("systemctl status nginx"), None);
        // A wrapper's own interactive form: after unwrapping there is nothing left to read.
        assert_eq!(refused("sudo -s"), None);
        // A name reached through a shell the rule does not read.
        assert_eq!(refused("sh -c 'nano'"), None);
        assert_eq!(refused("$EDITOR /etc/fstab"), None);
        // A REPL with arguments - the price of not keeping an option table per program.
        assert_eq!(refused("bash -l"), None);
        assert_eq!(refused("sqlite3 mydb.db"), None);
        assert_eq!(refused("psql -U me"), None);
        assert_eq!(refused("gdb ./prog"), None);
        // `R` is deliberately off every list, so a version check is never refused.
        assert_eq!(refused("R"), None);
        assert_eq!(refused("lynx https://example.com"), None);
        // A program nobody listed.
        assert_eq!(refused("nc example.com 80"), None);
        assert_eq!(refused("docker exec -it c sh"), None);
    }

    /// The other side of the ledger: batch commands this rule refuses, asserted rather than
    /// left in prose, because a documented misfire that nothing pins will quietly stop being
    /// true.
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
        // Listing sessions is not owning a screen, and the name is all the rule has.
        assert!(owns("tmux ls"));
    }

    /// A stage that runs only if a function is called is not a stage that runs, so a `nano`
    /// inside a function definition nothing calls is not a reason to refuse the line.
    #[test]
    fn a_screen_program_inside_a_function_body_is_not_a_refusal() {
        assert_eq!(refused("f() { nano x; }; echo done"), None);
        // The call itself is a stage of its own and the rule does not follow it: `f` is not a
        // listed program.
        assert_eq!(refused("f() { nano x; }; f"), None);
    }

    /// The refusal is said: the program's name, the fact that nothing ran, what the rule
    /// looked at, what the run actually is, and the remedy - asserted on the text rather than
    /// on the variant.
    #[test]
    fn the_refusal_says_what_ran_and_what_to_do_instead() {
        let r = wants_the_terminal("nano /etc/fstab").expect("nano is refused");
        assert_eq!(r.program, "nano");
        assert_eq!(r.class, Class::OwnsTheScreen);

        let why = r.why();
        assert!(why.contains("nano"), "{why}");
        assert!(why.contains("cannot hand you one"), "{why}");
        // The remedy names the verb that works, because a refusal that sent somebody out of
        // the harness for a program this harness runs itself would be a dead end.
        assert!(why.contains("`!term <command>`"), "{why}");

        let body = r.body();
        assert!(body.contains("`nano` was refused because"), "{body}");
        assert!(body.contains("The command was NOT run"), "{body}");
        assert!(
            body.contains("nothing is hanging"),
            "the row must say that nothing is left running: {body}"
        );
        // And what the run actually is, so the row is not only a refusal.
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
