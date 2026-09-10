//! T21.1 and T21.2: a process predicate that matches the process evaluating it.
//!
//! # Read this before adding a spelling to the table
//!
//! **This module is not the mechanism.** `TODO.md` T24 is explicit about why:
//!
//! > On 2026-09-09 a process check self-matched five times in one session, with
//! > `process-checks-that-self-match` loaded in memory and T21 open in this file.
//! > The fifth used the bracket trick — which defeats `pgrep`, but the shell
//! > wrapper echoes the expanded pattern back into its own command line, so the
//! > literal string was there to be found. A hazard with that many spellings is not
//! > one you check for; it is one you make unspellable.
//!
//! The mechanism is [`super::scope`]: `pkill -f X` is not *guarded*, it is
//! **unnecessary**, because `job_kill` takes a handle and `job_wait` takes a
//! cgroup. Nothing here would survive a determined spelling, and it is not
//! supposed to have to.
//!
//! What this module is for is `docs/tool-design-brief.md` §2.1 applied to a guard:
//! *a miss is self-correcting in the SAME call*. A model that wrote the pattern
//! form gets back, in one call, **the diagnosis, the pids it currently matches,
//! and the handle-shaped command that does what it meant** — the same shape as
//! `edit`'s read-before-write refusal handing over the file's contents.
//!
//! # Three responses, graded by what the command would do
//!
//! | the command | response |
//! |---|---|
//! | kills by pattern (`pkill`, `killall`, `kill $(pgrep …)`) and the pattern matches anything the harness owns | **refused** — the diagnosis and the handle form |
//! | waits on a pattern (`until pgrep …`) that matches its own shell, or a process that cannot exit while the session runs | **refused** — that loop cannot terminate, and the harness can see it while the model cannot |
//! | merely *asks* (`pgrep`, `ps \| grep`) | **runs**, with a note saying what else it matched and the count it is inflating |
//!
//! The third is the "instrument before you restrict" half of `docs/closed-loop.md`
//! §1. A read-only probe that self-matches produces a wrong number, not a dead
//! server, and a refusal there would buy nothing the note does not.

use super::host::Protected;

/// What a process predicate in a command would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// `pkill`, `killall`, `kill $(pgrep …)`. Destructive.
    Kill,
    /// Inside an `until`/`while` loop. Blocking.
    Wait,
    /// Just asks.
    Probe,
}

/// One process predicate found in a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    /// `pkill`, `pgrep`, `killall`, `ps|grep`.
    pub tool: String,
    /// The pattern as written.
    pub pattern: String,
    /// Whether it is matched against the whole command line (`-f`) or the process
    /// name. The distinction decides what it can hit and therefore what the
    /// diagnosis may claim.
    pub full_cmdline: bool,
    pub intent: Intent,
}

/// A process, or a command line, that a predicate currently matches.
///
/// `pid` is `None` for the one witness the model has the least chance of
/// predicting: **the shell that will evaluate this very command**, whose
/// `/proc/<pid>/cmdline` holds the command text and therefore the pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    pub pid: Option<u32>,
    /// A short name for what it is.
    pub what: String,
    /// The text that matched, clipped.
    pub excerpt: String,
    /// Why it being matched is a problem, in a sentence.
    pub why: String,
    /// Whether waiting for this to exit can never finish while the session runs.
    pub deadlock: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HazardKind {
    /// The predicate matches the shell evaluating it.
    SelfMatch,
    /// The predicate matches a process the harness owns or manages.
    Managed,
    /// A waiter on something that cannot exit while the session runs.
    Deadlock,
}

/// One predicate and everything it currently matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hazard {
    pub kind: HazardKind,
    pub predicate: Predicate,
    pub witnesses: Vec<Witness>,
}

/// The whole analysis of one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to say.
    Clear,
    /// The command runs and the model is told what its predicate also matches.
    /// Clause 1 without a refusal: the count it is about to read is inflated and
    /// it can correct for that without a second call.
    Annotate(Vec<Hazard>),
    /// The command does not run.
    Refuse(Vec<Hazard>),
}

impl Verdict {
    pub fn hazards(&self) -> &[Hazard] {
        match self {
            Verdict::Clear => &[],
            Verdict::Annotate(h) | Verdict::Refuse(h) => h,
        }
    }
}

/// Analyse a command against what the harness knows.
///
/// `protected` is the harness's own pid, its parent chain, and whatever a daemon
/// declared it manages — see [`super::host::HostProcesses::protect_listener`].
/// `command` is matched against **itself** as well, because the shell that
/// evaluates it carries it in its own command line.
pub fn examine(command: &str, protected: &[Protected]) -> Verdict {
    let preds = predicates(command);
    if preds.is_empty() {
        return Verdict::Clear;
    }
    let mut hazards = Vec::new();
    for p in preds {
        let mut witnesses = Vec::new();

        // 1. The shell that will run this. Only for a full-command-line match: a
        //    predicate on the process NAME sees `sh`, not the command text, and
        //    claiming otherwise would be a diagnosis that is wrong.
        if p.full_cmdline && matches(&p.pattern, command) {
            witnesses.push(Witness {
                pid: None,
                what: "the shell that will evaluate this command".into(),
                excerpt: clip(command, 160),
                why: "the command text is this shell's own /proc/<pid>/cmdline, so the \
                      predicate matches the process running it"
                    .into(),
                deadlock: p.intent == Intent::Wait,
            });
        }

        // 2. What the harness owns and the model cannot see.
        for e in protected {
            let hay = if p.full_cmdline { &e.cmdline } else { &e.comm };
            if hay.is_empty() || !matches(&p.pattern, hay) {
                continue;
            }
            witnesses.push(Witness {
                pid: Some(e.pid),
                what: e.comm.clone(),
                excerpt: clip(hay, 160),
                why: e.why.clone(),
                deadlock: e.outlives_the_turn && p.intent == Intent::Wait,
            });
        }

        if witnesses.is_empty() {
            continue;
        }
        let kind = if witnesses.iter().any(|w| w.deadlock) {
            HazardKind::Deadlock
        } else if witnesses.iter().any(|w| w.pid.is_none()) {
            HazardKind::SelfMatch
        } else {
            HazardKind::Managed
        };
        hazards.push(Hazard {
            kind,
            predicate: p,
            witnesses,
        });
    }

    if hazards.is_empty() {
        return Verdict::Clear;
    }
    if hazards
        .iter()
        .any(|h| matches!(h.predicate.intent, Intent::Kill | Intent::Wait))
    {
        Verdict::Refuse(hazards)
    } else {
        Verdict::Annotate(hazards)
    }
}

/// Does `pattern` — read the way `pgrep` reads it, as an ERE — match `hay`?
///
/// A pattern that will not compile falls back to a **literal substring** test.
/// That is not the approximation `grep` refuses: there the question was whether
/// the search found what was asked for, and an approximation would have been an
/// answer to a different question. Here the question is "could this hurt", and a
/// broader test is the conservative direction.
fn matches(pattern: &str, hay: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(hay),
        Err(_) => hay.contains(pattern),
    }
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    format!("{cut}…")
}

/// Split a command into words, treating shell separators as words of their own
/// and honouring quotes.
///
/// Not a shell parser and does not need to be: it is looking for the *presence* of
/// a predicate, not evaluating one. `$( )` and backticks become separators so a
/// `pgrep` inside a substitution is a word rather than part of one.
fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let push = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                push(&mut cur, &mut out);
                out.push("$(".into());
            }
            '`' | ')' | '(' => {
                push(&mut cur, &mut out);
                out.push(c.to_string());
            }
            '|' | ';' | '&' | '\n' => {
                push(&mut cur, &mut out);
                out.push(c.to_string());
            }
            c if c.is_whitespace() => push(&mut cur, &mut out),
            c => cur.push(c),
        }
    }
    push(&mut cur, &mut out);
    out
}

/// Flags that take a value, so the value is not mistaken for the pattern.
const VALUED: &[&str] = &["-u", "-U", "-g", "-G", "-P", "-s", "-t", "-e", "--signal", "-d", "--delay"];

fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

/// Every process predicate in a command, with what it would do.
pub fn predicates(command: &str) -> Vec<Predicate> {
    let ws = words(command);
    let lowered: Vec<String> = ws.iter().map(|w| basename(w).to_string()).collect();
    // A loop keyword anywhere makes a predicate inside it a WAIT. Coarse on
    // purpose: `until … ; do sleep 1; done` and `while ! …` and a `for` retry loop
    // are the same hazard, and distinguishing them would need a real parser to
    // buy nothing.
    let looping = lowered
        .iter()
        .any(|w| w == "until" || w == "while" || w == "done");
    // A `kill` anywhere makes a `pgrep` in the same command a KILL: `kill $(pgrep
    // -f x)` is `pkill -f x` with more steps.
    let killing = lowered.iter().any(|w| w == "kill" || w == "xargs");

    let mut out = Vec::new();
    let mut saw_ps = false;
    for (i, w) in lowered.iter().enumerate() {
        match w.as_str() {
            "ps" => saw_ps = true,
            "pgrep" | "pkill" => {
                let (pattern, full) = operand(&ws[i + 1..]);
                let Some(pattern) = pattern else { continue };
                let intent = if w == "pkill" || killing {
                    Intent::Kill
                } else if looping {
                    Intent::Wait
                } else {
                    Intent::Probe
                };
                out.push(Predicate {
                    tool: w.clone(),
                    pattern,
                    full_cmdline: full,
                    intent,
                });
            }
            "killall" | "pidof" => {
                let (pattern, _) = operand(&ws[i + 1..]);
                let Some(pattern) = pattern else { continue };
                out.push(Predicate {
                    tool: w.clone(),
                    pattern,
                    // `killall` matches the process NAME, so the shell running the
                    // command is not a witness and the diagnosis must not claim it.
                    full_cmdline: false,
                    intent: if w == "killall" {
                        Intent::Kill
                    } else if looping {
                        Intent::Wait
                    } else {
                        Intent::Probe
                    },
                });
            }
            "grep" | "egrep" | "fgrep" if saw_ps => {
                let (pattern, _) = operand(&ws[i + 1..]);
                let Some(pattern) = pattern else { continue };
                out.push(Predicate {
                    tool: "ps|grep".into(),
                    pattern,
                    // `ps` prints whole command lines, so this one does see itself.
                    full_cmdline: true,
                    intent: if killing {
                        Intent::Kill
                    } else if looping {
                        Intent::Wait
                    } else {
                        Intent::Probe
                    },
                });
            }
            _ => {}
        }
    }
    out
}

/// The first non-flag operand after a command word, and whether `-f` was seen.
fn operand(rest: &[String]) -> (Option<String>, bool) {
    let mut full = false;
    let mut i = 0;
    while i < rest.len() {
        let w = &rest[i];
        if w == "|" || w == ";" || w == "&" || w == ")" || w == "\n" {
            break;
        }
        if w == "-f" || w == "--full" || w == "-af" || w == "-fa" {
            full = true;
            i += 1;
            continue;
        }
        if VALUED.contains(&w.as_str()) {
            i += 2;
            continue;
        }
        if let Some(flag) = w.strip_prefix('-') {
            // `-9`, `-KILL`, `-Ei`, `--exact` and the rest: flags, and a combined
            // short flag may still carry the `f`.
            if !flag.is_empty() && !w.starts_with("--") && flag.contains('f') {
                full = true;
            }
            i += 1;
            continue;
        }
        return (Some(w.clone()), full);
    }
    (None, full)
}

/// The refusal body: the diagnosis, what it currently matches, and the command
/// that does what was meant.
///
/// Structured as one function so that the shape cannot drift between the kill case
/// and the wait case: both get all three parts, because a refusal missing the
/// third is one the model routes around.
pub fn refusal(command: &str, hazards: &[Hazard]) -> String {
    let mut s = String::new();
    s.push_str(
        "This command was NOT run. Its process predicate matches the process \
         evaluating it, or a process this harness manages — which is the failure \
         `TODO.md` T21 is about, and it was measured on this box five times in one \
         session with the lesson already in memory.\n\n",
    );
    for h in hazards {
        s.push_str(&format!(
            "`{} {}{}` ({}) currently matches:\n",
            h.predicate.tool,
            if h.predicate.full_cmdline { "-f " } else { "" },
            h.predicate.pattern,
            match h.predicate.intent {
                Intent::Kill => "it would kill these",
                Intent::Wait => "it would wait for these to go away",
                Intent::Probe => "it counts these",
            }
        ));
        for w in &h.witnesses {
            s.push_str(&format!(
                "  - {} {} — {}\n      {}\n",
                w.pid.map(|p| format!("pid {p}")).unwrap_or("pid n/a".into()),
                w.what,
                w.why,
                w.excerpt
            ));
            if w.deadlock {
                s.push_str(
                    "      THIS LOOP CANNOT TERMINATE: that process does not exit while \
                     this session runs, so the condition never becomes false.\n",
                );
            }
        }
        s.push('\n');
    }
    s.push_str(remedy(hazards));
    s.push_str(&format!(
        "\n\nThe command as written was:\n  {}\n",
        clip(command, 400)
    ));
    s
}

/// What to call instead. **The point of the whole module**: the alternative has no
/// pattern, so there is nothing that can match the caller.
fn remedy(hazards: &[Hazard]) -> &'static str {
    let waiting = hazards
        .iter()
        .any(|h| h.predicate.intent == Intent::Wait);
    if waiting {
        "Wait on a HANDLE instead of a pattern. `job_wait` takes `job` (a job id from \
         `bash` or `job_list`) or `scope`, and blocks until that job leaves `running` \
         or that cgroup empties — with a mandatory deadline whose expiry is reported \
         as its own outcome, never as completion. There is no pattern in it, so there \
         is nothing that can match the waiter. `job_list` answers \"is it still \
         running\" without blocking at all."
    } else {
        "Kill by HANDLE instead of by pattern. `job_kill` takes `job` (a job id from \
         `bash` or `job_list`) or `scope` (`turn`, `session`, or an explicit scope's \
         name) and kills that cgroup — every descendant included, with no pattern, so \
         nothing can match the caller. `job_list` shows the ids. A process this \
         harness did not start is not yours to kill from here; say what you want \
         stopped and why."
    }
}

/// The note an admitted probe carries. Clause 1 without a refusal.
pub fn annotation(hazards: &[Hazard]) -> String {
    let mut s = String::from(
        "this predicate also matches the process asking, so the count it returns is \
         inflated by at least one and is not a measurement of what you meant: ",
    );
    let mut parts = Vec::new();
    for h in hazards {
        for w in &h.witnesses {
            parts.push(format!(
                "{} ({})",
                w.pid.map(|p| format!("pid {p}")).unwrap_or("the evaluating shell".into()),
                w.what
            ));
        }
    }
    s.push_str(&parts.join(", "));
    s.push_str(". `job_list` answers the same question from cgroup membership, with no pattern.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected(pid: u32, comm: &str, cmdline: &str, why: &str, outlives: bool) -> Protected {
        Protected {
            pid,
            comm: comm.into(),
            cmdline: cmdline.into(),
            why: why.into(),
            outlives_the_turn: outlives,
        }
    }

    #[test]
    fn t21_1_a_pkill_whose_pattern_matches_the_evaluating_shell_is_refused() {
        let cmd = "pkill -f harnessd";
        let v = examine(cmd, &[protected(384248, "harnessd", "/usr/bin/harnessd --port 9099", "this is the harness daemon this session runs inside", false)]);
        let Verdict::Refuse(h) = &v else {
            panic!("must refuse: {v:?}")
        };
        let body = refusal(cmd, h);
        // The diagnosis names WHAT IT CURRENTLY MATCHES, both the shell and the pid.
        assert!(body.contains("the shell that will evaluate this command"), "{body}");
        assert!(body.contains("pid 384248"), "{body}");
        assert!(body.contains("harness daemon"), "{body}");
        // And it hands over the handle form, which has no pattern.
        assert!(body.contains("job_kill"), "{body}");
    }

    #[test]
    fn t21_2_a_waiter_on_the_model_server_is_a_deadlock_the_harness_can_see() {
        let cmd = "until pgrep -f llama-server; do sleep 2; done";
        let v = examine(
            cmd,
            &[protected(
                4242,
                "llama-server",
                "/opt/llama.cpp/llama-server --port 8080",
                "this is the model server serving this session",
                true,
            )],
        );
        let Verdict::Refuse(h) = &v else {
            panic!("must refuse: {v:?}")
        };
        assert_eq!(h[0].kind, HazardKind::Deadlock);
        let body = refusal(cmd, h);
        assert!(body.contains("CANNOT TERMINATE"), "{body}");
        assert!(body.contains("model server serving this session"), "{body}");
        // The remedy is the WAIT VERB, not a better loop.
        assert!(body.contains("job_wait"), "{body}");
        assert!(!body.contains("job_kill"), "a waiter's remedy is not a kill: {body}");
    }

    #[test]
    fn the_bracket_trick_still_self_matches_through_the_shell() {
        // The fifth failure of the five. `[h]arnessd` as an ERE does not match the
        // literal `[h]arnessd` in the shell's own cmdline — so this witness comes
        // from the PROTECTED process, not from the shell, and the diagnosis says
        // which. A guard that claimed the shell here would be wrong.
        let cmd = "until ps aux | grep -E '[h]arnessd'; do sleep 1; done";
        let v = examine(
            cmd,
            &[protected(384248, "harnessd", "/usr/bin/harnessd", "the harness daemon", false)],
        );
        let Verdict::Refuse(h) = &v else {
            panic!("must refuse: {v:?}")
        };
        assert!(h[0].witnesses.iter().any(|w| w.pid == Some(384248)));
    }

    #[test]
    fn a_bare_probe_runs_and_is_annotated_rather_than_refused() {
        // Instrument before you restrict: a self-matching `pgrep` produces a wrong
        // number, not a dead server.
        let v = examine("pgrep -f cargo | wc -l", &[]);
        let Verdict::Annotate(h) = &v else {
            panic!("a probe is not refused: {v:?}")
        };
        let note = annotation(h);
        assert!(note.contains("inflated by at least one"), "{note}");
        assert!(note.contains("job_list"), "{note}");
    }

    #[test]
    fn kill_of_a_pgrep_substitution_is_a_kill() {
        let ps = predicates("kill -9 $(pgrep -f my-server)");
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].intent, Intent::Kill);
        assert_eq!(ps[0].pattern, "my-server");
        assert!(ps[0].full_cmdline);
    }

    #[test]
    fn a_command_with_no_process_predicate_is_clear() {
        assert_eq!(examine("cargo test --workspace", &[]), Verdict::Clear);
        assert_eq!(examine("grep -rn 'pkill' docs/", &[]).hazards().len(), 0);
    }

    #[test]
    fn killall_matches_the_process_name_and_the_diagnosis_does_not_claim_the_shell() {
        // `killall` reads /proc/<pid>/comm, so the command text is not a witness
        // and saying it was would be a wrong diagnosis in a refusal.
        let v = examine(
            "killall llama-server",
            &[protected(7, "llama-server", "/opt/llama-server --port 8080", "the model server", false)],
        );
        let Verdict::Refuse(h) = &v else {
            panic!("{v:?}")
        };
        assert!(h[0].witnesses.iter().all(|w| w.pid.is_some()), "{h:?}");
    }

    #[test]
    fn a_valued_flag_is_not_mistaken_for_the_pattern() {
        let ps = predicates("pkill -u dead -f stale-worker");
        assert_eq!(ps[0].pattern, "stale-worker");
        assert!(ps[0].full_cmdline);
    }

    #[test]
    fn a_quoted_pattern_keeps_its_spaces() {
        let ps = predicates("pkill -f 'python train.py'");
        assert_eq!(ps[0].pattern, "python train.py");
    }

    #[test]
    fn an_uncompilable_pattern_falls_back_to_a_literal_test_rather_than_passing() {
        // `[` alone will not compile as an ERE. The conservative direction here is
        // a broader test, not a silent clear.
        assert!(matches("a[b", "xxa[byy"));
    }
}
