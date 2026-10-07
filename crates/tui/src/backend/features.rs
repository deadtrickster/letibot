//! **What the terminal on the other end can do beyond drawing cells.**
//!
//! The operator, 2026-10-07, on Ghostty: *"what ghostty specific things we can have?"* and then
//! *"i want all 7 you listed"*. Seven terminal features, each a sequence a terminal that does
//! not know it may print as garbage or act on wrongly — so each is switched on only for a
//! terminal known to speak it, and the set is one value decided once, here, rather than seven
//! `TERM` checks scattered through the head.
//!
//! # Detection
//!
//! `TERM=xterm-ghostty` (which survives `ssh` when the terminfo is installed on the far side —
//! the Linux box case) or `TERM_PROGRAM=ghostty` (local only; ssh does not forward it) turns on
//! all seven. Nothing else does by default: kitty, WezTerm and iTerm2 speak some of them, each
//! a different some, and a guess about which is a garbled screen for somebody.
//!
//! `LETIBOT_TERM_FEATURES` overrides the guess either way: `all`, `none`, or a comma list of
//! the names below (`notify,progress,keys,links,clipboard,images,background`). A `-name`
//! removes one from whatever the detection gave, so `-keys` on Ghostty turns the kitty keyboard
//! off and leaves the rest.
//!
//! **Inside tmux nothing is on** unless the override says so: tmux eats or mangles most of
//! these unless its own passthrough is configured, and `TERM` there names tmux, not the
//! terminal outside it.

/// The seven, as switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features {
    /// Desktop notifications (OSC 9) and focus reporting (`?1004`), which says when to send one.
    pub notify: bool,
    /// The tab/titlebar progress bar (OSC 9;4).
    pub progress: bool,
    /// The kitty keyboard protocol (`CSI > 1 u`): Shift+Enter, an Esc that is never half an
    /// arrow key.
    pub keys: bool,
    /// Hyperlinks (OSC 8).
    pub links: bool,
    /// Writing the system clipboard (OSC 52).
    pub clipboard: bool,
    /// Inline images (the kitty graphics protocol).
    pub images: bool,
    /// Asking the terminal its background colour (OSC 11), to choose a light or dark palette.
    pub background: bool,
}

const NAMES: [&str; 7] = [
    "notify",
    "progress",
    "keys",
    "links",
    "clipboard",
    "images",
    "background",
];

impl Features {
    pub const ALL: Features = Features {
        notify: true,
        progress: true,
        keys: true,
        links: true,
        clipboard: true,
        images: true,
        background: true,
    };

    /// From this process's environment.
    pub fn detect() -> Features {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Features::from_env(
            &var("TERM"),
            &var("TERM_PROGRAM"),
            !var("TMUX").is_empty(),
            &var("LETIBOT_TERM_FEATURES"),
        )
    }

    /// The decision, from the four inputs that make it — a function so it can be tested
    /// without touching the environment.
    pub fn from_env(term: &str, term_program: &str, in_tmux: bool, wanted: &str) -> Features {
        let ghostty = term == "xterm-ghostty" || term_program.eq_ignore_ascii_case("ghostty");
        let mut f = if ghostty && !in_tmux {
            Features::ALL
        } else {
            Features::default()
        };
        for word in wanted.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            match word {
                "all" => f = Features::ALL,
                "none" => f = Features::default(),
                w => {
                    let (on, name) = match w.strip_prefix('-') {
                        Some(n) => (false, n),
                        None => (true, w.strip_prefix('+').unwrap_or(w)),
                    };
                    if let Some(slot) = f.slot(name) {
                        *slot = on;
                    }
                }
            }
        }
        f
    }

    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "notify" => &mut self.notify,
            "progress" => &mut self.progress,
            "keys" => &mut self.keys,
            "links" => &mut self.links,
            "clipboard" => &mut self.clipboard,
            "images" => &mut self.images,
            "background" => &mut self.background,
            _ => return None,
        })
    }

    /// The names that are on, for `/status` and the start-up line.
    pub fn names(&self) -> Vec<&'static str> {
        let mut me = *self;
        NAMES
            .iter()
            .copied()
            .filter(|n| me.slot(n).is_some_and(|s| *s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_gets_all_seven_and_nothing_else_gets_any_by_default() {
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, ""),
            Features::ALL
        );
        assert_eq!(
            Features::from_env("xterm-256color", "ghostty", false, ""),
            Features::ALL
        );
        for (term, prog) in [
            ("xterm-256color", "Apple_Terminal"),
            ("xterm-kitty", ""),
            ("xterm-256color", "WezTerm"),
            ("screen-256color", ""),
        ] {
            assert_eq!(
                Features::from_env(term, prog, false, ""),
                Features::default(),
                "{term} {prog}"
            );
        }
        // tmux in front of Ghostty: the sequences would be eaten or mangled.
        assert_eq!(
            Features::from_env("xterm-ghostty", "ghostty", true, ""),
            Features::default()
        );
    }

    #[test]
    fn the_override_adds_removes_and_replaces() {
        let f = Features::from_env("xterm-ghostty", "", false, "-keys");
        assert!(!f.keys && f.notify && f.images);
        let f = Features::from_env("xterm-256color", "", false, "links,clipboard");
        assert_eq!(f.names(), vec!["links", "clipboard"]);
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "none"),
            Features::default()
        );
        assert_eq!(
            Features::from_env("xterm-256color", "", true, "all"),
            Features::ALL
        );
        // An unknown word is ignored rather than refusing to start a head.
        assert_eq!(
            Features::from_env("xterm-256color", "", false, "sparkles"),
            Features::default()
        );
    }
}
