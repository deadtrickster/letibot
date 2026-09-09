//! The one place a colour is chosen.
//!
//! `letibot-tui::render::sgr` is a list of ANSI constants and every caller picks
//! from it directly, so "what colour is a failed tool" is answered in as many
//! places as it is asked. That is fine at 2,500 lines and is the reason a large
//! TUI eventually grows a theme system nobody wanted.
//!
//! The middle path taken here: name the **roles**, not the colours. A caller
//! asks for [`Role::Failure`], not for red. A [`Palette`] maps roles to
//! sequences, and there are two of them — colour and none — because the second
//! is not a theme, it is the `--replay`, pipe-to-a-file and CI case that must
//! produce byte-identical output on every machine.
//!
//! # 256 colours, not truecolour
//!
//! Every sequence below is either a basic SGR attribute or a 256-colour index.
//! Truecolour is not universally forwarded through `tmux`, `screen` or `ssh`
//! with an old `TERM`, and a head that renders a diff as invisible-on-invisible
//! in a multiplexer has failed at the only job the colour had.

/// What a piece of text *means*. Callers name these; only [`Palette`] knows what
/// they look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary body text. No sequence at all.
    Plain,
    /// Structure a reader skips: frame lines, counts, ids.
    Faint,
    /// A heading or anything that anchors a scan.
    Strong,
    /// A top-level heading in rendered markdown.
    ///
    /// Both surveyed heads colour their headings and letibot painted every one of
    /// them bold-and-nothing-else, which is why a long answer read as one slab: a
    /// model writes `## Findings` and `### Why` constantly and bold alone does not
    /// separate them from the emphasis inside a paragraph.
    Heading,
    /// A second-level heading. A *different* colour rather than a dimmer one — two
    /// shades of the same hue are indistinguishable under half the terminal themes
    /// on this box, and the level is the thing being encoded.
    Subheading,
    /// The bar down the left of a user's own message.
    UserAccent,
    /// The user's own words, on a raised block.
    ///
    /// **Foreground and background together, always.** The head's own argument
    /// against a raised block (see `app::screen`) is that a dark block is invisible
    /// or unreadable depending on which half of the pair the terminal's theme
    /// supplies — which is true of a background set alone. Setting both makes the
    /// pair self-consistent under any theme, and it is still nothing at all under
    /// [`Palette::None`], where the accent glyph is what survives.
    UserBlock,
    /// Something completed successfully.
    Success,
    /// Something in flight.
    Pending,
    /// Something that failed, was refused, or was interrupted.
    Failure,
    /// Something that needs a person: a decision request, a warning.
    Attention,
    /// The model's own reasoning, which must never read like its answer.
    Reasoning,
    /// A code span or a code block's body.
    Code,
    /// A diff line that was added.
    Added,
    /// A diff line that was removed.
    Removed,
    /// The changed run *inside* an added or removed line.
    Emphasis,
    /// Syntax: a language keyword.
    Keyword,
    /// Syntax: a string literal.
    StringLit,
    /// Syntax: a numeric literal.
    NumberLit,
    /// Syntax: a comment.
    Comment,
    /// Syntax: a type or a constructor.
    TypeName,
    /// Syntax: a function name at its definition or call site.
    FuncName,
}

/// A mapping from roles to escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// 256-colour terminal.
    Colour,
    /// No sequences at all. Not a "monochrome theme": the output is plain text,
    /// which is what a replay diff and a CI log need.
    None,
}

impl Palette {
    /// The opening sequence for a role. Empty for [`Palette::None`].
    pub fn open(self, r: Role) -> &'static str {
        if self == Palette::None {
            return "";
        }
        match r {
            Role::Plain => "",
            Role::Faint => "\x1b[38;5;244m",
            Role::Strong => "\x1b[1m",
            Role::Heading => "\x1b[1;38;5;79m",
            Role::Subheading => "\x1b[1;38;5;111m",
            Role::UserAccent => "\x1b[38;5;111m",
            Role::UserBlock => "\x1b[48;5;236;38;5;253m",
            Role::Success => "\x1b[38;5;71m",
            Role::Pending => "\x1b[38;5;179m",
            Role::Failure => "\x1b[38;5;167m",
            Role::Attention => "\x1b[38;5;214m",
            Role::Reasoning => "\x1b[2;38;5;103m",
            Role::Code => "\x1b[38;5;180m",
            Role::Added => "\x1b[38;5;71m",
            Role::Removed => "\x1b[38;5;167m",
            Role::Emphasis => "\x1b[1;4m",
            Role::Keyword => "\x1b[38;5;140m",
            Role::StringLit => "\x1b[38;5;107m",
            Role::NumberLit => "\x1b[38;5;173m",
            Role::Comment => "\x1b[38;5;244m",
            Role::TypeName => "\x1b[38;5;110m",
            Role::FuncName => "\x1b[38;5;179m",
        }
    }

    /// Wrap `s` in the role. A no-op for [`Palette::None`] and for
    /// [`Role::Plain`], so neither costs bytes.
    pub fn paint(self, r: Role, s: &str) -> String {
        let o = self.open(r);
        if o.is_empty() {
            return s.to_string();
        }
        format!("{o}{s}{}", crate::width::RESET)
    }

    pub fn is_colour(self) -> bool {
        self == Palette::Colour
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_none_palette_emits_no_bytes_a_terminal_would_eat() {
        for r in [Role::Failure, Role::Added, Role::Keyword, Role::Strong] {
            assert_eq!(Palette::None.paint(r, "x"), "x");
        }
    }

    #[test]
    fn every_role_closes_what_it_opens() {
        for r in [
            Role::Faint,
            Role::Strong,
            Role::Heading,
            Role::Subheading,
            Role::UserAccent,
            Role::UserBlock,
            Role::Success,
            Role::Pending,
            Role::Failure,
            Role::Attention,
            Role::Reasoning,
            Role::Code,
            Role::Added,
            Role::Removed,
            Role::Emphasis,
            Role::Keyword,
            Role::StringLit,
            Role::NumberLit,
            Role::Comment,
            Role::TypeName,
            Role::FuncName,
        ] {
            let s = Palette::Colour.paint(r, "abc");
            assert!(s.ends_with(crate::width::RESET), "{r:?} -> {s:?}");
            assert_eq!(crate::width::width(&s), 3, "{r:?} changed the width");
        }
    }
}
