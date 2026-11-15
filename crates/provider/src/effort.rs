//! One effort scale for every door: `off`, `low`, `medium`, `high`, `max`. A door takes a subset,
//! and a level it lacks is clamped to the nearest one it has and said once.

use std::fmt;

/// The operator's word for how hard a turn thinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effort {
    Off,
    Low,
    Medium,
    High,
    Max,
}

/// The five words, in order, for help text and refusals.
pub const WORDS: &str = "off, low, medium, high, max";

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Off,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Max,
    ];

    /// The five words, and the older spellings configs still carry (`none`, `minimal`, `xhigh`).
    pub fn parse(s: &str) -> Result<Effort, String> {
        Ok(
            match s.trim().trim_matches('"').to_ascii_lowercase().as_str() {
                "off" | "none" => Effort::Off,
                "low" | "minimal" => Effort::Low,
                "medium" => Effort::Medium,
                "high" => Effort::High,
                "max" | "xhigh" => Effort::Max,
                other => return Err(format!("`{other}` is not an effort; they are {WORDS}")),
            },
        )
    }

    pub fn word(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    /// The nearest of `have` to `self`, ties upward. A thinking level never clamps to `off`, so a
    /// door's missing rung never silently turns thinking off.
    pub fn clamp_to(self, have: &[Effort]) -> Option<Effort> {
        let on = |e: &&Effort| self == Effort::Off || **e != Effort::Off;
        have.iter()
            .filter(on)
            .min_by_key(|e| ((**e as i8 - self as i8).abs(), -(**e as i8)))
            .copied()
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// The canonical word for `s`, or `s` unchanged when it is not a level: callers that pass a word
/// through to a dialect keep the dialect's own refusal.
pub fn canonical(s: &str) -> String {
    Effort::parse(s)
        .map(|e| e.word().to_string())
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_words_and_the_old_spellings_parse() {
        for e in Effort::ALL {
            assert_eq!(Effort::parse(e.word()), Ok(e));
        }
        assert_eq!(Effort::parse("XHIGH"), Ok(Effort::Max));
        assert_eq!(Effort::parse("none"), Ok(Effort::Off));
        let why = Effort::parse("heroic").unwrap_err();
        assert!(why.contains(WORDS), "{why}");
    }

    #[test]
    fn a_missing_level_clamps_to_the_nearest_and_ties_go_up() {
        use Effort::*;
        let kimi = [Off, Low, High, Max];
        assert_eq!(Medium.clamp_to(&kimi), Some(High));
        assert_eq!(Max.clamp_to(&kimi), Some(Max));
        assert_eq!(Low.clamp_to(&[Off, High, Max]), Some(High));
        assert_eq!(Low.clamp_to(&[Off]), None, "thinking never clamps to off");
        assert_eq!(Off.clamp_to(&[Low, High]), Some(Low));
        assert_eq!(High.clamp_to(&[]), None);
    }
}
