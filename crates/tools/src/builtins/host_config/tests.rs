//! The `host_config` contract: a read on each arm, a setting this platform has not got refused by
//! name, a change with no `why` refused before the shell runs, an applied change recorded with the
//! value the read returned, and a revert that round-trips through the changelog's own verb.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::backend::tempdir::TempDir;

// ---------------------------------------------------------------------------
// A platform that answers from a script
// ---------------------------------------------------------------------------

/// One command the tool asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

/// **What a scripted command answers**: a closure over the argv it was asked for.
type Answer = Box<dyn Fn(&Asked) -> Result<Ran, String> + Send + Sync>;

/// A runner that runs nothing: it records what was asked and answers from a closure.
struct Scripted {
    asked: Mutex<Vec<Asked>>,
    answer: Answer,
}

impl Scripted {
    fn new(
        answer: impl Fn(&Asked) -> Result<Ran, String> + Send + Sync + 'static,
    ) -> Arc<Scripted> {
        Arc::new(Scripted {
            asked: Mutex::new(Vec::new()),
            answer: Box::new(answer),
        })
    }

    /// The answer to every command, by the words it contains: `("auto repeat delay", "660")`.
    fn answering(pairs: Vec<(&'static str, Ran)>) -> Arc<Scripted> {
        Scripted::new(move |asked| {
            let joined = asked.argv.join(" ");
            for (needle, ran) in &pairs {
                if joined.contains(needle) {
                    return Ok(ran.clone());
                }
            }
            Err(format!("the fixture has no answer for `{joined}`"))
        })
    }

    /// Every command run, each as one line — what a refusal is measured against.
    fn ran(&self) -> Vec<String> {
        self.asked
            .lock()
            .expect("the scripted log")
            .iter()
            .map(|a| a.argv.join(" "))
            .collect()
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().expect("the scripted log").clone()
    }
}

impl Runner for Scripted {
    fn run(&self, argv: &[String], env: &[(String, String)]) -> Result<Ran, String> {
        let asked = Asked {
            argv: argv.to_vec(),
            env: env.to_vec(),
        };
        self.asked
            .lock()
            .expect("the scripted log")
            .push(asked.clone());
        (self.answer)(&asked)
    }
}

/// `xset q`'s answer, as X11 prints it — the one line this tool reads, in its own context.
fn xset_q(delay: u32, rate: u32) -> Ran {
    Ran::ok(format!(
        "Keyboard Control:\n  auto repeat:  on    key click percent:  0    LED mask:  00000002\n  \
         auto repeat delay:  {delay}    repeat rate:  {rate}\n  auto repeating keys:  \
         00ffffffdffffbbf\n"
    ))
}

/// **A real unix socket on disk** — what a [`Channel`] insists on before it will claim a helper can
/// attach. A regular file at a `.sock` path would pass `exists` and prove nothing.
fn a_socket() -> (TempDir, PathBuf) {
    let dir = TempDir::new();
    let path = dir.path().join("daemon.sock");
    std::os::unix::net::UnixListener::bind(&path).expect("a unix socket");
    (dir, path)
}

// ---------------------------------------------------------------------------
// The changelog the tool records into
// ---------------------------------------------------------------------------

/// A changelog in a temp directory — the same fixture `host_changelog`'s tests use.
struct Log {
    dir: PathBuf,
    _owned: TempDir,
    who: String,
}

impl Log {
    fn new() -> Log {
        let d = TempDir::new();
        Log {
            dir: d.path().to_path_buf(),
            _owned: d,
            who: "s-1".to_string(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(changelog::FILE)
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The changes the file holds, folded by the changelog's own reader — a test that asked the
    /// tool what it wrote would be a test of the tool's opinion.
    fn entries(&self) -> Vec<changelog::Entry> {
        changelog::read(&self.path()).entries
    }
}

impl ChangelogScope for Log {
    fn path(&self) -> PathBuf {
        self.path()
    }

    fn host(&self) -> String {
        "fixture-box".to_string()
    }

    fn who(&self) -> String {
        self.who.clone()
    }
}

const NOW: u64 = 1_700_000_000_000;

/// The refusal a call was meant to get, as the sentence the model reads.
fn refusal(r: Result<String, Invocation>) -> String {
    match r {
        Ok(said) => panic!("this call was meant to be refused, and it answered: {said}"),
        Err(f) => match &f.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => {
                format!("{reason} — {}", f.payload)
            }
            other => format!("{other:?} — {}", f.payload),
        },
    }
}

fn set_args(setting: &str, value: &str, why: &str, words: &str) -> Value {
    json!({"verb": "set", "setting": setting, "value": value, "why": why, "words": words})
}

/// **A macOS arm over a scripted runner that keeps its values**: `defaults` reads what a previous
/// `defaults write` put there, so a read-back is the box's answer and not a canned one. The
/// commands themselves are matched by their exact argv, because the spelling is half of what the
/// macOS arm is.
fn macos(key_repeat: &str, initial: &str, low_power: &str) -> (Arc<Scripted>, MacOs) {
    let state = Arc::new(Mutex::new((
        key_repeat.to_string(),
        initial.to_string(),
        low_power.to_string(),
    )));
    let script = Scripted::new({
        let state = Arc::clone(&state);
        move |asked| {
            let a: Vec<&str> = asked.argv.iter().map(String::as_str).collect();
            let mut now = state.lock().expect("the fixture's defaults");
            match a.as_slice() {
                ["defaults", "read", "-g", "KeyRepeat"] => Ok(Ran::ok(format!("{}\n", now.0))),
                ["defaults", "read", "-g", "InitialKeyRepeat"] => {
                    Ok(Ran::ok(format!("{}\n", now.1)))
                }
                ["defaults", "write", "-g", "KeyRepeat", "-int", v] => {
                    now.0 = v.to_string();
                    Ok(Ran::ok(""))
                }
                ["defaults", "write", "-g", "InitialKeyRepeat", "-int", v] => {
                    now.1 = v.to_string();
                    Ok(Ran::ok(""))
                }
                ["pmset", "-g"] => Ok(Ran::ok(format!(
                    "System-wide power settings:\nCurrently in use:\n lowpowermode         {}\n",
                    now.2
                ))),
                ["pmset", "-a", "lowpowermode", v]
                | ["sudo", "-A", "pmset", "-a", "lowpowermode", v] => {
                    now.2 = v.to_string();
                    Ok(Ran::ok(""))
                }
                _ => Err(format!(
                    "the macOS fixture has no answer for `{}`",
                    a.join(" ")
                )),
            }
        }
    });
    (Arc::clone(&script), MacOs::new(script))
}

/// **A Linux arm over a scripted runner that keeps its values**: governor `powersave` (with
/// `performance` offered), X11's repeat at 660 ms / 25 Hz, and `cpupower` and `xset r rate`
/// writing what they are told.
fn linux(governor: &str, offered: &str, delay: u32, rate: u32) -> (Arc<Scripted>, Linux) {
    let state = Arc::new(Mutex::new((
        governor.to_string(),
        delay.to_string(),
        rate.to_string(),
    )));
    let script = Scripted::new({
        let offered = offered.to_string();
        move |asked| {
            let a: Vec<&str> = asked.argv.iter().map(String::as_str).collect();
            let mut now = state.lock().expect("the fixture's values");
            match a.as_slice() {
                ["cat", p] if *p == GOVERNOR => Ok(Ran::ok(format!("{}\n", now.0))),
                ["cat", p] if *p == GOVERNORS => Ok(Ran::ok(format!("{offered}\n"))),
                ["xset", "q"] => Ok(xset_q(
                    now.1.parse().unwrap_or(660),
                    now.2.parse().unwrap_or(25),
                )),
                ["xset", "r", "rate", d, r] => {
                    now.1 = d.to_string();
                    now.2 = r.to_string();
                    Ok(Ran::ok(""))
                }
                ["cpupower", "frequency-set", "-g", g]
                | ["sudo", "-A", "cpupower", "frequency-set", "-g", g] => {
                    now.0 = g.to_string();
                    Ok(Ran::ok(""))
                }
                _ => Err(format!(
                    "the linux fixture has no answer for `{}`",
                    a.join(" ")
                )),
            }
        }
    });
    (Arc::clone(&script), Linux::new(script))
}

// ---------------------------------------------------------------------------
// A read returns the current value, on each arm it can be tested on
// ---------------------------------------------------------------------------

/// **macOS's keyboard repeat, read from `defaults`, in macOS's own unit.**
#[test]
fn a_read_answers_macos_key_repeat() {
    let (script, platform) = macos("2", "15", "0");
    let said = read(&platform, &json!({"verb": "read", "setting": "key-repeat"}))
        .expect("macOS's KeyRepeat is readable");
    assert!(said.contains("`key-repeat` = `2`"), "{said}");
    assert!(said.contains("1/60 s"), "the unit is stated: {said}");
    assert_eq!(script.ran(), vec!["defaults read -g KeyRepeat"]);
}

/// **macOS's delay before the repeat is its own key**, `InitialKeyRepeat`, in the same unit — and a
/// `set` of it writes that key and not `KeyRepeat`.
#[test]
fn a_read_answers_macos_key_repeat_delay() {
    let (script, platform) = macos("2", "15", "0");
    let said = read(
        &platform,
        &json!({"verb": "read", "setting": "key-repeat-delay"}),
    )
    .expect("InitialKeyRepeat is readable");
    assert!(said.contains("`key-repeat-delay` = `15`"), "{said}");
    assert!(said.contains("1/60 s"), "{said}");
    assert!(
        said.contains("defaults read -g InitialKeyRepeat"),
        "the key is named: {said}"
    );
    assert_eq!(script.ran(), vec!["defaults read -g InitialKeyRepeat"]);
}

/// **A delay change writes `InitialKeyRepeat`**, and the record carries both values read back.
#[test]
fn a_key_repeat_delay_change_writes_initial_key_repeat() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat-delay",
            "20",
            "the delay before the repeat is too long",
            "make it shorter",
        ),
        NOW,
    )
    .expect("the change lands");
    assert!(
        script
            .ran()
            .iter()
            .any(|c| c == "defaults write -g InitialKeyRepeat -int 20"),
        "the other key is written: {:?}",
        script.ran()
    );
    assert!(
        !script
            .ran()
            .iter()
            .any(|c| c == "defaults write -g KeyRepeat -int 20"),
        "and not `KeyRepeat`: {:?}",
        script.ran()
    );
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].change.before, "15");
    assert_eq!(entries[0].change.after, "20");
}

/// **macOS's perf policy is `lowpowermode` in `pmset -g`** — the line and only the line, with the
/// unit stated and the privilege said out loud.
#[test]
fn a_read_answers_macos_perf_policy() {
    let (script, platform) = macos("2", "15", "1");
    let said = read(
        &platform,
        &json!({"verb": "read", "setting": "perf-policy"}),
    )
    .expect("macOS 12 and later report lowpowermode");
    assert!(said.contains("`perf-policy` = `1`"), "{said}");
    assert!(said.contains("needs root to change"), "{said}");
    assert!(
        said.contains("`0` (no low-power restriction) or `1`"),
        "{said}"
    );
    assert!(said.contains("`pmset -g`"), "how it was read: {said}");
    assert_eq!(script.ran(), vec!["pmset -g"]);
}

/// **A macOS whose `pmset -g` has no `lowpowermode` line is refused by name**, with the version
/// that arrived — not reported as an empty value, which would make a `before` out of nothing.
#[test]
fn a_macos_without_lowpowermode_is_refused_by_name() {
    let script = Scripted::answering(vec![(
        "pmset -g",
        Ran::ok("System-wide power settings:\nCurrently in use:\n sleep         1\n"),
    )]);
    let platform = MacOs::new(script);
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "perf-policy"}),
    ));
    assert!(said.contains("does not report `lowpowermode`"), "{said}");
    assert!(
        said.contains("macOS 12"),
        "it says when the setting arrived: {said}"
    );
    assert!(
        said.contains("nothing was changed"),
        "and that nothing ran: {said}"
    );
}

/// **A `pmset` that will not answer is a refusal, not an empty perf policy** — the same rule as
/// the governor: a value nobody could read is not a value nobody set.
#[test]
fn a_pmset_that_will_not_answer_refuses_the_read() {
    let script = Scripted::answering(vec![(
        "pmset -g",
        Ran::failed(1, "pmset: this must be run as root"),
    )]);
    let platform = MacOs::new(script);
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "perf-policy"}),
    ));
    assert!(said.contains("`pmset -g` would not answer"), "{said}");
    assert!(
        said.contains("this must be run as root"),
        "it quotes what pmset said: {said}"
    );
}

/// **A perf-policy change runs `pmset -a lowpowermode` and records what the box reads back** —
/// the macOS write arm, which nothing drove before.
#[test]
fn a_perf_policy_change_runs_pmset_and_records_both_values() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "perf-policy",
            "1",
            "the fans never stop in the test window",
            "make it quiet, i don't care about the speed",
        ),
        NOW,
    )
    .expect("the change lands");
    assert!(said.contains("`0` → `1`"), "{said}");
    assert!(
        script.ran().iter().any(|c| c == "pmset -a lowpowermode 1"),
        "pmset wrote it: {:?}",
        script.ran()
    );
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].change.what, "perf-policy");
    assert_eq!(entries[0].change.before, "0");
    assert_eq!(entries[0].change.after, "1");
}

/// **The same change with the helper is `sudo -A pmset …`** — the third argv shape the fixture
/// scripts, and the one the real box takes, since `pmset` needs root.
#[test]
fn a_perf_policy_change_with_the_helper_runs_sudo_dash_a() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    let (_held, socket) = a_socket();
    let privilege = Privilege::Askpass {
        helper: PathBuf::from("/run/user/1000/letibot/letibot-askpass"),
        channel: Channel::new(socket.clone(), "s-1"),
    };
    let said = set(
        Some(&log),
        &platform,
        &privilege,
        &set_args(
            "perf-policy",
            "1",
            "the fans never stop in the test window",
            "make it quiet",
        ),
        NOW,
    )
    .expect("the change lands");
    assert!(said.contains("`0` → `1`"), "{said}");
    let asked = script.asked();
    let applied = asked
        .iter()
        .find(|a| a.argv.iter().any(|w| w == "lowpowermode"))
        .expect("pmset ran");
    assert_eq!(
        applied.argv,
        vec!["sudo", "-A", "pmset", "-a", "lowpowermode", "1"]
    );
    assert_eq!(
        applied.env,
        vec![
            (
                "SUDO_ASKPASS".to_string(),
                "/run/user/1000/letibot/letibot-askpass".to_string()
            ),
            ("LETIBOT_SOCKET".to_string(), socket.display().to_string()),
            ("LETIBOT_SESSION".to_string(), "s-1".to_string()),
            (
                "LETIBOT_COMMAND".to_string(),
                "pmset -a lowpowermode 1".to_string()
            ),
        ]
    );
}

/// **`lowpowermode` takes `0` or `1` and nothing else**, refused before a single command runs —
/// not even the read that would become the `before`.
#[test]
fn a_perf_policy_value_that_is_not_zero_or_one_is_refused_before_anything_runs() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("perf-policy", "2", "the fans are loud", "make it quiet"),
        NOW,
    ));
    assert!(
        said.contains("`2` is not a value `perf-policy` can take"),
        "{said}"
    );
    assert!(
        said.contains("`0` (no low-power restriction) or `1` (low power)"),
        "it says what it takes: {said}"
    );
    assert!(script.ran().is_empty(), "nothing ran: {:?}", script.ran());
    assert!(
        !log.path().exists(),
        "and no changelog was created for a change that never was"
    );
}

/// **Linux's governor, read from the sysfs file the kernel keeps it in.**
#[test]
fn a_read_answers_linux_cpu_governor() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let said = read(
        &platform,
        &json!({"verb": "read", "setting": "cpu-governor"}),
    )
    .expect("the governor is readable");
    assert!(said.contains("`cpu-governor` = `powersave`"), "{said}");
    assert!(
        said.contains("needs root"),
        "the privilege is stated: {said}"
    );
    assert_eq!(script.ran(), vec![format!("cat {GOVERNOR}")]);
}

/// **X11's auto-repeat is one knob with two numbers**, and each half reads its own.
#[test]
fn a_read_answers_linux_key_repeat_delay_and_rate() {
    let (_, platform) = linux("powersave", "performance powersave", 660, 25);
    let delay = read(
        &platform,
        &json!({"verb": "read", "setting": "key-repeat-delay"}),
    )
    .expect("the delay is readable");
    assert!(delay.contains("= `660`"), "{delay}");
    assert!(delay.contains("no root needed"), "{delay}");
    let rate = read(
        &platform,
        &json!({"verb": "read", "setting": "key-repeat-rate"}),
    )
    .expect("the rate is readable");
    assert!(rate.contains("= `25`"), "{rate}");
}

/// **A value the platform does not report is not a value.** A governor file that is not there is
/// a refusal naming the file, not an empty `before`.
#[test]
fn a_governor_this_box_does_not_have_is_refused_by_name() {
    let script = Scripted::new(|asked| {
        let joined = asked.argv.join(" ");
        if joined.starts_with("cat ") {
            Ok(Ran::failed(1, "cat: No such file or directory"))
        } else {
            Err(format!("nothing for `{joined}`"))
        }
    });
    let platform = Linux::new(script);
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "cpu-governor"}),
    ));
    assert!(said.contains("no cpufreq scaling governor"), "{said}");
    assert!(said.contains(GOVERNOR), "the file is named: {said}");
}

/// **A key that was never set is a fact, said as one** — macOS's default applies, and there is no
/// value to record as a `before` until one is written.
#[test]
fn an_unset_macos_default_says_so_rather_than_answering_empty() {
    let script = Scripted::answering(vec![(
        "KeyRepeat",
        Ran::failed(
            1,
            "The domain/default pair of (kCFPreferencesAnyApplication, KeyRepeat) does not exist",
        ),
    )]);
    let platform = MacOs::new(script);
    let said = read(&platform, &json!({"verb": "read", "setting": "key-repeat"}))
        .expect("an unset key is an answer");
    assert!(said.contains("(not set)"), "{said}");
    assert!(said.contains("macOS's own default applies"), "{said}");
}

// ---------------------------------------------------------------------------
// A setting this platform has not got is refused by name, and never mapped
// ---------------------------------------------------------------------------

/// **Linux has no `perf-policy`** — its knob is `cpu-governor`, and the refusal says so.
#[test]
fn linux_refuses_macoss_perf_policy_by_name_and_names_its_own() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "perf-policy"}),
    ));
    assert!(said.contains("linux has no `perf-policy`"), "{said}");
    assert!(said.contains("`cpu-governor`"), "it names the knob: {said}");
    assert!(
        said.contains("a refusal and not a translation"),
        "and says why it is not a mapping: {said}"
    );
    assert!(
        script.ran().is_empty(),
        "nothing was run: {:?}",
        script.ran()
    );
}

/// **macOS has no `cpu-governor`** — its knob is `perf-policy` (`pmset`), and a `set` is refused
/// before anything runs.
#[test]
fn macos_refuses_a_linux_governor_by_name_and_names_its_own() {
    let (script, platform) = macos("2", "15", "0");
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "cpu-governor"}),
    ));
    assert!(said.contains("macos has no `cpu-governor`"), "{said}");
    assert!(said.contains("`perf-policy`"), "{said}");
    assert!(script.ran().is_empty(), "{:?}", script.ran());
}

/// **A platform this tool has no arm for refuses every setting**, rather than the tool being
/// absent from the seat: a role that names a tool nobody registered will not resolve.
#[test]
fn a_platform_with_no_arm_refuses_every_setting() {
    let platform = Other::new("windows");
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "key-repeat"}),
    ));
    assert!(said.contains("no arm for windows"), "{said}");
    let said = refusal(settings(&platform));
    assert!(said.contains("no arm for windows"), "{said}");
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "2", "because", "do it"),
        NOW,
    ));
    assert!(said.contains("no arm for windows"), "{said}");
    assert!(log.lines().is_empty(), "nothing was recorded");
}

// ---------------------------------------------------------------------------
// The two refusals that are the feature: no `why`, no `words` — before anything runs
// ---------------------------------------------------------------------------

/// **A change with no `why` is refused while the box is still untouched.**
#[test]
fn a_change_with_no_why_is_refused_before_the_shell_runs() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("cpu-governor", "performance", "", "make it fast"),
        NOW,
    ));
    assert!(said.contains("`set` needs `why`"), "{said}");
    assert!(
        script.ran().is_empty(),
        "not one command ran, not even the read: {:?}",
        script.ran()
    );
    assert!(log.lines().is_empty(), "and nothing was recorded");
}

/// **A change with no `words` is refused the same way** — the operator's sentence is the half a
/// person can act on a month later.
#[test]
fn a_change_with_no_words_is_refused_before_the_shell_runs() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", ""),
        NOW,
    ));
    assert!(said.contains("`set` needs `words`"), "{said}");
    assert!(script.ran().is_empty(), "{:?}", script.ran());
    assert!(log.lines().is_empty());
}

/// **Both missing is one refusal naming both**, so a caller fixes it in one call.
#[test]
fn a_change_with_neither_why_nor_words_names_both() {
    let (_, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "", ""),
        NOW,
    ));
    assert!(said.contains("`why` and `words`"), "{said}");
}

/// **A value this setting cannot take is refused before the platform runs anything.**
#[test]
fn a_value_the_setting_cannot_take_is_refused_before_the_shell_runs() {
    let (script, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat",
            "fast",
            "the keys are slow",
            "make them faster",
        ),
        NOW,
    ));
    assert!(
        said.contains("is not a value `key-repeat` can take"),
        "{said}"
    );
    assert!(
        said.contains("whole number"),
        "it says what it takes: {said}"
    );
    assert!(script.ran().is_empty(), "{:?}", script.ran());
}

/// **A governor word this kernel does not offer is refused with the list it does offer** — the
/// value is checked against the machine before anything is written.
#[test]
fn a_governor_this_kernel_does_not_offer_is_refused_with_the_list() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("cpu-governor", "turbo", "the box is slow", "make it fast"),
        NOW,
    ));
    assert!(said.contains("`turbo` is not a value"), "{said}");
    assert!(said.contains("performance, powersave"), "{said}");
    assert!(
        !script.ran().iter().any(|c| c.contains("cpupower")),
        "nothing was applied: {:?}",
        script.ran()
    );
    assert!(log.lines().is_empty());
}

// ---------------------------------------------------------------------------
// Privilege, through the refusal and through the argv
// ---------------------------------------------------------------------------

/// **A setting that needs root, in a session with no way to ask, is refused with a sentence** —
/// and nothing runs, because a daemon has no terminal to wait at.
#[test]
fn a_root_setting_with_no_askpass_refuses_and_runs_nothing() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let privilege = Privilege::None(
        "this session is not root and has no way to ask for a password: `letibot-askpass` is not \
         on `PATH`"
            .to_string(),
    );
    let said = refusal(set(
        Some(&log),
        &platform,
        &privilege,
        &set_args(
            "cpu-governor",
            "performance",
            "the box is slow",
            "make it fast",
        ),
        NOW,
    ));
    assert!(
        said.contains("this setting needs root, and this session cannot ask for it"),
        "{said}"
    );
    assert!(said.contains("letibot-askpass"), "{said}");
    assert!(
        !script.ran().iter().any(|c| c.contains("cpupower")),
        "nothing was applied: {:?}",
        script.ran()
    );
    assert!(log.lines().is_empty(), "and nothing was recorded");
}

/// **With the helper, the command is `sudo -A`, and the helper AND the session it asks on are in
/// the command's environment.** A helper with no `LETIBOT_SOCKET`/`LETIBOT_SESSION` prints *not
/// inside a letibot session* and exits 1, so the channel is part of the privilege and not a detail
/// of it — which is what this asserts, pair by pair.
#[test]
fn a_root_setting_with_the_helper_runs_sudo_dash_a() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let (_held, socket) = a_socket();
    let privilege = Privilege::Askpass {
        helper: PathBuf::from("/run/user/1000/letibot/letibot-askpass"),
        channel: Channel::new(socket.clone(), "s-1"),
    };
    let said = set(
        Some(&log),
        &platform,
        &privilege,
        &set_args(
            "cpu-governor",
            "performance",
            "the box is slow",
            "make it fast",
        ),
        NOW,
    )
    .expect("the change lands");
    assert!(said.contains("`powersave` → `performance`"), "{said}");
    let asked = script.asked();
    let applied = asked
        .iter()
        .find(|a| a.argv.iter().any(|w| w == "cpupower"))
        .expect("cpupower ran");
    assert_eq!(
        applied.argv,
        vec![
            "sudo",
            "-A",
            "cpupower",
            "frequency-set",
            "-g",
            "performance"
        ]
    );
    assert_eq!(
        applied.env,
        vec![
            (
                "SUDO_ASKPASS".to_string(),
                "/run/user/1000/letibot/letibot-askpass".to_string()
            ),
            ("LETIBOT_SOCKET".to_string(), socket.display().to_string()),
            ("LETIBOT_SESSION".to_string(), "s-1".to_string()),
            (
                "LETIBOT_COMMAND".to_string(),
                "cpupower frequency-set -g performance".to_string()
            ),
        ]
    );
}

/// **A helper is not a channel** — the reviewer's finding, pinned. `letibot-askpass` on this box
/// with no session to ask on is a helper that would print *not inside a letibot session*, so the
/// decision is a refusal naming what is missing, and the platform's own command never runs.
#[test]
fn a_helper_with_no_session_to_ask_on_is_a_refusal_by_name() {
    let helper = PathBuf::from("/run/user/1000/letibot/letibot-askpass");
    let decided = Privilege::of(Some(helper.clone()), None);
    let Privilege::None(why) = &decided else {
        panic!("a helper with no channel is not a way to ask: {decided:?}");
    };
    assert!(why.contains(&helper.display().to_string()), "{why}");
    assert!(
        why.contains("LETIBOT_SOCKET"),
        "it names what is missing: {why}"
    );
    assert!(
        why.contains("not inside a letibot session"),
        "and what the helper would say: {why}"
    );
    assert!(
        why.contains("Nothing was changed"),
        "and that nothing ran: {why}"
    );

    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &decided,
        &set_args(
            "cpu-governor",
            "performance",
            "the box is slow",
            "make it fast",
        ),
        NOW,
    ));
    assert!(
        said.contains("this setting needs root, and this session cannot ask for it"),
        "{said}"
    );
    assert!(
        !script.ran().iter().any(|c| c.contains("cpupower")),
        "nothing was applied: {:?}",
        script.ran()
    );
    assert!(log.lines().is_empty(), "and nothing was recorded");
}

/// **A socket nobody listens on is not a channel either** — the helper would attach to nothing and
/// fail one layer deeper, inside `sudo`, where the sentence is sudo's rather than ours.
#[test]
fn a_channel_whose_socket_is_not_there_is_a_refusal_by_name() {
    let dir = TempDir::new();
    let missing = dir.path().join("gone.sock");
    let channel = Channel::new(missing.clone(), "s-1");
    let decided = Privilege::of(Some(PathBuf::from("/bin/true")), Some(&channel));
    let Privilege::None(why) = &decided else {
        panic!("a socket that is not there is not a channel: {decided:?}");
    };
    assert!(
        why.contains(&missing.display().to_string()),
        "it names the socket: {why}"
    );
    assert!(why.contains("is not there"), "{why}");
}

/// **Helper, socket and session together are the channel**, and that is what `as_root` is handed.
#[test]
fn a_helper_a_socket_and_a_session_are_a_channel() {
    let (_held, socket) = a_socket();
    let channel = Channel::new(socket, "s-1");
    let helper = PathBuf::from("/run/user/1000/letibot/letibot-askpass");
    let decided = Privilege::of(Some(helper.clone()), Some(&channel));
    assert_eq!(decided, Privilege::Askpass { helper, channel });
}

// ---------------------------------------------------------------------------
// An applied change writes one line, and its `before` is what the read returned
// ---------------------------------------------------------------------------

/// **The record's `before` is the value the read returned**, and the file holds exactly one line.
#[test]
fn an_applied_change_records_one_line_whose_before_is_the_read() {
    let (_, platform) = macos("2", "15", "0");
    let log = Log::new();
    set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat",
            "4",
            "the keys repeat too slowly for the test window",
            "make it repeat faster",
        ),
        NOW,
    )
    .expect("the change lands");

    let lines = log.lines();
    assert_eq!(lines.len(), 1, "one change, one line: {lines:?}");
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    let c = &entries[0].change;
    assert_eq!(c.what, "key-repeat");
    assert_eq!(c.before, "2", "the before is the read's answer");
    assert_eq!(c.after, "4");
    assert_eq!(c.why, "the keys repeat too slowly for the test window");
    assert_eq!(c.words, "make it repeat faster");
    assert_eq!(c.host, "fixture-box");
    assert_eq!(c.who, "s-1");
}

/// **Linux's two-number knob: setting the rate carries the current delay**, because
/// `xset r rate <delay> <rate>` is one command and writing half of it would zero the other.
#[test]
fn setting_the_linux_rate_carries_the_current_delay() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat-rate",
            "40",
            "the keys are slow",
            "make them faster",
        ),
        NOW,
    )
    .expect("the change lands");
    assert!(
        script.ran().iter().any(|c| c == "xset r rate 660 40"),
        "the delay travelled with it: {:?}",
        script.ran()
    );
    let entries = log.entries();
    assert_eq!(entries[0].change.before, "25");
    assert_eq!(entries[0].change.after, "40");
}

/// **The record says what the box says, not what was asked for.** A command that exits 0 without
/// moving the setting is reported as such, and the line carries the value that is really there.
#[test]
fn a_write_the_box_did_not_take_is_recorded_as_the_box_reads() {
    // The write succeeds and the read-back still says 2: a `defaults` write cfprefsd has not
    // picked up, which is the shape this exists for.
    let script = Scripted::new(|asked| {
        let joined = asked.argv.join(" ");
        if joined.starts_with("defaults write") {
            Ok(Ran::ok(""))
        } else if joined.contains("KeyRepeat") {
            Ok(Ran::ok("2\n"))
        } else {
            Err(format!("nothing for `{joined}`"))
        }
    });
    let platform = MacOs::new(script);
    let log = Log::new();
    let said = set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", "make them faster"),
        NOW,
    )
    .expect("the change was applied");
    assert!(
        said.contains("the box still reads back `2`, not `4`"),
        "{said}"
    );
    let entries = log.entries();
    assert_eq!(entries[0].change.after, "2", "the truth, not the ask");
}

/// **A read-back that fails is recorded as `unread`, never as the value that was asked for.** The
/// reviewer's finding: the field's whole job is *what the machine says now*, so a `set` that could
/// not measure it says so rather than promoting the ask into a measurement.
#[test]
fn a_read_back_that_fails_is_recorded_as_unread() {
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let script = Scripted::new(move |asked| {
        let joined = asked.argv.join(" ");
        if joined.starts_with("defaults write") {
            return Ok(Ran::ok(""));
        }
        if joined.contains("KeyRepeat") {
            // The `before` reads, and the read-back after the write does not: cfprefsd gone, or a
            // `defaults` that answers nothing at all.
            let n = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return if n == 0 {
                Ok(Ran::ok("2\n"))
            } else {
                Ok(Ran::failed(1, "defaults: cfprefsd is not answering"))
            };
        }
        Err(format!("nothing for `{joined}`"))
    });
    let platform = MacOs::new(script);
    let log = Log::new();
    let said = set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", "make them faster"),
        NOW,
    )
    .expect("the change was applied");
    assert!(
        said.contains("the record says `unread`"),
        "the reply says what the record says: {said}"
    );
    assert!(
        !said.contains("the box now reads back `4`"),
        "and claims no measurement it did not make: {said}"
    );
    assert!(
        said.contains("macOS would not report `KeyRepeat`"),
        "it names the read that failed: {said}"
    );
    let entries = log.entries();
    assert_eq!(entries.len(), 1, "the change is recorded: {entries:?}");
    assert_eq!(entries[0].change.before, "2", "the before is still a read");
    assert_eq!(entries[0].change.after, UNREAD);
}

/// **A session with no changelog refuses a change before it runs anything** — a change no record
/// would ever see is the state this tool exists to end.
#[test]
fn a_set_with_no_changelog_refuses_and_applies_nothing() {
    let (script, platform) = macos("2", "15", "0");
    let said = refusal(set(
        None,
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", "make them faster"),
        NOW,
    ));
    assert!(said.contains("no host changelog"), "{said}");
    assert!(script.ran().is_empty(), "{:?}", script.ran());
}

/// **A changelog that cannot be written stops the change**, because the record is the half that
/// must not be missing. The `before` read runs first — it changes nothing, and it is the cheaper
/// refusal — so one read has happened by the time the door refuses, and no write has.
#[test]
fn a_changelog_that_cannot_be_written_stops_the_change() {
    let (script, platform) = macos("2", "15", "0");
    // A path whose parent is a FILE, so `create_dir_all` cannot make it. The temp directory is
    // held in a binding: a `TempDir` dropped at the end of the statement removes itself, and the
    // file would then be written into a directory that is gone.
    let dir = TempDir::new();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"").expect("a file where a directory would go");
    struct Unwritable(PathBuf);
    impl ChangelogScope for Unwritable {
        fn path(&self) -> PathBuf {
            self.0.join(changelog::FILE)
        }
        fn host(&self) -> String {
            "fixture-box".to_string()
        }
        fn who(&self) -> String {
            "s-1".to_string()
        }
    }
    let scope = Unwritable(file);
    let said = refusal(set(
        Some(&scope),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", "make them faster"),
        NOW,
    ));
    assert!(
        said.contains("the host changelog cannot be written"),
        "{said}"
    );
    assert!(
        !script.ran().iter().any(|c| c.contains("write")),
        "nothing was written to the box: {:?}",
        script.ran()
    );
    assert!(
        script
            .ran()
            .iter()
            .any(|c| c == "defaults read -g KeyRepeat"),
        "the read that would have been the `before` did run: {:?}",
        script.ran()
    );
}

/// **A `before` that cannot be read stops the change** — a keyboard repeat that could not be read
/// is not a keyboard repeat that was not set, and a revert would have nothing to put back.
#[test]
fn a_before_that_cannot_be_read_stops_the_change() {
    let script = Scripted::new(|asked| {
        let joined = asked.argv.join(" ");
        if joined == "xset q" {
            Ok(Ran::failed(1, "xset: unable to open display"))
        } else {
            Err(format!("nothing for `{joined}`"))
        }
    });
    let platform = Linux::new(script);
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat-rate",
            "40",
            "the keys are slow",
            "make them faster",
        ),
        NOW,
    ));
    assert!(
        said.contains("could not be read, so nothing was changed"),
        "{said}"
    );
    assert!(log.lines().is_empty());
    assert!(
        !log.path().exists(),
        "and the changelog was never created for a change that never was: {}",
        log.path().display()
    );
}

/// **A value the platform would not take costs no command at all** — not even the read that would
/// have become the `before`. The `why`/`words` refusals are the same shape one step earlier.
#[test]
fn a_refused_value_runs_nothing_at_all() {
    let (script, platform) = linux("powersave", "performance powersave", 660, 25);
    let log = Log::new();
    let said = refusal(set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args(
            "key-repeat-rate",
            "fast",
            "the keys are slow",
            "make them faster",
        ),
        NOW,
    ));
    assert!(said.contains("is not a value `key-repeat-rate`"), "{said}");
    assert!(script.ran().is_empty(), "{:?}", script.ran());
    assert!(log.lines().is_empty());
}

// ---------------------------------------------------------------------------
// The revert round-trips through the changelog's own verb
// ---------------------------------------------------------------------------

/// **A change, a revert of it by the changelog's own verb, and the value put back** — the whole
/// loop, through the two tools' own doors and no hand-written JSON in between.
#[test]
fn a_revert_round_trips_through_the_changelogs_own_verb() {
    let (_, platform) = macos("2", "15", "0");
    let log = Log::new();
    let said = read(&platform, &json!({"verb": "read", "setting": "key-repeat"}))
        .expect("readable before the change");
    assert!(said.contains("= `2`"), "{said}");

    set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "4", "the keys are slow", "make them faster"),
        NOW,
    )
    .expect("the change lands");

    // The changelog's own `revert`: it applies nothing, and hands back the `before` to put back.
    let handed = changelog::dispatch(
        &log,
        &json!({"verb": "revert", "change": 1, "words": "put it back"}),
        NOW + 60_000,
    )
    .expect("the revert is recorded");
    assert!(handed.contains("before: `2`"), "{handed}");

    // The seat that made the change puts it back, and that is a new change to record.
    set(
        Some(&log),
        &platform,
        &Privilege::Root,
        &set_args("key-repeat", "2", "the revert was asked for", "put it back"),
        NOW + 120_000,
    )
    .expect("the value goes back");

    let said = read(&platform, &json!({"verb": "read", "setting": "key-repeat"}))
        .expect("readable after the round trip");
    assert!(said.contains("= `2`"), "back where it started: {said}");

    // And the file says so: two changes, the first marked reverted, the second the undo.
    let entries = log.entries();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries[0].reverted.is_some(), "the first change is undone");
    assert_eq!(entries[1].change.before, "4");
    assert_eq!(entries[1].change.after, "2");
}

// ---------------------------------------------------------------------------
// `settings` — the vocabulary, named
// ---------------------------------------------------------------------------

/// **`settings` names the platform and every knob it has**, with the value and the privilege.
#[test]
fn settings_names_the_platform_and_what_it_has() {
    let (_, platform) = linux("powersave", "performance powersave", 660, 25);
    let said = settings(&platform).expect("the platform has settings");
    assert!(said.starts_with("linux — 3 setting(s)"), "{said}");
    for name in ["cpu-governor", "key-repeat-delay", "key-repeat-rate"] {
        assert!(said.contains(name), "`{name}` is listed: {said}");
    }
    assert!(said.contains("needs root"), "{said}");
    assert!(said.contains("no root needed"), "{said}");
    assert!(said.contains("cpu-governor"), "{said}");
}

/// **A setting the platform has not got is refused with the list**, so a caller can see what the
/// name would have been.
#[test]
fn an_unknown_setting_is_refused_with_the_list_of_what_there_is() {
    let (_, platform) = linux("powersave", "performance powersave", 660, 25);
    let said = refusal(read(
        &platform,
        &json!({"verb": "read", "setting": "vm.swappiness"}),
    ));
    assert!(said.contains("linux has no `vm.swappiness`"), "{said}");
    assert!(said.contains("cpu-governor"), "{said}");
}

/// **No verb, and a verb that is not one, are refusals that name the three.**
#[test]
fn a_verb_that_is_not_one_names_the_three() {
    let (_, platform) = linux("powersave", "performance powersave", 660, 25);
    let said = refusal(dispatch(None, &platform, &Privilege::Root, &json!({}), NOW));
    assert!(said.contains("needs a `verb`"), "{said}");
    let said = refusal(dispatch(
        None,
        &platform,
        &Privilege::Root,
        &json!({"verb": "apply"}),
        NOW,
    ));
    assert!(
        said.contains("unknown `host_config` verb `apply`"),
        "{said}"
    );
}

// ---------------------------------------------------------------------------
// The real box
// ---------------------------------------------------------------------------

/// **The Linux arm reads this box's real governor**, when the box has one.
///
/// A kernel with no cpufreq driver — a VM, or a CPU that does not expose one — has no governor at
/// all, and that is [`letibot_tokencore::apparatus::present`]'s skip rather than a failure.
#[test]
fn the_real_linux_arm_reads_this_boxs_governor() {
    let here = std::path::Path::new(GOVERNOR).is_file();
    let Some(_) = letibot_tokencore::apparatus::present("a cpufreq scaling governor", here) else {
        return;
    };
    let platform = Linux::new(Arc::new(System));
    let said = read(
        &platform,
        &json!({"verb": "read", "setting": "cpu-governor"}),
    )
    .expect("the real governor is readable");
    let reported = std::fs::read_to_string(GOVERNOR).expect("the file the read just used");
    assert!(
        said.contains(&format!("`{}`", reported.trim())),
        "the read agrees with the file: {said} vs {reported:?}"
    );
}

/// **The macOS arm reads this box's real `KeyRepeat`** — on a Mac, and nowhere else.
#[cfg(target_os = "macos")]
#[test]
fn the_real_macos_arm_reads_this_boxs_key_repeat() {
    let platform = MacOs::new(Arc::new(System));
    let said = read(&platform, &json!({"verb": "read", "setting": "key-repeat"}))
        .expect("KeyRepeat is readable on a Mac, set or not");
    assert!(said.contains("`key-repeat` ="), "{said}");
    assert!(said.contains("1/60 s"), "{said}");
}

/// **The real runner runs a real program** and reports its exit status — the one thing every
/// other test in this file scripts away.
#[test]
fn the_system_runner_answers_with_the_program_and_its_exit() {
    let ran = System
        .run(
            &["sh".to_string(), "-c".to_string(), "echo hi".to_string()],
            &[],
        )
        .expect("`sh` is on every box this runs on");
    assert_eq!(ran.exit, 0);
    assert_eq!(ran.stdout.trim(), "hi");
    let missing = System.run(&["letibot-no-such-program".to_string()], &[]);
    assert!(missing.is_err(), "a program that is not here is an Err");
}
