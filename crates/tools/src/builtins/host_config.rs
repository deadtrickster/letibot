//! `host_config` — **this box's own settings, in the platform's own vocabulary.**
//!
//! The operator's ask, from the room's design: *keyboard and input*, and *perf policy*, named
//! the way the platform names them, with the privilege stated and **every change written into
//! the host changelog**.
//!
//! # The vocabulary is per platform, and a setting this platform has not got is refused by name
//!
//! *"Perf policy"* on a Mac and on a Linux box are not the same knob. macOS has `pmset -a
//! lowpowermode` (`0` or `1`); Linux has a cpufreq governor (`powersave`, `performance`,
//! `schedutil`, …) written with `cpupower frequency-set -g`. So they are **two settings with two
//! names** — `perf-policy` on macOS, `cpu-governor` on Linux — and asking a Mac for
//! `cpu-governor` is refused with a sentence that says what macOS has instead. Mapping one onto
//! the other silently is the thing this refusal exists to prevent: a `set` that "worked" by
//! changing a different knob is worse than a refusal.
//!
//! # A read comes first, and the `before` is not optional
//!
//! A changelog line whose `before` is missing cannot be reverted, so `set` **reads the current
//! value before it changes anything** and hands that to the record. A read that fails is a
//! refusal, not an empty `before`: a governor that could not be read is not a governor that was
//! not set.
//!
//! # Privilege is stated, never assumed
//!
//! Each setting says whether it needs root. When it does, the tool needs a way to *ask*, and the
//! box's answer is `letibot-askpass` (`sudo -A` through the daemon's shim — the password goes
//! head → daemon → sudo and nowhere else). **A daemon has no tty**, so a bare `sudo` is a hang:
//! with no askpass there is no channel, and the refusal says so in a sentence a person can act
//! on rather than waiting two minutes for a prompt that never appears.
//!
//! # Every change goes into the record
//!
//! The apply is the middle of three acts: **read**, apply, **record**. The record's door is
//! proved open *before* the apply ([`changelog::writable`]) — a change that lands with no line
//! in the file is exactly the state the changelog exists to end — and the line is written the
//! moment the apply returns. `why` and `words` are required and are checked **before anything
//! runs**: a `set` with no reason is refused while the box is still untouched, which is the only
//! moment at which refusing costs nothing.
//!
//! Undoing a change is the changelog's own verb: `host_changelog revert N` hands back the
//! recorded `before`, and putting the value back is a `set` — a new change, recorded, which is
//! what the changelog's own doc says a second move is.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::builtins::changelog::{self, ChangelogScope};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name.
pub const TOOL: &str = "host_config";

// ---------------------------------------------------------------------------
// What a platform hands back
// ---------------------------------------------------------------------------

/// **A platform's refusal, in two sentences**: what it is about, and what a person can do next.
///
/// The same shape [`Invocation::failed`] takes, kept as its own type so a platform can answer
/// one without knowing what a tool invocation is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub what: String,
    pub why: String,
}

impl Refusal {
    pub fn new(what: impl Into<String>, why: impl Into<String>) -> Refusal {
        Refusal {
            what: what.into(),
            why: why.into(),
        }
    }
}

impl From<Refusal> for Invocation {
    fn from(r: Refusal) -> Invocation {
        Invocation::failed(r.what, r.why)
    }
}

/// **One setting, as one platform spells it.** `unit` is the platform's own unit and is printed
/// with every value, because macOS's `KeyRepeat` is in 1/60 s and X11's rate is in hertz — two
/// numbers that would look interchangeable and are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// The name a caller asks for, in this platform's vocabulary.
    pub name: &'static str,
    /// What the value counts, in this platform's words.
    pub unit: &'static str,
    /// Whether changing it needs root — stated here so a refusal can say so before it runs.
    pub needs_root: bool,
    /// How it is read and written, in the platform's own words: the commands, named.
    pub note: &'static str,
}

/// One read: the value the platform reports now, and how it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The value, as the platform prints it. Empty is a fact — *not set* — and is said as one.
    pub value: String,
    /// How it was read, in a person's words (`xset q`, `defaults read -g KeyRepeat`).
    pub how: String,
    /// A sentence for a value that needs one: *not set, the system default applies*.
    pub note: String,
}

impl Reading {
    fn of(value: impl Into<String>, how: impl Into<String>) -> Reading {
        Reading {
            value: value.into(),
            how: how.into(),
            note: String::new(),
        }
    }

    fn noted(mut self, note: impl Into<String>) -> Reading {
        self.note = note.into();
        self
    }
}

/// One apply: what ran, and what the box says now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The commands that ran, each as one line a person can read back.
    pub ran: Vec<String>,
}

/// **One command's answer.** `exit` is the status; bytes are read as text, because every setting
/// here is a word or a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    /// A command that answered successfully.
    pub fn ok(stdout: impl Into<String>) -> Ran {
        Ran {
            exit: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A command that failed, with what it said.
    pub fn failed(exit: i32, stderr: impl Into<String>) -> Ran {
        Ran {
            exit,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    /// The first line of what it said, for a refusal that has to quote it.
    fn said(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("nothing")
            .to_string()
    }
}

/// **Where a platform's commands run.** One argv in, one answer out.
///
/// A trait for the reason the changelog's scope is one: a test must be able to drive the whole
/// tool without changing this box's keyboard repeat or its governor. The production runner is
/// [`System`]; a test's is a recording fake.
pub trait Runner: Send + Sync {
    /// Run one argv with these environment additions. `Err` is a spawn that never happened —
    /// a program this box has not got — and is a different fact from a non-zero exit.
    fn run(&self, argv: &[String], env: &[(String, String)]) -> Result<Ran, String>;
}

/// The real box: `std::process::Command`, no shell.
pub struct System;

impl Runner for System {
    fn run(&self, argv: &[String], env: &[(String, String)]) -> Result<Ran, String> {
        let Some((program, args)) = argv.split_first() else {
            return Err("an empty argv".to_string());
        };
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().map_err(|e| format!("{program}: {e}"))?;
        Ok(Ran {
            exit: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Privilege
// ---------------------------------------------------------------------------

/// **How a setting that needs root can be asked for**, decided before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Privilege {
    /// This process is already root: no helper, no question.
    Root,
    /// `sudo -A` with this helper — `letibot-askpass`, which reaches the head.
    Askpass(PathBuf),
    /// There is no way to ask. The sentence says what is missing and what a person can do.
    None(String),
}

impl Privilege {
    /// What this session has. `$SUDO_ASKPASS` wins (the daemon's shim points it at the helper);
    /// otherwise `letibot-askpass` on `PATH`; otherwise nothing, and the sentence says so.
    pub fn detect() -> Privilege {
        // SAFETY: `geteuid` has no preconditions and touches no memory.
        if unsafe { libc::geteuid() } == 0 {
            return Privilege::Root;
        }
        let named = std::env::var_os("SUDO_ASKPASS").map(PathBuf::from);
        if let Some(p) = named.filter(|p| p.is_file()) {
            return Privilege::Askpass(p);
        }
        if let Some(p) = which("letibot-askpass") {
            return Privilege::Askpass(p);
        }
        Privilege::None(format!(
            "this session is not root and has no way to ask for a password: `$SUDO_ASKPASS` \
             names nothing and `letibot-askpass` is not on `PATH`. A daemon has no terminal, so \
             a bare `sudo` here would wait for a prompt nobody can see. Start the daemon under \
             `letibot` (which installs the helper), or run the command yourself in a shell you \
             are sitting at."
        ))
    }

    /// The argv to run `argv` as root, and the environment that carries the helper.
    ///
    /// Public because a [`Platform`] is what builds a command and this is where the *how* lives:
    /// a platform that re-implemented it would be a second answer to *can this session ask?*.
    pub fn as_root(
        &self,
        argv: &[String],
    ) -> Result<(Vec<String>, Vec<(String, String)>), Refusal> {
        match self {
            Privilege::Root => Ok((argv.to_vec(), Vec::new())),
            Privilege::Askpass(helper) => {
                let mut full = vec!["sudo".to_string(), "-A".to_string()];
                full.extend(argv.iter().cloned());
                Ok((
                    full,
                    vec![("SUDO_ASKPASS".to_string(), helper.display().to_string())],
                ))
            }
            Privilege::None(why) => Err(Refusal::new(
                "this setting needs root, and this session cannot ask for it",
                why.clone(),
            )),
        }
    }
}

/// The first `name` on `PATH` that is a file — what `command -v` answers, without a shell.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------------------
// The platforms
// ---------------------------------------------------------------------------

/// **One platform's settings, and the commands that read and write them.**
///
/// A trait rather than `cfg!` arms inside the tool, because a Mac's `defaults` and a Linux box's
/// `xset` are *data* — argv and parsers — and data can be driven on either box. A test on Linux
/// can therefore test the macOS arm, which is the only way a refusal for a setting this platform
/// has not got is ever measured from both sides.
pub trait Platform: Send + Sync {
    /// The platform's name, as `std::env::consts::OS` spells it.
    fn name(&self) -> &'static str;

    /// Every setting this platform has, in this platform's vocabulary.
    fn settings(&self) -> &'static [Setting];

    /// What the setting is now.
    fn read(&self, setting: &str) -> Result<Reading, Refusal>;

    /// **Set it**, after checking the value can be taken. Runs nothing for a value the platform
    /// would not accept.
    fn apply(&self, setting: &str, value: &str, privilege: &Privilege) -> Result<Applied, Refusal>;
}

/// The setting this platform has not got — refused by name, with what it has instead.
///
/// `other` is the *concept's* name on the platform that does have it, when one does: the point is
/// that a person who asked in macOS's words is told Linux's, and the other way round. That is a
/// refusal that teaches, not a mapping.
fn no_such_setting(
    platform: &str,
    asked: &str,
    settings: &[Setting],
    other: Option<&str>,
) -> Refusal {
    let names: Vec<String> = settings.iter().map(|s| format!("`{}`", s.name)).collect();
    let has = if names.is_empty() {
        "nothing".to_string()
    } else {
        names.join(", ")
    };
    let instead = match other {
        Some(o) => format!(
            " The same idea on {platform} is `{o}` — a different knob with its own values, which \
             is why this is a refusal and not a translation."
        ),
        None => String::new(),
    };
    Refusal::new(
        format!("{platform} has no `{asked}`"),
        format!("{platform}'s settings here are {has}.{instead} Nothing was changed."),
    )
}

/// The value a setting will not take, named with what it will.
fn no_such_value(setting: &str, value: &str, wants: &str) -> Refusal {
    Refusal::new(
        format!("`{value}` is not a value `{setting}` can take"),
        format!("{wants} Nothing was changed."),
    )
}

/// A positive whole number, as the value these platforms take.
fn whole(setting: &str, value: &str, low: u32, high: u32, unit: &str) -> Result<u32, Refusal> {
    let n: u32 = value.trim().parse().map_err(|_| {
        no_such_value(
            setting,
            value,
            &format!("It takes a whole number of {unit}."),
        )
    })?;
    if n < low || n > high {
        return Err(no_such_value(
            setting,
            value,
            &format!("It takes a whole number of {unit}, from {low} to {high}."),
        ));
    }
    Ok(n)
}

/// A governor word, or any other `[a-z0-9_-]` token the platform's own tool would accept.
fn word(setting: &str, value: &str) -> Result<String, Refusal> {
    let v = value.trim();
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(v.to_string())
    } else {
        Err(no_such_value(
            setting,
            value,
            "It takes one word — letters, digits, `-` and `_` — as the platform spells it.",
        ))
    }
}

// --- macOS -----------------------------------------------------------------

/// The macOS settings, as `defaults` and `pmset` spell them.
pub const MACOS_SETTINGS: &[Setting] = &[
    Setting {
        name: "key-repeat",
        unit: "units of 1/60 s (2 is about 30 ms between repeats)",
        needs_root: false,
        note: "read with `defaults read -g KeyRepeat`, written with `defaults write -g KeyRepeat -int`",
    },
    Setting {
        name: "key-repeat-delay",
        unit: "units of 1/60 s (15 is about 250 ms before the first repeat)",
        needs_root: false,
        note: "read with `defaults read -g InitialKeyRepeat`, written with `defaults write -g InitialKeyRepeat -int`",
    },
    Setting {
        name: "perf-policy",
        unit: "`0` (no low-power restriction) or `1` (low power)",
        needs_root: true,
        note: "read from `pmset -g`, written with `pmset -a lowpowermode`",
    },
];

/// macOS: two `defaults` keys and one `pmset` value.
pub struct MacOs {
    runner: Arc<dyn Runner>,
}

impl MacOs {
    pub fn new(runner: Arc<dyn Runner>) -> MacOs {
        MacOs { runner }
    }

    fn run(&self, argv: &[&str], env: &[(String, String)]) -> Result<Ran, Refusal> {
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        self.runner.run(&argv, env).map_err(|e| {
            Refusal::new(
                format!("`{}` could not be run", argv.join(" ")),
                format!(
                    "{e} — macOS's own tool for this setting is not here, so nothing was changed."
                ),
            )
        })
    }

    /// A `defaults` key: `Ok(None)` when the key was never set, which is a fact and not a failure.
    fn defaults(&self, key: &str) -> Result<Option<String>, Refusal> {
        let ran = self.run(&["defaults", "read", "-g", key], &[])?;
        if ran.exit == 0 {
            let v = ran.stdout.trim().to_string();
            return Ok(if v.is_empty() { None } else { Some(v) });
        }
        if ran.said().contains("does not exist") {
            return Ok(None);
        }
        Err(Refusal::new(
            format!("macOS would not report `{key}`"),
            format!(
                "`defaults read -g {key}` answered `{}` (exit {}). Nothing was changed.",
                ran.said(),
                ran.exit
            ),
        ))
    }

    fn write_default(&self, key: &str, value: u32) -> Result<Applied, Refusal> {
        let text = value.to_string();
        let ran = self.run(&["defaults", "write", "-g", key, "-int", &text], &[])?;
        if ran.exit != 0 {
            return Err(Refusal::new(
                format!("`defaults write -g {key} -int {value}` was refused"),
                format!("It answered `{}` (exit {}).", ran.said(), ran.exit),
            ));
        }
        Ok(Applied {
            ran: vec![format!("defaults write -g {key} -int {value}")],
        })
    }
}

impl Platform for MacOs {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn settings(&self) -> &'static [Setting] {
        MACOS_SETTINGS
    }

    fn read(&self, setting: &str) -> Result<Reading, Refusal> {
        let (key, how) = match setting {
            "key-repeat" => ("KeyRepeat", "`defaults read -g KeyRepeat`"),
            "key-repeat-delay" => ("InitialKeyRepeat", "`defaults read -g InitialKeyRepeat`"),
            "perf-policy" => return self.read_low_power(),
            asked => {
                return Err(no_such_setting(
                    "macos",
                    asked,
                    MACOS_SETTINGS,
                    elsewhere(asked),
                ));
            }
        };
        match self.defaults(key)? {
            Some(v) => Ok(Reading::of(v, how)),
            None => Ok(Reading::of("", how).noted(format!(
                "`{key}` is not set in this user's global defaults, so macOS's own default applies \
                 — there is no value to record as the `before` of a revert until one is written"
            ))),
        }
    }

    fn apply(&self, setting: &str, value: &str, privilege: &Privilege) -> Result<Applied, Refusal> {
        match setting {
            "key-repeat" => {
                let n = whole(setting, value, 1, 120, "1/60 s")?;
                self.write_default("KeyRepeat", n)
            }
            "key-repeat-delay" => {
                let n = whole(setting, value, 1, 10_000, "1/60 s")?;
                self.write_default("InitialKeyRepeat", n)
            }
            "perf-policy" => {
                let n = match value.trim() {
                    "0" | "1" => value.trim().to_string(),
                    other => {
                        return Err(no_such_value(
                            setting,
                            other,
                            "`pmset`'s `lowpowermode` takes `0` (no low-power restriction) or `1` \
                             (low power).",
                        ));
                    }
                };
                let argv = vec![
                    "pmset".to_string(),
                    "-a".to_string(),
                    "lowpowermode".to_string(),
                    n.clone(),
                ];
                let (argv, env) = privilege.as_root(&argv)?;
                let joined: Vec<&str> = argv.iter().map(String::as_str).collect();
                let ran = self.run(&joined, &env)?;
                if ran.exit != 0 {
                    return Err(Refusal::new(
                        format!("`pmset -a lowpowermode {n}` was refused"),
                        format!("It answered `{}` (exit {}).", ran.said(), ran.exit),
                    ));
                }
                Ok(Applied {
                    ran: vec![argv.join(" ")],
                })
            }
            other => Err(no_such_setting("macos", other, MACOS_SETTINGS, None)),
        }
    }
}

impl MacOs {
    fn read_low_power(&self) -> Result<Reading, Refusal> {
        let ran = self.run(&["pmset", "-g"], &[])?;
        if ran.exit != 0 {
            return Err(Refusal::new(
                "`pmset -g` would not answer",
                format!(
                    "It said `{}` (exit {}), so the perf policy could not be read. Nothing was \
                     changed.",
                    ran.said(),
                    ran.exit
                ),
            ));
        }
        let found = ran
            .stdout
            .lines()
            .find_map(|l| {
                let mut it = l.split_whitespace();
                (it.next() == Some("lowpowermode")).then(|| it.next().unwrap_or("").to_string())
            })
            .filter(|v| !v.is_empty());
        match found {
            Some(v) => Ok(Reading::of(v, "`pmset -g`")),
            None => Err(Refusal::new(
                "this macOS does not report `lowpowermode`",
                "`pmset -g` answered without that line — the setting arrived in macOS 12. On this \
                 build there is no perf policy to read, and nothing was changed.",
            )),
        }
    }
}

// --- Linux -----------------------------------------------------------------

/// The cpufreq governor's own file: one word, one line, one per CPU.
pub const GOVERNOR: &str = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor";

/// The governors this box offers — the file a value is checked against before anything runs.
pub const GOVERNORS: &str = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors";

/// The Linux settings, as X11 and cpufreq spell them.
pub const LINUX_SETTINGS: &[Setting] = &[
    Setting {
        name: "cpu-governor",
        unit: "a governor word (`powersave`, `performance`, `schedutil`, …)",
        needs_root: true,
        note: "read from `/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor`, written with `cpupower frequency-set -g`",
    },
    Setting {
        name: "key-repeat-delay",
        unit: "ms before the key starts repeating",
        needs_root: false,
        note: "read from `xset q`, written with `xset r rate <delay> <rate>`",
    },
    Setting {
        name: "key-repeat-rate",
        unit: "repeats per second",
        needs_root: false,
        note: "read from `xset q`, written with `xset r rate <delay> <rate>`",
    },
];

/// Linux: a cpufreq governor and X11's auto-repeat.
pub struct Linux {
    runner: Arc<dyn Runner>,
}

impl Linux {
    pub fn new(runner: Arc<dyn Runner>) -> Linux {
        Linux { runner }
    }

    fn run(&self, argv: &[&str], env: &[(String, String)]) -> Result<Ran, Refusal> {
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        self.runner.run(&argv, env).map_err(|e| {
            Refusal::new(
                format!("`{}` could not be run", argv.join(" ")),
                format!("{e} — nothing was changed."),
            )
        })
    }

    /// **X11's auto-repeat, which is one knob with two numbers.** `xset r rate <delay> <rate>`
    /// sets both at once, so a `set` of either one has to carry the other's current value — a
    /// `set` that wrote only its own half would zero the other.
    fn repeat(&self) -> Result<(String, String, String), Refusal> {
        let ran = self.run(&["xset", "q"], &[])?;
        if ran.exit != 0 {
            return Err(Refusal::new(
                "this session has no X display to ask about the keyboard",
                format!(
                    "`xset q` answered `{}` (exit {}). `$DISPLAY` is {}. X11's auto-repeat lives \
                     in the X server, so there is nothing to read here — on Hyprland the same two \
                     numbers are `hyprctl getoption input:repeat_delay` and `input:repeat_rate`, \
                     which this slice does not drive. Nothing was changed.",
                    ran.said(),
                    ran.exit,
                    match std::env::var("DISPLAY") {
                        Ok(d) if !d.is_empty() => format!("`{d}`"),
                        _ => "not set".to_string(),
                    }
                ),
            ));
        }
        let delay = after(&ran.stdout, "auto repeat delay:").ok_or_else(|| {
            Refusal::new(
                "`xset q` did not report an auto-repeat delay",
                "The answer had no `auto repeat delay:` line, so the keyboard's repeat could not \
                 be read. Nothing was changed.",
            )
        })?;
        let rate = after(&ran.stdout, "repeat rate:").ok_or_else(|| {
            Refusal::new(
                "`xset q` did not report a repeat rate",
                "The answer had no `repeat rate:` line, so the keyboard's repeat could not be \
                 read. Nothing was changed.",
            )
        })?;
        Ok((delay, rate, "`xset q`".to_string()))
    }

    fn set_repeat(&self, delay: u32, rate: u32) -> Result<Applied, Refusal> {
        let (d, r) = (delay.to_string(), rate.to_string());
        let ran = self.run(&["xset", "r", "rate", &d, &r], &[])?;
        if ran.exit != 0 {
            return Err(Refusal::new(
                format!("`xset r rate {d} {r}` was refused"),
                format!("It answered `{}` (exit {}).", ran.said(), ran.exit),
            ));
        }
        Ok(Applied {
            ran: vec![format!("xset r rate {d} {r}")],
        })
    }
}

/// The first integer after `key` in `text` — `auto repeat delay:  660    repeat rate:  25`.
fn after(text: &str, key: &str) -> Option<String> {
    let tail = text.split(key).nth(1)?;
    let digits: String = tail
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

impl Platform for Linux {
    fn name(&self) -> &'static str {
        "linux"
    }

    fn settings(&self) -> &'static [Setting] {
        LINUX_SETTINGS
    }

    fn read(&self, setting: &str) -> Result<Reading, Refusal> {
        match setting {
            "cpu-governor" => {
                let ran = self.run(&["cat", GOVERNOR], &[])?;
                let v = ran.stdout.trim().to_string();
                if ran.exit != 0 || v.is_empty() {
                    return Err(Refusal::new(
                        "this box has no cpufreq scaling governor",
                        format!(
                            "`cat {GOVERNOR}` answered `{}` (exit {}). A kernel with no cpufreq \
                             driver — a VM, or a CPU that does not expose one — has no governor to \
                             read, so nothing was changed.",
                            ran.said(),
                            ran.exit
                        ),
                    ));
                }
                Ok(Reading::of(v, format!("`cat {GOVERNOR}`")))
            }
            "key-repeat-delay" => {
                let (delay, _, how) = self.repeat()?;
                Ok(Reading::of(delay, how))
            }
            "key-repeat-rate" => {
                let (_, rate, how) = self.repeat()?;
                Ok(Reading::of(rate, how))
            }
            asked => Err(no_such_setting(
                "linux",
                asked,
                LINUX_SETTINGS,
                elsewhere(asked),
            )),
        }
    }

    fn apply(&self, setting: &str, value: &str, privilege: &Privilege) -> Result<Applied, Refusal> {
        match setting {
            "cpu-governor" => {
                let v = word(setting, value)?;
                // **Checked against what this box offers before anything runs**, so a word from
                // another kernel is refused here rather than by a tool that half-changed a CPU.
                let ran = self.run(&["cat", GOVERNORS], &[])?;
                let offered: Vec<&str> = ran.stdout.split_whitespace().collect();
                if ran.exit != 0 || offered.is_empty() {
                    return Err(Refusal::new(
                        "this box does not list the governors it offers",
                        format!(
                            "`cat {GOVERNORS}` answered `{}` (exit {}), so a value cannot be \
                             checked before it is applied. Nothing was changed.",
                            ran.said(),
                            ran.exit
                        ),
                    ));
                }
                if !offered.contains(&v.as_str()) {
                    return Err(no_such_value(
                        setting,
                        &v,
                        &format!("This box offers {}.", offered.join(", ")),
                    ));
                }
                let argv = vec![
                    "cpupower".to_string(),
                    "frequency-set".to_string(),
                    "-g".to_string(),
                    v.clone(),
                ];
                let (argv, env) = privilege.as_root(&argv)?;
                let joined: Vec<&str> = argv.iter().map(String::as_str).collect();
                let ran = self.run(&joined, &env)?;
                if ran.exit != 0 {
                    return Err(Refusal::new(
                        format!("`cpupower frequency-set -g {v}` was refused"),
                        format!(
                            "It answered `{}` (exit {}). The governor is still what it was; \
                             `cpupower` is the package `linux-tools`, and writing {GOVERNOR} with \
                             `sudo tee` does the same thing.",
                            ran.said(),
                            ran.exit
                        ),
                    ));
                }
                Ok(Applied {
                    ran: vec![argv.join(" ")],
                })
            }
            "key-repeat-delay" => {
                let n = whole(setting, value, 1, 10_000, "ms")?;
                let (_, rate, _) = self.repeat()?;
                let rate: u32 = rate.trim().parse().unwrap_or(25);
                self.set_repeat(n, rate)
            }
            "key-repeat-rate" => {
                let n = whole(setting, value, 1, 1000, "repeats per second")?;
                let (delay, _, _) = self.repeat()?;
                let delay: u32 = delay.trim().parse().unwrap_or(660);
                self.set_repeat(delay, n)
            }
            other => Err(no_such_setting("linux", other, LINUX_SETTINGS, None)),
        }
    }
}

// --- any other platform ----------------------------------------------------

/// **A platform this tool has no arm for.** Every setting is refused by name, with the two
/// vocabularies it does know, rather than the tool being absent from the seat: a seat that names
/// a tool which does not exist refuses to open at all.
pub struct Other {
    name: &'static str,
}

impl Other {
    /// The platform's own name, from `std::env::consts::OS`.
    pub fn new(name: &'static str) -> Other {
        Other { name }
    }

    fn refusal(&self) -> Refusal {
        Refusal::new(
            format!("`host_config` has no arm for {}", self.name),
            format!(
                "It knows macOS's `defaults` and `pmset` and Linux's `xset` and `cpupower`, and \
                 nothing on {}. Nothing was changed.",
                self.name
            ),
        )
    }
}

impl Platform for Other {
    fn name(&self) -> &'static str {
        self.name
    }

    fn settings(&self) -> &'static [Setting] {
        &[]
    }

    fn read(&self, _setting: &str) -> Result<Reading, Refusal> {
        Err(self.refusal())
    }

    fn apply(
        &self,
        _setting: &str,
        _value: &str,
        _privilege: &Privilege,
    ) -> Result<Applied, Refusal> {
        Err(self.refusal())
    }
}

/// **The name a caller may have used from the other platform**, when this one has the same idea
/// under its own word. It goes into a refusal, never into a translation: the two knobs have
/// different units and different values, so naming the neighbour is a lesson and mapping it would
/// be a lie.
fn elsewhere(asked: &str) -> Option<&'static str> {
    match asked {
        "perf-policy" => Some("cpu-governor"),
        "cpu-governor" => Some("perf-policy"),
        "key-repeat" => Some("key-repeat-rate"),
        "key-repeat-rate" => Some("key-repeat"),
        _ => None,
    }
}

/// **This box's platform**, chosen from `std::env::consts::OS`.
///
/// A function and not a `const`, so a test can hold the arms it is not running on.
pub fn this_box() -> Arc<dyn Platform> {
    let runner: Arc<dyn Runner> = Arc::new(System);
    match std::env::consts::OS {
        "macos" => Arc::new(MacOs::new(runner)),
        "linux" => Arc::new(Linux::new(runner)),
        other => Arc::new(Other::new(leak(other))),
    }
}

/// `Other` holds a `&'static str` so `Platform::name` stays infallible; the set of operating
/// systems is closed and tiny, so this is one allocation per process at most.
fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

// ---------------------------------------------------------------------------
// The tool
// ---------------------------------------------------------------------------

/// The `host_config` tool.
pub struct HostConfigTool {
    /// Where a change is recorded. `None` is a registry with no daemon: a change with no record
    /// is the state this tool exists to end, so `set` refuses there rather than applying one.
    scope: Option<Arc<dyn ChangelogScope>>,
    platform: Arc<dyn Platform>,
    /// `None` is [`Privilege::detect`] at the call — a test's door, because setting
    /// `$SUDO_ASKPASS` in a test would race every other test in the process.
    privilege: Option<Privilege>,
}

impl HostConfigTool {
    /// The daemon's own: this box's platform, the session's changelog, and whatever privilege
    /// this box can actually offer.
    pub fn new(scope: Arc<dyn ChangelogScope>) -> HostConfigTool {
        HostConfigTool {
            scope: Some(scope),
            platform: this_box(),
            privilege: None,
        }
    }

    /// **No changelog behind this tool.** `read` and `settings` still answer — they change
    /// nothing — and `set` refuses before it runs anything.
    pub fn nowhere() -> HostConfigTool {
        HostConfigTool {
            scope: None,
            platform: this_box(),
            privilege: None,
        }
    }

    /// Drive another platform — a test's door, and the only way the arm this box is not is ever
    /// measured.
    pub fn with_platform(mut self, platform: Arc<dyn Platform>) -> HostConfigTool {
        self.platform = platform;
        self
    }

    /// Stand in for what this box can offer — a test's door for the privilege refusal.
    pub fn with_privilege(mut self, privilege: Privilege) -> HostConfigTool {
        self.privilege = Some(privilege);
        self
    }
}

impl Tool for HostConfigTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "This machine's own settings, in the platform's own vocabulary: keyboard repeat and \
             perf policy, read and written where the platform keeps them. `settings` lists what \
             this platform has, with the current value and whether it needs root; `read` gives \
             one setting's value; `set` changes one and records it in the host changelog. A \
             setting this platform has not got is refused by name — macOS's perf knob is \
             `perf-policy`, Linux's is `cpu-governor`, and they are not interchangeable. `set` \
             reads the current value first, so the record's `before` is real, and needs `why` and \
             `words` before it changes anything. Undoing a change is `host_changelog revert N`, \
             which hands back the `before` this tool recorded.",
            json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": ["settings", "read", "set"],
                        "description": "`settings` — every setting this platform has, with its value and whether it needs root; `read` — one setting's current value (needs `setting`); `set` — change one (needs `setting`, `value`, `why`, `words`)."
                    },
                    "setting": {
                        "type": "string",
                        "description": "The setting, in this platform's words. macOS: `key-repeat`, `key-repeat-delay`, `perf-policy`. Linux: `key-repeat-rate`, `key-repeat-delay`, `cpu-governor`. `settings` prints the list with each one's unit."
                    },
                    "value": {
                        "type": "string",
                        "description": "For `set`: the new value, in the setting's own unit — an integer for the keyboard ones, a governor word for `cpu-governor`, `0`/`1` for `perf-policy`. A value the platform would not take is refused before anything runs."
                    },
                    "why": {
                        "type": "string",
                        "description": "For `set`: the problem the change solves. Required, and checked before anything runs — a change with no reason is what the changelog exists to end."
                    },
                    "words": {
                        "type": "string",
                        "description": "For `set`: whose words it rests on, quoted — the operator's own sentence. Required, for the same reason."
                    }
                },
                "required": ["verb"]
            }),
            // `set` runs the platform's own tool — `cpupower`, `pmset` — so the gate decides.
            // `settings` and `read` narrow to a read below: asking what the keyboard repeat is
            // should not cost the operator a decision.
            Access::Exec,
        )
    }

    fn access_for(&self, args: &Value) -> Option<Access> {
        match verb_of(args).as_str() {
            "settings" | "read" => Some(Access::Read),
            _ => None,
        }
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let privilege = self.privilege.clone().unwrap_or_else(Privilege::detect);
        // The second gate, below the adjudicator's: a change to this box from a session opened
        // read-only is the one thing the backend's own answer must stop.
        if verb_of(args) == "set" && !ctx.backend.is_writable() {
            return Invocation::failed(
                "this session's backend was opened read-only",
                format!(
                    "so no host setting may be changed from it ({}). That is a second, \
                     independent gate below the adjudication one. Nothing was changed.",
                    ctx.backend.describe()
                ),
            );
        }
        match dispatch(
            self.scope.as_deref(),
            self.platform.as_ref(),
            &privilege,
            args,
            now_ms(),
        ) {
            Ok(said) => Invocation::ok(said),
            Err(failure) => failure,
        }
    }
}

/// **The tool's whole decision, with no context** — what the tests drive.
///
/// Split out of [`Tool::invoke`] for the reason `host_changelog`'s is: what this tool guarantees
/// (a change with no `why` never runs, a record whose `before` is a read, a setting this platform
/// has not got refused by name) is its contract, and a clock a test can choose is what makes the
/// record's age assertable.
pub fn dispatch(
    scope: Option<&dyn ChangelogScope>,
    platform: &dyn Platform,
    privilege: &Privilege,
    args: &Value,
    now_ms: u64,
) -> Result<String, Invocation> {
    match verb_of(args).as_str() {
        "settings" => settings(platform),
        "read" => read(platform, args),
        "set" => set(scope, platform, privilege, args, now_ms),
        "" => Err(Invocation::failed(
            "no verb",
            "the `host_config` tool needs a `verb`: `settings` for what this platform has, `read` \
             one setting's value, or `set` one. Nothing was changed.",
        )),
        other => Err(Invocation::failed(
            format!("unknown `host_config` verb `{other}`"),
            "the verbs are `settings`, `read` and `set`. Nothing was changed.",
        )),
    }
}

/// **What this platform has**, with each value read now — the answer to *which knob is it here*.
fn settings(platform: &dyn Platform) -> Result<String, Invocation> {
    let settings = platform.settings();
    if settings.is_empty() {
        return Err(Invocation::failed(
            format!("`host_config` has no arm for {}", platform.name()),
            "It knows macOS's `defaults` and `pmset` and Linux's `xset` and `cpupower`, and \
             nothing on this platform. Nothing was changed.",
        ));
    }
    let mut out = format!(
        "{} — {} setting(s), in this platform's own vocabulary:\n",
        platform.name(),
        settings.len()
    );
    for s in settings {
        let value = match platform.read(s.name) {
            Ok(r) => show(&r),
            Err(r) => format!("(unreadable: {})", r.what),
        };
        out.push_str(&format!(
            "  {} = {}\n      {}; {}\n      {}\n",
            s.name,
            value,
            s.unit,
            if s.needs_root {
                "needs root"
            } else {
                "no root needed"
            },
            s.note
        ));
    }
    out.push_str(&format!(
        "A setting this platform has not got is refused by name rather than mapped: macOS's perf \
         knob is `perf-policy`, Linux's is `cpu-governor`. `set` records every change in the host \
         changelog (`host_changelog list`), and `host_changelog revert N` hands back the value to \
         put back."
    ));
    Ok(out)
}

fn read(platform: &dyn Platform, args: &Value) -> Result<String, Invocation> {
    let Some(setting) = text(args, "setting") else {
        return Err(Invocation::failed(
            "`read` needs `setting`",
            format!(
                "`settings` prints what {} has, with each one's current value. Nothing was \
                 changed.",
                platform.name()
            ),
        ));
    };
    let s = find(platform, &setting)?;
    let r = platform.read(s.name).map_err(Invocation::from)?;
    Ok(format!(
        "{}'s `{}` = {}\n  {}; {}\n  read with {}{}",
        platform.name(),
        s.name,
        show(&r),
        s.unit,
        if s.needs_root {
            "needs root to change"
        } else {
            "no root needed"
        },
        r.how,
        if r.note.is_empty() {
            String::new()
        } else {
            format!("\n  {}", r.note)
        }
    ))
}

/// **The three acts**: read, apply, record — and the checks that come before any of them.
fn set(
    scope: Option<&dyn ChangelogScope>,
    platform: &dyn Platform,
    privilege: &Privilege,
    args: &Value,
    now_ms: u64,
) -> Result<String, Invocation> {
    // 1. The five facts, checked before a single command runs. `why` and `words` are the
    //    feature: a change nobody can explain a month later is the complaint this answers.
    let mut missing = Vec::new();
    for field in ["setting", "value", "why", "words"] {
        if text(args, field).is_none_or(|v| v.is_empty()) {
            missing.push(field);
        }
    }
    if !missing.is_empty() {
        return Err(needs("set", &missing));
    }
    let asked = text(args, "setting").unwrap_or_default();
    let value = text(args, "value").unwrap_or_default();
    let why = text(args, "why").unwrap_or_default();
    let words = text(args, "words").unwrap_or_default();
    let s = find(platform, &asked)?;

    // 2. The record's door, proved open before the change: a change that lands with no line in
    //    the file is exactly the state the changelog exists to end.
    let Some(scope) = scope else {
        return Err(Invocation::failed(
            "this session has no host changelog",
            "nothing told the tool where this box keeps its config, so a change here would be \
             one no record would ever see — which is the state this tool exists to end. Nothing \
             was changed.",
        ));
    };
    let path = scope.path();
    changelog::writable(&path).map_err(|e| {
        Invocation::failed(
            format!("the host changelog cannot be written: {e}"),
            format!(
                "{} could not be opened for a line, so nothing was changed: a change with no \
                 record is worse than no change.",
                path.display()
            ),
        )
    })?;

    // 3. **The `before` is a read.** A setting that cannot be read now cannot be reverted later,
    //    and an empty `before` would claim there was no such setting.
    let before = platform.read(s.name).map_err(|r| {
        Invocation::failed(
            format!("`{}` could not be read, so nothing was changed", s.name),
            r.why,
        )
    })?;

    // 4. The apply. Every refusal below here leaves the box as it was, and says so.
    let applied = platform
        .apply(s.name, &value, privilege)
        .map_err(Invocation::from)?;

    // 5. **What the box says now**, not what was asked for: the record's `after` has to be the
    //    truth, or a month later the file argues with the machine.
    let after = match platform.read(s.name) {
        Ok(r) => r,
        Err(r) => Reading::of(value.clone(), r.what).noted(
            "the box did not answer a read after the change, so this is the value that was asked \
             for and not one that was measured",
        ),
    };

    // 6. The record — the changelog's own `record` verb, so one function writes a line.
    let record = json!({
        "verb": "record",
        "what": s.name,
        "before": before.value.clone(),
        "after": after.value.clone(),
        "why": why,
        "words": words,
    });
    let recorded = changelog::dispatch(scope, &record, now_ms).map_err(|f| {
        Invocation::failed(
            "the change was applied and the record did not land",
            format!(
                "{} has been changed to `{}`, and {} could not be written. Record it by hand with \
                 `host_changelog record {{verb: \"record\", what: \"{}\", before: \"{}\", after: \
                 \"{}\", why: \"…\", words: \"…\"}}` — a change with no line in that file is the \
                 state this tool exists to end.",
                s.name,
                after.value,
                path.display(),
                s.name,
                before.value,
                after.value,
            ),
        )
    })?;

    let took = if after.value.trim() == value.trim() {
        format!("the box now reads back `{}`", after.value)
    } else {
        format!(
            "**the box still reads back `{}`, not `{}`** — the command exited 0 and the setting \
             did not move, so the record says what the machine says",
            after.value, value
        )
    };
    Ok(format!(
        "applied on {}: `{}` {} → {}\n  ran: {}\n  {}{}\n\n{}",
        platform.name(),
        s.name,
        if before.value.is_empty() {
            "(not set)".to_string()
        } else {
            format!("`{}`", before.value)
        },
        if after.value.is_empty() {
            "(not set)".to_string()
        } else {
            format!("`{}`", after.value)
        },
        applied.ran.join("; "),
        took,
        if after.note.is_empty() {
            String::new()
        } else {
            format!("\n  {}", after.note)
        },
        recorded,
    ))
}

/// The setting, by the name the caller used — with the platform's own refusal when it has none.
fn find<'a>(platform: &'a dyn Platform, asked: &str) -> Result<&'a Setting, Invocation> {
    let wanted = asked.trim();
    if let Some(s) = platform.settings().iter().find(|s| s.name == wanted) {
        return Ok(s);
    }
    // The platform's own `read` knows the near names and what this platform has instead; asking
    // it is how the refusal stays one sentence rather than two that can drift.
    match platform.read(wanted) {
        Err(r) => Err(Invocation::failed(r.what, r.why)),
        // A platform that answered a name it does not list is a bug, and saying so beats
        // inventing a setting.
        Ok(_) => Err(Invocation::failed(
            format!("`{wanted}` is not one of {}'s settings", platform.name()),
            format!(
                "This platform has {}. Nothing was changed.",
                platform
                    .settings()
                    .iter()
                    .map(|s| format!("`{}`", s.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// A value as it is printed: backticked, or `(not set)` when there is nothing there.
fn show(r: &Reading) -> String {
    if r.value.trim().is_empty() {
        "(not set)".to_string()
    } else {
        format!("`{}`", r.value)
    }
}

/// The refusal for a verb missing its fields, naming every one of them.
fn needs(verb: &str, fields: &[&str]) -> Invocation {
    let names: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
    let listed = match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        _ => format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names[names.len() - 1]
        ),
    };
    Invocation::failed(
        format!("`{verb}` needs {listed}"),
        "`why` is the problem the change solves and `words` is the operator's own sentence, \
         quoted — a month later those two are the answer to *why is this setting like this?*. \
         Nothing was changed: the checks run before the first command does."
            .to_string(),
    )
}

fn verb_of(args: &Value) -> String {
    args.get("verb")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn text(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn now_ms() -> u64 {
    u64::try_from(letibot_tokencore::store::Store::lease_now_ms()).unwrap_or(0)
}

#[cfg(test)]
mod tests;
