//! **How much memory a tool call may hold** — the policy, the box's numbers, and the decision.
//! `TODO.md` R64 and R65.
//!
//! The operator's report that started it: *"why my pg-noop session died and took byobu window
//! with it?"* A database server a subagent started in the background grew to 77 GB, the box
//! ran out, the kernel killed it — and because the daemon and every process its tools start
//! lived in the tmux window's systemd scope, systemd stopped the whole window. Four times in
//! seven hours, measured in the journal.
//!
//! The kernel half is in [`super::scope`]: a job's cgroup can carry `memory.max`, and a job
//! that reaches it dies alone. This module is the half that decides the number, and the rule
//! is the operator's, in their words:
//!
//! > *"I think model should try to decide, so it should know the free, how much it can ask
//! > and what are the consequences"*
//!
//! > *"default memory either as absolute or a fraction is a config option with 64mb being a
//! > good default"*
//!
//! > *"ceiling is a config too, with both absolute and fraction and the max is uncapped but
//! > if model wants it - must ask me. unaskable ask is 50%."*
//!
//! So there are three outcomes and no fourth: the **default** when the model names nothing,
//! a **grant** when it names an amount up to the ceiling, and **the operator's card** for
//! anything above — including no cap at all. Nothing is clipped silently: an ask is either
//! granted as asked or put to a person.
//!
//! Pure: no process, no `/proc` read except [`Meminfo::read`], so the rule is tested as a rule.

use std::fmt;

/// **An amount of memory as the operator or the model writes it** — absolute (`64M`, `2G`,
/// `1048576`) or a fraction of what is available when the call is made (`0.5`, `50%`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Amount {
    Bytes(u64),
    /// Of `MemAvailable` at call time, in `(0, 1]`.
    Fraction(f64),
}

impl Amount {
    /// Parse what a person or a model wrote. Units are binary (`M` is MiB), because
    /// `memory.max` is read back in bytes and `64M` must be the number that comes back.
    pub fn parse(text: &str) -> Result<Amount, String> {
        let t = text.trim();
        let bad = || {
            format!(
                "`{text}` is not an amount of memory: write a size (`64M`, `2G`, `512K`, \
                 bytes) or a fraction of what is free (`0.5`, `50%`)"
            )
        };
        if t.is_empty() {
            return Err(bad());
        }
        if let Some(p) = t.strip_suffix('%') {
            let f: f64 = p.trim().parse().map_err(|_| bad())?;
            return Amount::fraction(f / 100.0).ok_or_else(bad);
        }
        if t.contains('.') {
            let f: f64 = t.parse().map_err(|_| bad())?;
            return Amount::fraction(f).ok_or_else(bad);
        }
        let lower = t.to_ascii_lowercase();
        let digits_end = lower
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(lower.len());
        let (num, unit) = lower.split_at(digits_end);
        let n: u64 = num.parse().map_err(|_| bad())?;
        let shift = match unit.trim() {
            "" | "b" => 0,
            "k" | "kb" | "kib" => 10,
            "m" | "mb" | "mib" => 20,
            "g" | "gb" | "gib" => 30,
            "t" | "tb" | "tib" => 40,
            _ => return Err(bad()),
        };
        n.checked_mul(1u64 << shift)
            .filter(|b| *b > 0)
            .map(Amount::Bytes)
            .ok_or_else(bad)
    }

    fn fraction(f: f64) -> Option<Amount> {
        (f > 0.0 && f <= 1.0).then_some(Amount::Fraction(f))
    }

    /// The bytes this amount means when `available` bytes are free.
    pub fn resolve(&self, available: u64) -> u64 {
        match *self {
            Amount::Bytes(b) => b,
            Amount::Fraction(f) => (available as f64 * f) as u64,
        }
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Amount::Bytes(b) => write!(f, "{}", super::scope::human_bytes(b)),
            Amount::Fraction(x) => write!(f, "{}% of what is free", (x * 100.0).round()),
        }
    }
}

/// **The operator's two numbers**, from `providers.toml`'s `[memory]` section — and, over it,
/// the session's project's `leticode.toml` (TODO.md R76 item 7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryPolicy {
    /// What a call that names no amount runs under. 64 MiB out of the box.
    pub default: Amount,
    /// The most a call may ask for without the operator. Half of what is free out of the box.
    pub ceiling: Amount,
    /// **Which file set [`MemoryPolicy::default`]**, so the description can say where the
    /// number in force came from and a model killed at it is pointed at the line to change.
    pub default_from: PolicySource,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        MemoryPolicy {
            default: Amount::Bytes(64 << 20),
            ceiling: Amount::Fraction(0.5),
            default_from: PolicySource::BuiltIn,
        }
    }
}

/// **Where a `[memory]` number came from** — the four levels, lowest first. The project's
/// `leticode.toml` beats the user's, which beats the box-wide `providers.toml`, which beats
/// the built-in; the same precedence every other `leticode.toml` key has.
///
/// It exists because of the operator's *"when ran via tools cargo on that project dies with
/// 64mb cap"*: a Rust project needs gigabytes for a build, the box-wide default is right for
/// everything else, and the place to say *this project builds* is the project's own file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PolicySource {
    #[default]
    BuiltIn,
    /// `~/.config/letibot/providers.toml`, `[memory]`.
    Box,
    /// `~/.config/letibot/leticode.toml`, `[memory]`.
    User,
    /// The nearest `leticode.toml` at or above the session's workspace, `[memory]`.
    Project,
}

impl PolicySource {
    /// The level as the description and the kill note name it.
    pub fn describe(self) -> &'static str {
        match self {
            PolicySource::BuiltIn => "the built-in default",
            PolicySource::Box => "`[memory] default` in the box-wide providers.toml",
            PolicySource::User => "`[memory] default` in your ~/.config/letibot/leticode.toml",
            PolicySource::Project => "`[memory] default` in this project's leticode.toml",
        }
    }
}

/// **The least a killed call is told to ask for next time** — 2 GiB.
const ASK_FLOOR: u64 = 2 << 30;
/// **How many times the cap it died at.** A call killed at its cap says one thing about what
/// it needs — MORE than the cap — and nothing about how much more. A cap is a limit and not a
/// reservation, so a generous ask costs nothing until it is used, while a timid one costs
/// another killed build; so the suggestion is a big step (64 MiB → 2 GiB, 2 GiB → 32 GiB)
/// rather than a doubling that would take five kills to reach a `cargo test`.
const ASK_STEP: u64 = 16;

/// An amount as a model writes it back in `memory`: `2G` for whole GiB, else `M`.
pub fn ask_text(bytes: u64) -> String {
    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;
    if bytes >= G && bytes.is_multiple_of(G) {
        format!("{}G", bytes / G)
    } else {
        format!("{}M", bytes.div_ceil(M))
    }
}

/// **What a call killed at `cap` should ask for next time**, from the cap it died at and the
/// most it may take without the operator — said in the result, so the model's next call
/// is sized by a number and not by a guess (TODO.md R76 item 7).
///
/// `peaked at its 64 MiB cap — ask for memory: "2G" next time, or set `[memory] default` in
/// this project's leticode.toml so every call in it starts there`. When the step would pass
/// the ceiling, the ceiling is what is offered, and the sentence says what lies beyond it.
pub fn ask_after_kill(policy: &MemoryPolicy, cap: u64, mem: Option<&Meminfo>) -> String {
    use super::scope::human_bytes;
    const G: u64 = 1 << 30;
    let step = cap.saturating_mul(ASK_STEP).max(ASK_FLOOR).div_ceil(G) * G;
    let ceiling = mem.map(|m| policy.ceiling.resolve(m.available));
    let ask = match ceiling {
        Some(c) if step > c && c > cap => format!(
            "ask for memory: \"{}\" (the most granted without asking; more is put to the \
             operator)",
            ask_text(c)
        ),
        _ => format!("ask for memory: \"{}\"", ask_text(step)),
    };
    format!(
        "peaked at its {} cap — {ask} next time, or set `[memory] default` in this project's \
         leticode.toml so every call in it starts there (the default in force is {}, from {})",
        human_bytes(cap),
        policy.default,
        policy.default_from.describe()
    )
}

/// The box's memory, from `/proc/meminfo`, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meminfo {
    pub total: u64,
    /// `MemAvailable` — what the kernel says can be handed out without swapping.
    pub available: u64,
    pub swap_free: u64,
}

impl Meminfo {
    pub fn read() -> Option<Meminfo> {
        Meminfo::parse(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }

    pub fn parse(text: &str) -> Option<Meminfo> {
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
                .map(|kb| kb * 1024)
        };
        Some(Meminfo {
            total: field("MemTotal")?,
            available: field("MemAvailable")?,
            swap_free: field("SwapFree").unwrap_or(0),
        })
    }

    /// One line for a tool result: what is free and what may be asked for without asking.
    pub fn clause(&self, policy: &MemoryPolicy) -> String {
        use super::scope::human_bytes;
        format!(
            "box: {} free of {}; you may ask up to {} without asking the operator",
            human_bytes(self.available),
            human_bytes(self.total),
            human_bytes(policy.ceiling.resolve(self.available)),
        )
    }
}

/// **What the model asked for** in the call's `memory` argument.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ask {
    /// It named nothing: the default applies.
    Default,
    Amount(Amount),
    /// `uncapped` — legal, and always the operator's to grant.
    Uncapped,
}

impl Ask {
    pub fn parse(arg: Option<&str>) -> Result<Ask, String> {
        match arg.map(str::trim) {
            None | Some("") => Ok(Ask::Default),
            Some(s) if s.eq_ignore_ascii_case("uncapped") => Ok(Ask::Uncapped),
            Some(s) => Amount::parse(s).map(Ask::Amount),
        }
    }
}

/// **What happens to the ask.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run under this cap. No one is asked.
    Grant(u64),
    /// Above the ceiling: the operator decides. `bytes` is `None` for *uncapped*.
    Operator { bytes: Option<u64>, ceiling: u64 },
}

/// **The rule, in one place.** The default is the operator's own number and is granted
/// whatever the ceiling says — they set both. An amount up to the ceiling is granted as asked.
/// Anything above it, and `uncapped`, goes to the operator.
pub fn decide(policy: &MemoryPolicy, ask: Ask, mem: &Meminfo) -> Decision {
    let ceiling = policy.ceiling.resolve(mem.available);
    match ask {
        Ask::Default => Decision::Grant(policy.default.resolve(mem.available)),
        Ask::Amount(a) => {
            let b = a.resolve(mem.available);
            if b <= ceiling {
                Decision::Grant(b)
            } else {
                Decision::Operator {
                    bytes: Some(b),
                    ceiling,
                }
            }
        }
        Ask::Uncapped => Decision::Operator {
            bytes: None,
            ceiling,
        },
    }
}

/// **The question put to the operator** when an ask is above the ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAsk {
    pub command: String,
    /// `None` is *uncapped*.
    pub bytes: Option<u64>,
    pub ceiling: u64,
    pub available: u64,
    pub total: u64,
}

impl MemoryAsk {
    /// The card's one line.
    pub fn summary(&self) -> String {
        use super::scope::human_bytes;
        let what = match self.bytes {
            Some(b) => format!("{} of memory", human_bytes(b)),
            None => "NO memory cap".to_string(),
        };
        format!(
            "the model asks for {what} for `{}` — above the {} it may take without asking; \
             {} of {} is free now",
            self.command,
            human_bytes(self.ceiling),
            human_bytes(self.available),
            human_bytes(self.total),
        )
    }
}

/// The operator's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGrant {
    Granted,
    Refused(String),
    /// Nobody could be asked — a gate with no person behind it. The call does not run.
    NotAsked,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn mem(avail_gib: u64) -> Meminfo {
        Meminfo {
            total: 185 * GIB,
            available: avail_gib * GIB,
            swap_free: 0,
        }
    }

    #[test]
    fn amounts_parse_as_sizes_or_fractions_and_refuse_everything_else() {
        assert_eq!(Amount::parse("64M"), Ok(Amount::Bytes(64 << 20)));
        assert_eq!(Amount::parse("2G"), Ok(Amount::Bytes(2 * GIB)));
        assert_eq!(Amount::parse("2GiB"), Ok(Amount::Bytes(2 * GIB)));
        assert_eq!(Amount::parse("512k"), Ok(Amount::Bytes(512 << 10)));
        assert_eq!(Amount::parse("1048576"), Ok(Amount::Bytes(1 << 20)));
        assert_eq!(Amount::parse("0.5"), Ok(Amount::Fraction(0.5)));
        assert_eq!(Amount::parse("50%"), Ok(Amount::Fraction(0.5)));
        for bad in ["", "lots", "0", "0.0", "1.5", "150%", "64Q", "-1G"] {
            assert!(Amount::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// The operator's three outcomes, and that nothing is clipped: a granted ask is granted
    /// as asked, and anything else is a question rather than a smaller number.
    #[test]
    fn the_default_is_granted_an_ask_up_to_the_ceiling_is_granted_and_anything_above_is_the_operators()
     {
        let p = MemoryPolicy::default();
        let m = mem(100);
        assert_eq!(decide(&p, Ask::Default, &m), Decision::Grant(64 << 20));
        assert_eq!(
            decide(&p, Ask::Amount(Amount::Bytes(50 * GIB)), &m),
            Decision::Grant(50 * GIB)
        );
        assert_eq!(
            decide(&p, Ask::Amount(Amount::Bytes(51 * GIB)), &m),
            Decision::Operator {
                bytes: Some(51 * GIB),
                ceiling: 50 * GIB
            }
        );
        assert_eq!(
            decide(&p, Ask::Uncapped, &m),
            Decision::Operator {
                bytes: None,
                ceiling: 50 * GIB
            }
        );
    }

    /// The ceiling follows what is free: the same ask is granted on an idle box and goes to
    /// the operator on a busy one.
    #[test]
    fn a_fractional_ceiling_moves_with_what_is_free() {
        let p = MemoryPolicy::default();
        let ask = Ask::Amount(Amount::Bytes(40 * GIB));
        assert_eq!(decide(&p, ask, &mem(120)), Decision::Grant(40 * GIB));
        assert!(matches!(
            decide(&p, ask, &mem(60)),
            Decision::Operator { .. }
        ));
    }

    #[test]
    fn meminfo_is_read_in_bytes() {
        let m = Meminfo::parse(
            "MemTotal:       194000000 kB\nMemFree:  1 kB\nMemAvailable:   135000000 kB\nSwapFree: 228 kB\n",
        )
        .unwrap();
        assert_eq!(m.total, 194_000_000 * 1024);
        assert_eq!(m.available, 135_000_000 * 1024);
        assert_eq!(m.swap_free, 228 * 1024);
    }

    /// **The note a killed call carries names an ask big enough to matter** — the operator's
    /// cargo killed at 64 MiB is told `2G`, not `128M` — and the ceiling, when the step would
    /// pass it.
    #[test]
    fn a_kill_at_the_cap_says_what_to_ask_for_next_and_where_the_default_lives() {
        let p = MemoryPolicy::default();
        let note = ask_after_kill(&p, 64 << 20, Some(&mem(100)));
        assert!(note.contains("peaked at its 64 MiB cap"), "{note}");
        assert!(note.contains("memory: \"2G\""), "{note}");
        assert!(note.contains("this project's leticode.toml"), "{note}");
        assert!(note.contains("64 MiB, from the built-in default"), "{note}");
        // 2 GiB killed → 32 GiB, granted at once on a box with 100 GiB free.
        assert!(ask_after_kill(&p, 2 * GIB, Some(&mem(100))).contains("\"32G\""));
        // On a box with 20 GiB free the ceiling is 10 GiB, and that is what is offered.
        let tight = ask_after_kill(&p, 2 * GIB, Some(&mem(20)));
        assert!(
            tight.contains("\"10G\"") && tight.contains("operator"),
            "{tight}"
        );
        assert_eq!(ask_text(512 << 20), "512M");
        assert_eq!(ask_text(3 * GIB), "3G");
    }

    #[test]
    fn uncapped_is_an_ask_and_a_blank_is_the_default() {
        assert_eq!(Ask::parse(None), Ok(Ask::Default));
        assert_eq!(Ask::parse(Some("  ")), Ok(Ask::Default));
        assert_eq!(Ask::parse(Some("Uncapped")), Ok(Ask::Uncapped));
        assert_eq!(
            Ask::parse(Some("4G")),
            Ok(Ask::Amount(Amount::Bytes(4 * GIB)))
        );
    }
}
