//! How much memory a tool call may hold: policy, the box's numbers, and the decision.
//! [`super::scope`] puts `memory.max` on a job's cgroup; this module decides the number, purely.
//! Three outcomes: the default, a grant up to the ceiling, a question.

use std::fmt;

/// An amount of memory as a person or a model writes it: absolute (`64M`, `2G`,
/// `1048576`) or a fraction of what is free when the call is made (`0.5`, `50%`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Amount {
    Bytes(u64),
    /// Of `MemAvailable` at call time, in `(0, 1]`.
    Fraction(f64),
}

impl Amount {
    /// Parse what a person or a model wrote. Units are binary (`M` is MiB) because
    /// `memory.max` is read back in bytes: `64M` must be the number that comes back.
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

/// The operator's two numbers, from `providers.toml`'s `[memory]` section, overridden
/// per session by the project's `leticode.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryPolicy {
    /// What a call that names no amount runs under. 64 MiB out of the box.
    pub default: Amount,
    /// The most a call may ask for without the operator. Half of what is free out of the box.
    pub ceiling: Amount,
    /// Which file set [`MemoryPolicy::default`], so the description can say where the
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

/// Where a `[memory]` number came from: the four levels, lowest first. The project's
/// `leticode.toml` beats the user's, which beats the box-wide `providers.toml`, which
/// beats the built-in; the same precedence every other `leticode.toml` key has.
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

/// The least a killed call is told to ask for next time: 2 GiB.
const ASK_FLOOR: u64 = 2 << 30;
/// The step over the cap it died at. A kill says only that the cap was too small, so the
/// suggestion steps big: a cap is a limit and not a reservation, and a timid ask costs
/// another killed build.
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

/// What a call killed at `cap` should ask for next time, said in the result so the model's
/// next call is sized by a number and not by a guess. `mem` sizes the answer to what the box may
/// hand over without asking, so the next call starts instead of asking.
pub fn ask_after_kill(cap: u64, mem: Option<&Meminfo>) -> String {
    const G: u64 = 1 << 30;
    let step = cap.saturating_mul(ASK_STEP).max(ASK_FLOOR).div_ceil(G) * G;
    // The ceiling is the operator's number and is not named: it sizes the suggestion and no more.
    let step = match mem {
        Some(m) => step.min(Amount::Fraction(0.5).resolve(m.available).max(cap)),
        None => step,
    };
    format!("Run it again with memory: \"{}\".", ask_text(step))
}

/// The box's memory, from `/proc/meminfo`, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meminfo {
    pub total: u64,
    /// `MemAvailable`: what the kernel says can be handed out without swapping.
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

    /// One line for the model choosing a cap: what is free now, up to what an ask is granted
    /// without asking, and what this daemon's live work already promises. Composed per round
    /// rather than put in a schema, because a schema is the session's stable prefix.
    pub fn round_clause(&self, policy: &MemoryPolicy, holds: Holds) -> String {
        use super::scope::human_bytes;
        let mut line = format!(
            "{} of {} is free (swap {}); an ask up to {} is granted at once",
            human_bytes(self.available),
            human_bytes(self.total),
            human_bytes(self.swap_free),
            human_bytes(policy.ceiling.resolve(self.available)),
        );
        if holds.scopes > 0 {
            line.push_str(&format!(
                "; this daemon's live calls and jobs hold {} of {} promised across {} scope(s)",
                human_bytes(holds.peaks),
                human_bytes(holds.promised),
                holds.scopes
            ));
        }
        line.push('.');
        line
    }
}

/// The machine a command's cap is decided against: this box, or a placement and what its
/// `/proc/meminfo` last said. Two spellings rather than one optional number, because *this box has
/// no controller* (macOS) runs a command uncapped while *a placement did not answer* has an answer
/// nobody here can give — a cap sized from this box's figures is the fault this type exists to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Machine {
    /// This box. `None` is a platform with no `/proc/meminfo`.
    Here(Option<Meminfo>),
    /// A placement: the host, as a card names it, and its memory when it answered.
    There { host: String, mem: Option<Meminfo> },
}

impl Machine {
    /// This box, which is where a session that was not placed runs.
    pub fn here() -> Machine {
        Machine::Here(Meminfo::read())
    }

    /// What was read off it, or `None` when nothing was.
    pub fn numbers(&self) -> Option<Meminfo> {
        match self {
            Machine::Here(m) | Machine::There { mem: m, .. } => *m,
        }
    }

    /// The machine's name when a card must say it, and `None` for this box.
    pub fn host(&self) -> Option<&str> {
        match self {
            Machine::Here(_) => None,
            Machine::There { host, .. } => Some(host),
        }
    }
}

/// What a daemon's own live work already promises: how many scopes it holds that carry a cap, the
/// caps added up, and the most they have held. Read because a ceiling of a fraction of what is free
/// does not see a grant nobody has used yet.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Holds {
    pub scopes: usize,
    pub promised: u64,
    pub peaks: u64,
}

impl Holds {
    /// Read off the scopes a host holds open, counting only those that answer: a scope on a tree
    /// with no memory controller, or one already removed, is absence and not a measured zero.
    pub fn of(host: &dyn super::host::ProcessHost) -> Holds {
        let mut holds = Holds::default();
        for scope in host.scopes() {
            let Some(used) = super::scope::memory_use_of(&scope) else {
                continue;
            };
            holds.scopes += 1;
            holds.promised += used.cap.unwrap_or(0);
            holds.peaks += used.peak;
        }
        holds
    }
}

/// What the model asked for in the call's `memory` argument.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ask {
    /// It named nothing: the default applies.
    Default,
    Amount(Amount),
    /// `uncapped`: legal, and always the operator's to grant.
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

/// What happens to the ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run with no cap and ask nobody: this box has no memory controller to hold one.
    Uncapped,
    /// Run under this cap. No one is asked.
    Grant(u64),
    /// The operator decides. `bytes` is `None` for uncapped, and `ceiling` is `None` when the
    /// machine's memory was not read — which is what makes an ordinary size their question.
    Operator {
        bytes: Option<u64>,
        ceiling: Option<u64>,
    },
    /// Nothing can be decided and nobody can be asked a number: the ask is a fraction of what is
    /// free and the machine's memory was not read. The sentence says what to write instead.
    Unresolved(String),
}

/// The rule, in one place. The default is the operator's own number and is granted whatever
/// the ceiling says, because they set both. An amount up to the ceiling is granted as asked;
/// anything above it, and `uncapped`, goes to the operator.
pub fn decide(policy: &MemoryPolicy, ask: Ask, machine: &Machine) -> Decision {
    let Some(mem) = machine.numbers() else {
        return match machine {
            // No controller to hold a cap, which is how this box has always run.
            Machine::Here(_) => Decision::Uncapped,
            // A placement that did not answer. A size is still nobody's to compare, so it is the
            // operator's; a default of a size is their own number and runs; a fraction of what is
            // free means nothing here and no one can be asked a number for it.
            Machine::There { host, .. } => match ask {
                Ask::Default => match policy.default {
                    Amount::Bytes(b) => Decision::Grant(b),
                    Amount::Fraction(_) => Decision::Unresolved(format!(
                        "the default is a fraction of what is free, and {host}'s memory could not \
                         be read"
                    )),
                },
                Ask::Amount(Amount::Bytes(b)) => Decision::Operator {
                    bytes: Some(b),
                    ceiling: None,
                },
                Ask::Amount(Amount::Fraction(_)) => Decision::Unresolved(format!(
                    "you asked for a fraction of what is free, and {host}'s memory could not be \
                     read"
                )),
                Ask::Uncapped => Decision::Operator {
                    bytes: None,
                    ceiling: None,
                },
            },
        };
    };
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
                    ceiling: Some(ceiling),
                }
            }
        }
        Ask::Uncapped => Decision::Operator {
            bytes: None,
            ceiling: Some(ceiling),
        },
    }
}

/// The question put to the operator when an ask is above the ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAsk {
    pub command: String,
    /// `None` is uncapped.
    pub bytes: Option<u64>,
    /// The most it may take without asking. `None` when the machine's memory could not be read.
    pub ceiling: Option<u64>,
    /// The machine the command will run on, and what was read off it: the numbers a person is
    /// shown have to be that machine's, or the card is about the wrong box (R155).
    pub machine: Machine,
}

impl MemoryAsk {
    /// How much is free, said the way the card and its facts need it: this box's figures, or the
    /// placement's with its name, or the sentence that says there are none.
    pub fn free_now(&self) -> String {
        use super::scope::human_bytes;
        match (self.machine.numbers(), self.machine.host()) {
            (Some(m), Some(host)) => format!(
                "{} of {} is free on {host}",
                human_bytes(m.available),
                human_bytes(m.total)
            ),
            (Some(m), None) => format!(
                "{} of {} is free now",
                human_bytes(m.available),
                human_bytes(m.total)
            ),
            (None, Some(host)) => {
                format!("{host} did not answer when its memory was asked for")
            }
            (None, None) => "this box's memory could not be read".to_string(),
        }
    }

    /// The ceiling in the words a refusal uses. Without one, the sentence says why there is no
    /// number rather than quoting this box's.
    pub fn ceiling_clause(&self) -> String {
        use super::scope::human_bytes;
        match self.ceiling {
            Some(c) => format!("the {} you may take without asking", human_bytes(c)),
            None => "any ceiling at all: this machine's memory could not be read".to_string(),
        }
    }

    /// The way back from a refusal or a question nobody could answer: a number when the ceiling
    /// was read, and the work itself when it was not.
    pub fn ask_or_smaller(&self) -> String {
        use super::scope::human_bytes;
        match self.ceiling {
            Some(c) => format!(
                "Ask for {} or less and it starts at once, or make the work smaller.",
                human_bytes(c)
            ),
            None => "Nothing here could size a smaller ask, so make the work smaller.".to_string(),
        }
    }

    /// The card's one line.
    pub fn summary(&self) -> String {
        use super::scope::human_bytes;
        let what = match self.bytes {
            Some(b) => format!("{} of memory", human_bytes(b)),
            None => "NO memory cap".to_string(),
        };
        let above = match self.ceiling {
            Some(c) => format!("above the {} it may take without asking", human_bytes(c)),
            None => {
                "above the ceiling it may take without asking, which could not be read".to_string()
            }
        };
        format!(
            "the model asks for {what} for `{}` — {above}; {}",
            self.command,
            self.free_now(),
        )
    }
}

/// The operator's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGrant {
    Granted,
    Refused(String),
    /// Nobody could be asked: a gate with no person behind it. The call does not run.
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

    /// This box, with `avail_gib` free: the machine a session that was not placed runs on.
    fn here(avail_gib: u64) -> Machine {
        Machine::Here(Some(mem(avail_gib)))
    }

    /// Another machine, as a placement's host answers it (R155's report: 15.1 GiB of 123 GiB).
    fn lubuntu3(avail_gib: u64) -> Machine {
        Machine::There {
            host: "lubuntu3".into(),
            mem: Some(Meminfo {
                total: 123 * GIB,
                available: avail_gib * GIB,
                swap_free: 0,
            }),
        }
    }

    /// A card over a 12 GiB ask, which is above half of what that machine has free.
    fn a_12_gib_ask(machine: &Machine) -> MemoryAsk {
        let p = MemoryPolicy::default();
        let Decision::Operator { bytes, ceiling } =
            decide(&p, Ask::Amount(Amount::Bytes(12 * GIB)), machine)
        else {
            panic!("12 GiB is the operator's on {machine:?}");
        };
        MemoryAsk {
            command: "cargo clippy --workspace".into(),
            bytes,
            ceiling,
            machine: machine.clone(),
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

    /// The three outcomes, and that nothing is clipped: an ask within the ceiling is granted
    /// as asked, and anything above it is a question rather than a smaller number.
    #[test]
    fn the_default_is_granted_an_ask_up_to_the_ceiling_is_granted_and_anything_above_is_the_operators()
     {
        let p = MemoryPolicy::default();
        let m = here(100);
        assert_eq!(decide(&p, Ask::Default, &m), Decision::Grant(64 << 20));
        assert_eq!(
            decide(&p, Ask::Amount(Amount::Bytes(50 * GIB)), &m),
            Decision::Grant(50 * GIB)
        );
        assert_eq!(
            decide(&p, Ask::Amount(Amount::Bytes(51 * GIB)), &m),
            Decision::Operator {
                bytes: Some(51 * GIB),
                ceiling: Some(50 * GIB)
            }
        );
        assert_eq!(
            decide(&p, Ask::Uncapped, &m),
            Decision::Operator {
                bytes: None,
                ceiling: Some(50 * GIB)
            }
        );
    }

    /// The ceiling follows what is free: the same ask is granted on an idle box and goes to
    /// the operator on a busy one.
    #[test]
    fn a_fractional_ceiling_moves_with_what_is_free() {
        let p = MemoryPolicy::default();
        let ask = Ask::Amount(Amount::Bytes(40 * GIB));
        assert_eq!(decide(&p, ask, &here(120)), Decision::Grant(40 * GIB));
        assert!(matches!(
            decide(&p, ask, &here(60)),
            Decision::Operator { .. }
        ));
    }

    /// This box's card is the string it has always been, word for word: the numbers are this
    /// box's and there is no machine to name. A change here is a change to every local session.
    #[test]
    fn a_local_card_is_unchanged_and_names_no_machine() {
        let card = a_12_gib_ask(&here(15));
        assert_eq!(
            card.summary(),
            "the model asks for 12 GiB of memory for `cargo clippy --workspace` — above the 7.5 \
             GiB it may take without asking; 15 GiB of 185 GiB is free now"
        );
        assert_eq!(card.free_now(), "15 GiB of 185 GiB is free now");
    }

    /// A placement's cap is decided from ITS host's numbers and the card says which machine they
    /// are: the fault R155 reports is a card quoting the daemon's box for a command that runs
    /// somewhere else.
    #[test]
    fn a_placements_cap_reads_its_hosts_memory_and_the_card_names_the_host() {
        let card = a_12_gib_ask(&lubuntu3(15));
        // Half of lubuntu3's 15 GiB free, not half of this box's 185 GiB.
        assert_eq!(card.ceiling, Some(7 * GIB + GIB / 2));
        assert_eq!(card.free_now(), "15 GiB of 123 GiB is free on lubuntu3");
        assert_eq!(
            card.summary(),
            "the model asks for 12 GiB of memory for `cargo clippy --workspace` — above the 7.5 \
             GiB it may take without asking; 15 GiB of 123 GiB is free on lubuntu3"
        );
        assert!(!card.summary().contains("185"), "{}", card.summary());
    }

    /// A host that does not answer is `unknown`, and an unknown machine never falls back to this
    /// box's figures: a size is the operator's, and a fraction cannot be resolved at all.
    #[test]
    fn an_unreachable_hosts_memory_asks_and_never_quotes_this_box() {
        let silent = Machine::There {
            host: "lubuntu3".into(),
            mem: None,
        };
        let p = MemoryPolicy::default();
        // A size is the operator's, with no ceiling to compare it against.
        assert_eq!(
            decide(&p, Ask::Amount(Amount::Bytes(12 * GIB)), &silent),
            Decision::Operator {
                bytes: Some(12 * GIB),
                ceiling: None
            }
        );
        let card = a_12_gib_ask(&silent);
        assert!(
            card.summary().contains("lubuntu3 did not answer"),
            "{}",
            card.summary()
        );
        assert!(
            !card.summary().contains("185") && !card.summary().contains("is free"),
            "a card that quoted a figure would be quoting this box's: {}",
            card.summary()
        );
        // The default is the operator's own size, so a bare call still runs.
        assert_eq!(decide(&p, Ask::Default, &silent), Decision::Grant(64 << 20));
        // A fraction of what is free is a question nobody can put a number to.
        assert!(matches!(
            decide(&p, Ask::Amount(Amount::Fraction(0.5)), &silent),
            Decision::Unresolved(ref why) if why.contains("lubuntu3")
        ));
        assert!(matches!(
            decide(&p, Ask::Uncapped, &silent),
            Decision::Operator {
                bytes: None,
                ceiling: None
            }
        ));
    }

    /// This box without `/proc/meminfo` (macOS) has no controller to hold a cap, which is what it
    /// has always done; a placement without an answer is a different fact and is not this one.
    #[test]
    fn a_box_with_no_controller_runs_uncapped_and_a_placement_without_an_answer_does_not() {
        let p = MemoryPolicy::default();
        for ask in [Ask::Default, Ask::Amount(Amount::Bytes(GIB))] {
            assert_eq!(decide(&p, ask, &Machine::Here(None)), Decision::Uncapped);
        }
        assert!(matches!(
            decide(
                &p,
                Ask::Default,
                &Machine::There {
                    host: "lubuntu3".into(),
                    mem: None
                }
            ),
            Decision::Grant(_)
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

    /// A killed call is told the ask that would have been enough, sized by what the box may hand
    /// over without asking - and nothing else: no cap restated, no config file to edit.
    #[test]
    fn a_kill_at_the_cap_says_one_ask_sized_to_start() {
        let note = ask_after_kill(64 << 20, Some(&mem(100)));
        assert!(note.contains("memory: \"2G\""), "{note}");
        assert!(!note.contains("leticode.toml"), "{note}");
        assert!(
            !note.contains("cap"),
            "no second statement of the cap: {note}"
        );
        // 2 GiB killed gives 32 GiB, granted at once on a box with 100 GiB free.
        assert!(ask_after_kill(2 * GIB, Some(&mem(100))).contains("\"32G\""));
        // On a box with 20 GiB free the ceiling is 10 GiB, and that is what is offered.
        let tight = ask_after_kill(2 * GIB, Some(&mem(20)));
        assert!(tight.contains("\"10G\""), "{tight}");
        assert_eq!(ask_text(512 << 20), "512M");
        assert_eq!(ask_text(3 * GIB), "3G");
    }

    /// The line a round hands the model where it chooses a cap: what is free, what an ask may take
    /// without asking, and what this daemon's own work already promises.
    #[test]
    fn the_round_clause_says_what_is_free_and_what_an_ask_may_take() {
        let p = MemoryPolicy::default();
        assert_eq!(
            mem(100).round_clause(&p, Holds::default()),
            "100 GiB of 185 GiB is free (swap 0 B); an ask up to 50 GiB is granted at once."
        );
        let holds = Holds {
            scopes: 2,
            promised: 4 * GIB,
            peaks: 300 << 20,
        };
        assert_eq!(
            mem(100).round_clause(&p, holds),
            "100 GiB of 185 GiB is free (swap 0 B); an ask up to 50 GiB is granted at once; \
             this daemon's live calls and jobs hold 300 MiB of 4 GiB promised across 2 scope(s)."
        );
        // The ceiling moves with what is free, which is the whole reason this cannot be a schema.
        let tight = mem(20).round_clause(&p, Holds::default());
        assert!(tight.contains("an ask up to 10 GiB"), "{tight}");
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
