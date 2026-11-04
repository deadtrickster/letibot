//! A palette bound to the block it is painting inside, closing spans back to the enclosing block.
//! The role table is rano's. [`Sgr`] computes its sequences once, since `Palette::open` allocates
//! per call and frames paint every tick.

use std::sync::OnceLock;

use rano::style::{Palette, Role};
use rano::width::text::RESET;

/// Every sequence rano spells, per palette, kept for the life of the process.
struct Table {
    open: [[String; Role::ALL.len()]; 3],
    background: [[String; 16]; 3],
    reverse: [String; 3],
}

const PALETTES: [Palette; 3] = [Palette::Colour, Palette::Light, Palette::None];

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table {
        open: std::array::from_fn(|p| std::array::from_fn(|r| PALETTES[p].open(Role::ALL[r]))),
        background: std::array::from_fn(|p| {
            std::array::from_fn(|slot| PALETTES[p].background_look(slot as u8).sgr())
        }),
        reverse: std::array::from_fn(|p| PALETTES[p].reverse_look().sgr()),
    })
}

fn palette_index(p: Palette) -> usize {
    match p {
        Palette::Colour => 0,
        Palette::Light => 1,
        Palette::None => 2,
    }
}

/// [`Role::ALL`] is in declaration order (a test holds it to that), so a role's discriminant
/// is its row.
fn role_index(r: Role) -> usize {
    r as usize
}

/// rano's role table, as `&'static str`: [`Palette::open`] and [`Palette::paint`] without
/// the allocation per call. See the module header.
pub trait Sgr: Copy {
    /// The opening sequence for a role, as [`Palette::open`] spells it.
    fn sgr(self, r: Role) -> &'static str;
    /// `s` wrapped in the role.
    fn painted(self, r: Role, s: &str) -> String;
}

impl Sgr for Palette {
    fn sgr(self, r: Role) -> &'static str {
        &table().open[palette_index(self)][role_index(r)]
    }

    /// As [`Palette::paint`]: closed with a reset, and a no-op for [`Palette::None`] and
    /// [`Role::Plain`], so neither costs bytes. It does not sanitise `s` - see
    /// [`Palette::paint`] for why that is a correction rather than an omission.
    fn painted(self, r: Role, s: &str) -> String {
        let o = self.sgr(r);
        if o.is_empty() {
            return s.to_string();
        }
        format!("{o}{s}{RESET}")
    }
}

/// A palette bound to the style of the block it is painting inside. `Palette::paint` closes a
/// span with `RESET`, which restores the terminal default, so a coloured span inside a dim
/// block went white; a close that knows its block is the only fix no call site can forget.
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
    pub fn sgr(self, r: Role) -> &'static str {
        self.palette.sgr(r)
    }

    /// A program's own background slot, `0`-`15`, as a sequence: empty under [`Palette::None`]
    /// and for a slot above the sixteen. A slot rather than a role because the cube's indices
    /// are absolute RGB; the screen is its one caller and a payload row never comes here.
    pub fn background(self, slot: u8) -> &'static str {
        match table().background[palette_index(self.palette)].get(slot as usize) {
            Some(s) => s,
            None => "",
        }
    }

    /// Reverse video, as a sequence: empty under [`Palette::None`]. One spelling with
    /// [`Role::UserBlock`], because a terminal has one way to say reverse.
    pub fn reverse(self) -> &'static str {
        &table().reverse[palette_index(self.palette)]
    }

    /// What re-establishes the enclosing block after a span: a reset, plus the block's own
    /// opening sequence when there is a block. Empty under [`Palette::None`], where there is
    /// nothing to close.
    pub fn close(self) -> String {
        if self.palette == Palette::None {
            return String::new();
        }
        match self.base {
            None => RESET.to_string(),
            Some(b) => format!("{RESET}{}", self.palette.sgr(b)),
        }
    }

    /// Wrap `s` in the role, closing back to the block, not to the terminal.
    pub fn painted(self, r: Role, s: &str) -> String {
        let o = self.sgr(r);
        if o.is_empty() {
            // `Role::Plain` inside a themed block still has to be the block's
            // style, and it already is: nothing was opened, so nothing is closed.
            return s.to_string();
        }
        format!("{o}{s}{}", self.close())
    }

    /// `s`, already carrying escapes of its own, made safe to place inside the block: every
    /// reset in it becomes a restore of the block's style. The escape hatch for text painted by
    /// something never told what it is inside - a highlighter's line, a diff's gutter.
    pub fn rebase_resets(self, s: &str) -> String {
        let close = self.close();
        if self.base.is_none() || close == RESET {
            return s.to_string();
        }
        s.replace(RESET, &close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_painter_at_the_top_level_is_the_palette_it_wraps() {
        let p = Painter::new(Palette::Colour);
        for r in [Role::Heading, Role::Code, Role::Strong] {
            assert_eq!(p.painted(r, "abc"), Palette::Colour.paint(r, "abc"));
        }
        assert_eq!(p.close(), RESET);
    }

    #[test]
    fn a_span_inside_a_block_restores_the_block_and_not_the_default() {
        let p = Painter::inside(Palette::Colour, Role::Reasoning);
        let painted = p.painted(Role::Code, "x");
        assert!(
            painted.ends_with(&format!("{RESET}{}", Palette::Colour.open(Role::Reasoning))),
            "a code span in reasoning closed to the terminal default: {painted:?}"
        );
    }

    #[test]
    fn the_none_palette_has_nothing_to_restore() {
        let p = Painter::inside(Palette::None, Role::Reasoning);
        assert_eq!(p.close(), "");
        assert_eq!(p.painted(Role::Code, "x"), "x");
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

    /// The kept table is rano's, byte for byte, for every palette and role - and
    /// [`Role::ALL`] is in declaration order, which is what lets a discriminant index it.
    #[test]
    fn the_kept_sequences_are_ranos() {
        for (i, r) in Role::ALL.iter().enumerate() {
            assert_eq!(role_index(*r), i, "{r:?}");
            for p in PALETTES {
                assert_eq!(p.sgr(*r), p.open(*r), "{p:?} {r:?}");
                assert_eq!(p.painted(*r, "abc"), p.paint(*r, "abc"), "{p:?} {r:?}");
            }
        }
        for p in PALETTES {
            for slot in 0..=255u8 {
                assert_eq!(
                    Painter::new(p).background(slot),
                    p.background_look(slot).sgr(),
                    "{p:?} {slot}"
                );
            }
            assert_eq!(Painter::new(p).reverse(), p.reverse_look().sgr());
        }
    }

    /// A program's background is the reader's own theme slot, spelled the way a terminal spells
    /// them - the eight, then the bright eight at `100` - and nothing past the sixteen; and
    /// reverse is the user block's one spelling. These are the bytes the screen writes.
    #[test]
    fn a_programs_background_and_reverse_are_the_bytes_they_were() {
        let p = Painter::new(Palette::Colour);
        for (slot, n) in (0u8..16).zip((40..48).chain(100..108)) {
            assert_eq!(p.background(slot), format!("\x1b[{n}m"), "slot {slot}");
        }
        assert_eq!(p.background(16), "");
        assert_eq!(p.background(255), "");
        assert_eq!(p.reverse(), "\x1b[7m");
        assert_eq!(p.reverse(), Palette::Colour.open(Role::Selected));
        let none = Painter::new(Palette::None);
        for slot in 0..16 {
            assert_eq!(none.background(slot), "", "slot {slot}");
        }
        assert_eq!(none.reverse(), "");
    }
}
