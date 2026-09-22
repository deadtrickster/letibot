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
//! # The ANSI 16, not the 256-colour cube
//!
//! Every sequence below is a basic SGR attribute or one of the sixteen named
//! colours. That is not a downgrade for its own sake, it is the only way a role
//! can mean anything to the person looking at it.
//!
//! **Colours 16–255 of the xterm cube are absolute RGB.** A terminal theme
//! defines slots 0–15 and nothing else, so a role painted `38;5;167` is the same
//! salmon on Solarized Light, Gruvbox and Nord — it paints *beside* the theme
//! rather than within it. This table used to be entirely cube indices and the
//! operator's report was the predictable one: "colors not matching theme". Now
//! `Role::Failure` is `31`, which is whatever the reader calls red.
//!
//! Nothing here needs more precision than sixteen slots. The roles that looked
//! like they did — the two heading levels, `Attention` against `Pending`,
//! `Reasoning` against `Faint` — are separated by an **attribute** instead of by
//! a second shade, which is stronger: two shades of one hue are indistinguishable
//! under half the themes on this box, and bold-vs-plain is not.
//!
//! Truecolour is not an option either, and was not before: it is not universally
//! forwarded through `tmux`, `screen` or `ssh` with an old `TERM`.
//!
//! # There is no light-mode and no dark-mode here
//!
//! Deliberately, and it is worth saying because the previous table had a dark
//! assumption in it that nothing declared: `UserBlock` was `48;5;236` — a dark
//! grey background — with a near-white foreground on top. On a light terminal
//! that is a black bar across the transcript. Since every other role is now a
//! theme slot or an attribute, the only role that names a *pair* is `UserBlock`,
//! and it names it as `7` (reverse video), which asks the terminal to swap its
//! own two colours. That is self-consistent under any theme by construction,
//! which is exactly what the old comment claimed for the hardcoded pair and could
//! not deliver.

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
    /// **Foreground and background together, always** — and taken from the theme,
    /// not named. The head's own argument against a raised block (see
    /// `app::screen`) is that a dark block is invisible or unreadable depending on
    /// which half of the pair the terminal's theme supplies. That argument applies
    /// just as well to a *hardcoded* pair, which is what this used to be: a dark
    /// grey behind a near-white, i.e. a black bar on a light terminal. Reverse
    /// video is the pair the reader already chose, swapped, so it is legible by
    /// construction. It is still nothing at all under [`Palette::None`], where the
    /// accent glyph is what survives.
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

impl Role {
    /// The role whose **foreground** this role used to paint with, before the
    /// diff roles spent their sequence on a background.
    ///
    /// The operator, on the first cube tint: *"please keep original
    /// foregrounds"* — a cell opened with `32;48;5;22` and every plain run in
    /// it inherited the green, so the diff read as green text on green. Now
    /// the role's sequence is background-only and the text keeps whatever
    /// foreground it had — syntax colours, or the terminal's default. What
    /// still wants the original green and red is the **sign glyph**, which was
    /// `32`/`31` from the day it was drawn: it paints through this mapping,
    /// and `Success`/`Failure` are exactly those two sequences.
    pub fn foreground(self) -> Role {
        match self {
            Role::Added => Role::Success,
            Role::Removed => Role::Failure,
            other => other,
        }
    }
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
            // Dim, not a grey. `90` is the theme's "bright black", which several
            // light themes put within a hair of the background; the attribute
            // de-emphasises whatever the foreground already is.
            Role::Faint => "\x1b[2m",
            Role::Strong => "\x1b[1m",
            // The two levels differ by **hue**, as they did — cyan and blue are
            // both theme slots — and the level is carried by the hashes too, for
            // the monochrome reader.
            Role::Heading => "\x1b[1;36m",
            Role::Subheading => "\x1b[1;34m",
            Role::UserAccent => "\x1b[34m",
            // Reverse video: the terminal's own pair, swapped. See the module
            // header — this role names neither a foreground nor a background,
            // and naming them absolutely is what made a light-themed
            // terminal render the operator's own words as a black bar. (The
            // diff roles do name a background — but from the theme's slots,
            // which is the difference between *the theme decides* and *the
            // table overrides it*.)
            Role::UserBlock => "\x1b[7m",
            Role::Success => "\x1b[32m",
            Role::Pending => "\x1b[33m",
            Role::Failure => "\x1b[31m",
            // Bold yellow against `Pending`'s plain yellow. "Needs a person" and
            // "is happening" are close enough in meaning that a *weight* is the
            // right distinction; a second orange was never one, since the cube's
            // orange is not a slot any theme defines.
            Role::Attention => "\x1b[1;33m",
            // Dim italic, which is what `card::reasoning` and `app::screen` have
            // both claimed in prose since they were written: *"de-emphasis by
            // colour is a no-op under a terminal-native palette, so the attribute
            // is what actually carries it."* The table was the half that had not
            // caught up — it painted a blue-grey and no italic.
            Role::Reasoning => "\x1b[2;3m",
            Role::Code => "\x1b[36m",
            // The diff roles carry a **background**, and only a background —
            // the operator, on the first cube tint: *"please keep original
            // foregrounds"*. The trial ran three times: the theme's slots
            // (`42`/`41`) were bands, not tints — *"too much color, the diff
            // is unreadable"*; the cube tint beside the role's own foreground
            // turned every plain run green-on-green; so what remains is the
            // xterm cube's darkest green and red (`48;5;22`, `48;5;52`) alone,
            // under whatever foreground the text already had — syntax colours,
            // or the terminal's default. The sign glyph keeps the original
            // green and red through [`Role::foreground`]. The exception is
            // carried by name in `no_role_paints_outside…`: two roles, chosen
            // once, and nothing else may follow them out.
            Role::Added => "\x1b[48;5;22m",
            Role::Removed => "\x1b[48;5;52m",
            Role::Emphasis => "\x1b[1;4m",
            Role::Keyword => "\x1b[35m",
            Role::StringLit => "\x1b[32m",
            Role::NumberLit => "\x1b[33m",
            // The same attribute as `Faint`, and for the same reason: a comment is
            // structure the reader skips. Two roles are allowed to look alike;
            // what is not allowed is one role meaning two things.
            Role::Comment => "\x1b[2m",
            Role::TypeName => "\x1b[36m",
            Role::FuncName => "\x1b[34m",
        }
    }

    /// Wrap `s` in the role. A no-op for [`Palette::None`] and for
    /// [`Role::Plain`], so neither costs bytes.
    pub fn paint(self, r: Role, s: &str) -> String {
        // **The text is sanitised here, once, for every painted string** (§3.1).
        //
        // A palette's argument is *text* by definition — the head's own sequences are
        // the ones `open`/`close` supply — so a control byte in `s` is somebody else's
        // bytes being painted as though they were words. Doing it here rather than at
        // each caller is the difference between a rule and a habit: this crate draws
        // every card and had **no** sanitiser at all, and a tool-progress note reached a
        // card's tail raw because the guard lived in the head instead.
        let s = &crate::text::without_control(s);
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

/// A palette **bound to the style of the block it is painting inside**.
///
/// # The defect this type exists to make unrepresentable
///
/// [`Palette::paint`] closes a span with `RESET`, which restores the *terminal
/// default*. That is right at the top level and wrong everywhere else, and the
/// operator found the difference by looking at the screen: the model's reasoning
/// is dim grey, the reasoning contained a heading and an inline code span, and
/// each of those closed to white — so the block "tries to be grey, then goes
/// green and becomes white for several rows and then grey again".
///
/// A reset is not a restore. Inside a themed block the close has to re-establish
/// the block's own style, and the only way that cannot be forgotten one call site
/// at a time is for the closing sequence to come from a value that knows what it
/// is inside. That value is this one: [`Painter::close`] is `RESET` at the top
/// level and `RESET` + the block's opening sequence inside one, and every span
/// painted through a `Painter` closes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Painter {
    palette: Palette,
    /// The role of the block this painter is painting inside, if any.
    base: Option<Role>,
}

impl Painter {
    /// A painter at the top level: nothing encloses it, so a close is a reset.
    pub fn new(palette: Palette) -> Painter {
        Painter {
            palette,
            base: None,
        }
    }

    /// A painter inside a block styled as `base`.
    pub fn inside(palette: Palette, base: Role) -> Painter {
        Painter {
            palette,
            base: Some(base),
        }
    }

    /// The same painter, re-based on `base`. `None` lifts it back to the top level.
    pub fn rebase(self, base: Option<Role>) -> Painter {
        Painter {
            palette: self.palette,
            base,
        }
    }

    pub fn palette(self) -> Palette {
        self.palette
    }

    pub fn base(self) -> Option<Role> {
        self.base
    }

    /// The opening sequence for a role, as [`Palette::open`].
    pub fn open(self, r: Role) -> &'static str {
        self.palette.open(r)
    }

    /// What re-establishes the enclosing block after a span: a reset, plus the
    /// block's own opening sequence when there is a block.
    ///
    /// Empty under [`Palette::None`], where there is nothing to close.
    pub fn close(self) -> String {
        if self.palette == Palette::None {
            return String::new();
        }
        match self.base {
            None => crate::width::RESET.to_string(),
            Some(b) => format!("{}{}", crate::width::RESET, self.palette.open(b)),
        }
    }

    /// Wrap `s` in the role, closing back to the **block**, not to the terminal.
    pub fn paint(self, r: Role, s: &str) -> String {
        let o = self.open(r);
        if o.is_empty() {
            // `Role::Plain` inside a themed block still has to be the block's
            // style, and it already is: nothing was opened, so nothing is closed.
            return s.to_string();
        }
        format!("{o}{s}{}", self.close())
    }

    /// `s`, already carrying escapes of its own, made safe to place inside the
    /// block: every reset in it is turned into a restore of the block's style.
    ///
    /// The escape hatch for text painted by something that was never told what it
    /// is inside — a syntax highlighter's line, a diff's gutter. Preferred over
    /// re-painting, which would double the sequences.
    pub fn rebase_resets(self, s: &str) -> String {
        let close = self.close();
        if self.base.is_none() || close == crate::width::RESET {
            return s.to_string();
        }
        s.replace(crate::width::RESET, &close)
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
    /// Every role a theme can reach.
    const EVERY_ROLE: [Role; 22] = [
        Role::Plain,
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
    ];

    /// The operator's fourth report: *"colors not matching theme."*
    ///
    /// It cannot be settled by looking at one terminal — a screenshot on a dark
    /// theme looks fine either way — so it is settled by the sequences. `38;5;N`
    /// and `48;5;N` above 15 are absolute RGB out of the xterm cube and are the
    /// definition of ignoring the theme; `2;` and `3;` (truecolour) likewise.
    ///
    /// **One exception, by name.** The diff roles' backgrounds are the cube's
    /// darkest green and red (`48;5;22`, `48;5;52`), because the operator tried
    /// the theme's own slots live and ruled them *"too much color, the diff is
    /// unreadable"* — a theme's background slots are bands, not tints, and the
    /// only calm green is off-law. The exception is exactly two roles and
    /// exactly a background: no foreground may leave the cube, and no third
    /// role may follow, so the loophole is a named pair rather than a door.
    #[test]
    fn no_role_paints_outside_the_sixteen_colours_a_theme_defines() {
        // The two roles the operator spent the cube on, and the exact
        // sequences they are allowed: a background tint, and nothing else —
        // the foregrounds stay the theme's, per the operator's own ruling.
        const CUBE_EXCEPTIONS: &[(Role, &str)] = &[
            (Role::Added, "\x1b[48;5;22m"),
            (Role::Removed, "\x1b[48;5;52m"),
        ];
        for r in EVERY_ROLE {
            let o = Palette::Colour.open(r);
            if let Some((_, allowed)) = CUBE_EXCEPTIONS.iter().find(|(e, _)| e == &r) {
                assert_eq!(
                    o, *allowed,
                    "{r:?} is the named cube exception and may not drift: {o:?}"
                );
                continue;
            }
            assert!(
                !o.contains("38;5;") && !o.contains("48;5;") && !o.contains("38;2;"),
                "{r:?} paints an absolute colour the theme cannot reach: {o:?}"
            );
            for p in o
                .trim_start_matches("\x1b[")
                .trim_end_matches('m')
                .split(';')
                .filter(|p| !p.is_empty())
            {
                let n: u32 = p.parse().expect("every parameter is numeric");
                assert!(
                    // attributes and their cancels
                    n <= 29
                        // the eight, foreground and background
                        || (30..=37).contains(&n)
                        || (40..=47).contains(&n)
                        // the bright eight
                        || (90..=97).contains(&n)
                        || (100..=107).contains(&n),
                    "{r:?} uses SGR parameter {n}, which is not a theme slot: {o:?}"
                );
            }
        }
    }

    /// The half of the same report that a light terminal would have shown as a
    /// black bar: the one role that sets a background must not name one.
    #[test]
    fn the_user_block_takes_its_pair_from_the_terminal() {
        let o = Palette::Colour.open(Role::UserBlock);
        assert_eq!(o, "\x1b[7m", "the raised block must be reverse video");
    }

    /// Roles that mean different things have to *look* different, or moving to
    /// sixteen slots would have traded one defect for another.
    #[test]
    fn roles_a_reader_has_to_tell_apart_still_differ() {
        for (a, b) in [
            (Role::Heading, Role::Subheading),
            (Role::Attention, Role::Pending),
            (Role::Reasoning, Role::Plain),
            (Role::Success, Role::Failure),
            (Role::Added, Role::Removed),
            (Role::Keyword, Role::StringLit),
            (Role::Keyword, Role::NumberLit),
            (Role::TypeName, Role::FuncName),
        ] {
            assert_ne!(
                Palette::Colour.open(a),
                Palette::Colour.open(b),
                "{a:?} and {b:?} are the same sequence"
            );
        }
    }

    #[test]
    fn a_painter_at_the_top_level_is_the_palette_it_wraps() {
        let p = Painter::new(Palette::Colour);
        for r in [Role::Heading, Role::Code, Role::Strong] {
            assert_eq!(p.paint(r, "abc"), Palette::Colour.paint(r, "abc"));
        }
        assert_eq!(p.close(), crate::width::RESET);
    }

    #[test]
    fn a_span_inside_a_block_restores_the_block_and_not_the_default() {
        let p = Painter::inside(Palette::Colour, Role::Reasoning);
        let painted = p.paint(Role::Code, "x");
        assert!(
            painted.ends_with(&format!(
                "{}{}",
                crate::width::RESET,
                Palette::Colour.open(Role::Reasoning)
            )),
            "a code span in reasoning closed to the terminal default: {painted:?}"
        );
    }

    #[test]
    fn the_none_palette_has_nothing_to_restore() {
        let p = Painter::inside(Palette::None, Role::Reasoning);
        assert_eq!(p.close(), "");
        assert_eq!(p.paint(Role::Code, "x"), "x");
        assert_eq!(p.rebase_resets("x"), "x");
    }

    #[test]
    fn rebasing_resets_rewrites_a_foreign_painters_closes() {
        let p = Painter::inside(Palette::Colour, Role::Reasoning);
        let foreign = Palette::Colour.paint(Role::Keyword, "fn");
        let fixed = p.rebase_resets(&foreign);
        assert!(fixed.ends_with(&p.close()), "{fixed:?}");
        assert!(!fixed.ends_with("\x1b[0m"), "{fixed:?}");
    }
}
