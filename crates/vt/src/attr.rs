//! The pen: the sixteen foreground and background slots and four attributes, and the one walk
//! that reads an SGR parameter list into them. Nothing here names a meaning -- the head's role
//! table does, in `letibot_ui::ansi`.

/// The hue of a foreground slot, with the intensity taken off: `slot % 8` names the hue and
/// `slot >= 8` says bright, which is a property of the slot rather than of the theme's colour.
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

/// What a program asked the pen to be. `Default` is what a reset leaves behind: no colour of its
/// own and no attributes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attr {
    /// The foreground slot, `0`–`15`, or `None` for the terminal's default foreground.
    pub fg: Option<u8>,
    /// The background slot, or `None` for the terminal's default. A screen needs it and a payload
    /// line does not, and it is a slot rather than a colour because the sixteen are theme
    /// positions.
    pub bg: Option<u8>,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

impl Attr {
    /// Whether this is the terminal's own pen with no attributes — what a blank cell has.
    pub fn is_default(self) -> bool {
        self == Attr::default()
    }

    /// The hue of the foreground, or `None` when the program asked for no colour of its own. The
    /// intensity is gone because a reader with one role per hue does not want it; `fg` still holds
    /// which of the two was asked for.
    pub fn hue(self) -> Option<Hue> {
        self.fg.map(Hue::of_slot)
    }

    /// The hue of the background, or `None` when the program asked for none: a reader that wants
    /// to ask which colour a panel is rather than which of the sixteen.
    pub fn bg_hue(self) -> Option<Hue> {
        self.bg.map(Hue::of_slot)
    }
}

/// Read one SGR parameter list into `pen`, answering whether a full reset was among the
/// parameters. A parameter this does not know is ignored and the rest still applies.
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
            // The background, carried the same way: `mc`'s panels are `44` and a pane that
            // dropped the slot would draw their text on the transcript's own background. The slot
            // is what is kept.
            40..=47 => pen.bg = Some((p - 40) as u8),
            // The background's own reset, as `39` is the foreground's.
            49 => pen.bg = None,
            100..=107 => pen.bg = Some((p - 100 + 8) as u8),
            // An extended colour is consumed whole and paints nothing: its parameters are not
            // SGR codes, and reading them one by one would take the `1` of `38;5;1` for a bold.
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
        // The background is a slot of its own, set and reset on its own code.
        apply_sgr(&[44], &mut pen);
        assert_eq!(pen.bg, Some(4));
        apply_sgr(&[49], &mut pen);
        assert_eq!(pen.bg, None, "`49` is the background's own reset");
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
        // A background and a foreground in one list are two slots, and neither takes the other.
        apply_sgr(&[44, 36], &mut pen);
        assert_eq!(pen.hue(), Some(Hue::Cyan));
        assert_eq!(pen.bg_hue(), Some(Hue::Blue));
    }

    /// An extended colour is consumed whole and its parameters are not read as codes: the `1` of
    /// `38;5;1` is a cube index and also the code for bold. Nothing is set, not even the
    /// foreground.
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

    /// A background is carried the way a foreground is, and the bright half is a slot rather than
    /// a second colour. This is the slot `mc`'s panels and `nano`'s status bar are made of.
    #[test]
    fn a_background_slot_is_carried_and_the_bright_half_is_the_same_hue() {
        let mut pen = Attr::default();
        apply_sgr(&[44], &mut pen);
        assert_eq!(pen.bg, Some(4), "`mc`'s blue panel");
        assert_eq!(pen.bg_hue(), Some(Hue::Blue));
        // The bright half is a different *slot* and the same hue, exactly as `31`/`91` are.
        apply_sgr(&[104], &mut pen);
        assert_eq!(pen.bg, Some(12));
        assert_eq!(pen.bg_hue(), Some(Hue::Blue));
        // `49` clears the background and leaves the foreground beside it alone — the two slots have
        // their own reset codes and neither is the other's.
        apply_sgr(&[31, 49], &mut pen);
        assert_eq!(pen.bg, None);
        assert_eq!(pen.fg, Some(1));
        // A full reset takes it with everything else.
        apply_sgr(&[44, 0], &mut pen);
        assert_eq!(pen, Attr::default());
    }

    /// **A 256-colour or truecolour background is consumed whole and paints nothing**, which is the
    /// same rule as the foreground's and for the same reason one slot over: `48;5;1`'s `1` is the
    /// cube's index and also the code for bold, and `48;2;255;0;0`'s zero is not a reset.
    #[test]
    fn an_extended_background_is_consumed_whole_and_sets_no_slot() {
        let mut pen = Attr::default();
        apply_sgr(&[48, 5, 1], &mut pen);
        assert_eq!(
            pen,
            Attr::default(),
            "the cube's index 1 is neither a bold nor a background this palette can name"
        );
        apply_sgr(&[48, 2, 255, 0, 0], &mut pen);
        assert_eq!(
            pen,
            Attr::default(),
            "and the truecolour form's zero is not one"
        );
        // The parameters *after* the extended form still apply, and the background beside them is
        // not set on the way past.
        apply_sgr(&[48, 5, 167, 44], &mut pen);
        assert_eq!(
            pen.bg,
            Some(4),
            "the extended form consumed its own parameters and no more"
        );
        assert_eq!(pen.fg, None, "and `48` is not `38`");
    }

    /// **A background does not disturb reverse, and reverse does not disturb a background.** They
    /// are the two things a screen needs that a payload row does not, and `mc`'s selected row is
    /// both at once: a reverse run inside a blue panel.
    #[test]
    fn reverse_and_a_background_are_independent() {
        let mut pen = Attr::default();
        apply_sgr(&[7, 44, 31], &mut pen);
        assert!(pen.reverse, "a background does not clear reverse");
        assert_eq!(pen.bg, Some(4));
        assert_eq!(pen.fg, Some(1));
        apply_sgr(&[27], &mut pen);
        assert!(!pen.reverse);
        assert_eq!(pen.bg, Some(4), "`27` is reverse off, not everything off");
        apply_sgr(&[49], &mut pen);
        assert_eq!(pen.fg, Some(1), "and `49` is the background's own code");
        // A background alone is enough to make a cell not blank, which is what the frame's row
        // trim reads: the edge of `mc`'s panel is a *space* with a pen on it.
        let mut pen = Attr::default();
        apply_sgr(&[44], &mut pen);
        assert!(
            !pen.is_default(),
            "a blank cell with a background is not blank"
        );
    }

    /// A reset says so, and an extended colour that contains a zero does not: the walk returns the
    /// flag rather than a caller scanning for a `0`, since `38;5;0` has one and is not a reset.
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
