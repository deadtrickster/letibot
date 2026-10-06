//! The pen: the sixteen foreground slots and four attributes, and the one walk that reads
//! an SGR parameter list into them.
//!
//! # This is the terminal's vocabulary, and it is the only one of it
//!
//! A cell carries what the program **said**: a foreground slot `0`–`15`, bold, dim, reverse.
//! Nothing here names a meaning. The head's vocabulary is `letibot_ui::style::Role` —
//! *"something failed"*, *"this is syntax"* — and the mapping between the two lives in
//! exactly one place, `letibot_ui::ansi`. That is why there is no `Role` in this crate and
//! must not be: a screen model that knew what red *means* could not be reused by a head
//! that means something else by it, and `Role::Failure` is `31` in *this* palette rather
//! than in every one.
//!
//! # The walk moved down here, and why
//!
//! It used to be `letibot_ui::ansi`'s `Wanted`/`apply`, written for one reader: a line of a
//! foreign program's output, painted as roles. **A screen is a second reader of the same
//! parameters**, and a second copy of this table would be a second answer to *"what did
//! `38;5;1` mean"* — the duplicate this tree keeps deleting. So the walk moved to the crate
//! both readers can see, and `ansi.rs` keeps the half that is the head's: which role a pen
//! is drawn as.
//!
//! The proof that the move changed nothing is `ansi.rs`'s own tests, which are byte-exact
//! and include the operator's real `ls -la`: they were not touched, and they pass.
//!
//! # What is deliberately not carried, and what that costs
//!
//! - **A background** (`40`–`47`, `100`–`107`, `48;5;n`, `48;2;…`). It is consumed whole and
//!   nothing is set, which is the rule `ansi.rs` already had: the head's palette has no
//!   background for a body cell, and a program's own background would put a colour *beside*
//!   the reader's theme rather than within it. **The consequence for the pane is real and
//!   worth stating**: `mc`'s classic blue panels are a background, so the pane shows their
//!   text on the transcript's own background, and a `top`-style full-screen bar that is
//!   only a background is invisible.
//! - **A 256-colour or truecolour value** (`38;5;167`, `38;2;r;g;b`). The cube's indices are
//!   absolute RGB and a slot is a theme position, so the extended form is consumed and the
//!   foreground is left as it was — **not** guessed at, and not reset. Consuming it whole is
//!   the load-bearing half: `38;5;1`'s last parameter is the `1` that means bold, and reading
//!   the parameters one by one would turn a colour this palette cannot name into an attribute
//!   it can.
//! - **Underline, italic, blink, conceal, strike, overline** (`4`, `3`, `5`, `8`, `9`, `53`)
//!   and the font selector (`10`–`19`). A cell carries four attributes and these are not
//!   among them, so a program that underlines a menu accelerator draws it plain. Adding one
//!   is a field on [`Attr`] and a decision in the head, not a rewrite.
//!
//! # Brightness is not a second hue
//!
//! Slots `8`–`15` are the bright half of the sixteen and [`Attr::hue`] takes the intensity
//! off them, because that is what the reader on the other side does with them: there is no
//! role per slot, and `ansi.rs` draws a bright red as `Role::Failure` for the same reason it
//! draws a bold blue as `Role::Subheading`. Which of the sixteen a program asked for is still
//! in [`Attr::fg`] for a head that wants to tell them apart.

/// The hue of a foreground slot, with the intensity taken off.
///
/// Eight names for sixteen slots: `slot % 8` is the hue and `slot >= 8` is "bright", which is
/// a property of the *slot* rather than of the colour a theme puts in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hue {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl Hue {
    /// The hue of slot `0`–`15`. Slots above `15` wrap, which no SGR sequence can produce.
    pub fn of_slot(slot: u8) -> Hue {
        match slot % 8 {
            0 => Hue::Black,
            1 => Hue::Red,
            2 => Hue::Green,
            3 => Hue::Yellow,
            4 => Hue::Blue,
            5 => Hue::Magenta,
            6 => Hue::Cyan,
            _ => Hue::White,
        }
    }
}

/// What a program asked the pen to be.
///
/// `Default` is what a reset (`SGR 0`) leaves behind: no colour of its own, no attributes —
/// the terminal's own foreground and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attr {
    /// The foreground slot, `0`–`15`, or `None` for the terminal's default foreground.
    pub fg: Option<u8>,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

impl Attr {
    /// Whether this is the terminal's own foreground with no attributes — what a blank cell has.
    pub fn is_default(self) -> bool {
        self == Attr::default()
    }

    /// The hue of the foreground, or `None` when the program asked for no colour of its own.
    ///
    /// **This is the reduction a reader with a role per hue needs**, and the intensity is
    /// deliberately gone: `ansi.rs` has one role per hue, so `31` and `91` are the same red
    /// there, and `Attr::fg` still holds which of the two was asked for.
    pub fn hue(self) -> Option<Hue> {
        self.fg.map(Hue::of_slot)
    }
}

/// Read one SGR parameter list into `pen`.
///
/// Returns **whether a full reset (`0`) was among the parameters**, which is the one thing a
/// caller tracking an *open span* needs and cannot recover afterwards: a reset ends the span a
/// terminal would have ended, so a reader that is remembering "what the current run is painted
/// in" must forget it. It is a return value rather than a field because it is a fact about the
/// *sequence*, not about the pen.
///
/// A parameter this does not know is **ignored and the rest of the sequence still applies**:
/// `4;31` is an underline no field here can hold and a red it can, and dropping the red over
/// the underline would be the worse answer.
pub fn apply_sgr(params: &[u16], pen: &mut Attr) -> bool {
    let mut reset = false;
    let mut i = 0usize;
    while i < params.len() {
        let p = params[i];
        i += 1;
        match p {
            // Reset, and the two "back to normal" codes a program writes instead of it.
            0 => {
                *pen = Attr::default();
                reset = true;
            }
            22 => {
                pen.bold = false;
                pen.dim = false;
            }
            1 => pen.bold = true,
            2 => pen.dim = true,
            7 => pen.reverse = true,
            27 => pen.reverse = false,
            30..=37 => pen.fg = Some((p - 30) as u8),
            // The default foreground, which is where a reset of the colour alone goes.
            39 => pen.fg = None,
            // The bright half. Same hues, and the slot says which half.
            90..=97 => pen.fg = Some((p - 90 + 8) as u8),
            // **A background is consumed and not carried.** See the module header: the pane
            // will not show `mc`'s blue panels, and that is the stated cost rather than a
            // colour chosen on the program's behalf.
            40..=47 | 49 | 100..=107 => {}
            // **An extended colour is consumed whole and paints nothing.** `38;5;n` and
            // `38;2;r;g;b` carry parameters that are *not* SGR codes, and reading them one by
            // one would take the `1` of `38;5;1` for a bold — which is a colour mistake this
            // walk would be making rather than the program.
            38 | 48 => match params.get(i) {
                Some(5) => i += 2,
                Some(2) => i += 4,
                _ => i += 1,
            },
            _ => {}
        }
    }
    reset
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The table, at the level it is written**: a slot, its hue, and the intensity the
    /// hue does not carry.
    #[test]
    fn the_sixteen_slots_are_eight_hues_at_two_intensities() {
        assert_eq!(Hue::of_slot(1), Hue::Red);
        assert_eq!(Hue::of_slot(9), Hue::Red, "bright red is red");
        assert_eq!(Hue::of_slot(0), Hue::Black);
        assert_eq!(Hue::of_slot(15), Hue::White, "bright white is white");
        assert_eq!(Hue::of_slot(6), Hue::Cyan);
        assert_eq!(Hue::of_slot(14), Hue::Cyan);
        // And the slot itself survives, because a head that wants to tell `31` from `91`
        // is entitled to.
        let mut pen = Attr::default();
        apply_sgr(&[91], &mut pen);
        assert_eq!(pen.fg, Some(9));
        assert_eq!(pen.hue(), Some(Hue::Red));
    }

    /// **What the parameters set, one family at a time** — the walk's own contract, asserted
    /// here rather than only through the role table above it.
    #[test]
    fn a_parameter_list_sets_exactly_what_it_names() {
        let mut pen = Attr::default();
        // `ls`'s directory colour: bold blue, as a slot.
        apply_sgr(&[1, 34], &mut pen);
        assert_eq!(
            pen,
            Attr {
                fg: Some(4),
                bold: true,
                ..Attr::default()
            }
        );
        // `22` clears both weights and leaves the colour, which is what a program means by it.
        apply_sgr(&[22], &mut pen);
        assert!(!pen.bold && !pen.dim && pen.fg == Some(4));
        // `39` is the colour's own reset.
        apply_sgr(&[39], &mut pen);
        assert_eq!(pen.fg, None);
        // Reverse, both ways, and dim.
        apply_sgr(&[7], &mut pen);
        assert!(pen.reverse);
        apply_sgr(&[2], &mut pen);
        assert!(pen.dim);
        apply_sgr(&[27], &mut pen);
        assert!(!pen.reverse);
        // And `0` is everything, which the return value reports.
        assert!(
            apply_sgr(&[0], &mut pen),
            "a reset must say that it was one"
        );
        assert_eq!(pen, Attr::default());
        assert!(!apply_sgr(&[31], &mut pen), "a colour is not a reset");
    }

    /// **A parameter this walk does not know does not take the rest of the sequence with it.**
    /// `4;31` is the case: no field here can hold an underline, and the red is the part the
    /// reader can still be told about.
    #[test]
    fn an_unknown_parameter_does_not_drop_the_ones_after_it() {
        let mut pen = Attr::default();
        apply_sgr(&[4, 31], &mut pen);
        assert_eq!(pen.hue(), Some(Hue::Red));
        // The same for a code that is only meaningful to a terminal we are not: `5` blink.
        apply_sgr(&[5, 42, 33], &mut pen);
        assert_eq!(pen.hue(), Some(Hue::Yellow));
        // A background is consumed and the foreground beside it still applies.
        apply_sgr(&[44, 36], &mut pen);
        assert_eq!(pen.hue(), Some(Hue::Cyan));
    }

    /// **An extended colour is consumed whole, and its parameters are not read as codes.**
    ///
    /// The trap is `38;5;1`: the `1` is the cube's index and also the code for bold, so a walk
    /// that read the list one parameter at a time would turn a colour it cannot name into an
    /// attribute it can. Nothing is set — not even the foreground, which is left as the
    /// program last set it.
    #[test]
    fn an_extended_colour_is_consumed_whole_and_paints_nothing() {
        let mut pen = Attr::default();
        apply_sgr(&[38, 5, 1], &mut pen);
        assert_eq!(
            pen,
            Attr::default(),
            "the cube's index 1 is neither a bold nor a colour this palette can name"
        );
        // The truecolour form, whose four extra parameters are not codes either — the `0`
        // among them is not a reset and the `2` is not a dim.
        apply_sgr(&[38, 2, 255, 0, 0], &mut pen);
        assert_eq!(pen, Attr::default());
        // A background form is consumed by the same rule, and `48;5;22`'s `22` is not a
        // "bold off".
        let mut pen = Attr {
            bold: true,
            ..Attr::default()
        };
        apply_sgr(&[48, 5, 22], &mut pen);
        assert!(pen.bold, "the cube's index 22 is not `22`");
        // And the parameters after the extended form still apply.
        apply_sgr(&[38, 5, 167, 32], &mut pen);
        assert_eq!(pen.hue(), Some(Hue::Green));
    }

    /// **A reset says so, and an extended colour that *contains* a zero does not.**
    ///
    /// The distinction is why the walk returns the flag instead of a caller scanning the
    /// parameter list for a `0`: `38;5;0` is a parameter list with a zero in it and is not a
    /// reset.
    #[test]
    fn only_a_bare_zero_is_a_reset() {
        let mut pen = Attr {
            bold: true,
            ..Attr::default()
        };
        assert!(!apply_sgr(&[38, 5, 0], &mut pen));
        assert!(pen.bold, "the zero inside an extended colour reset the pen");
        assert!(apply_sgr(&[0], &mut pen));
        assert!(
            apply_sgr(&[1, 0, 31], &mut pen),
            "a zero later in the list is still one"
        );
        assert_eq!(pen.hue(), Some(Hue::Red));
        assert!(!pen.bold, "and it reset everything before the red");
    }
}
