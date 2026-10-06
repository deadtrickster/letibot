//! The byte state machine: whatever a `read` handed us, applied to a screen.
//!
//! # Why this is a state machine and not a parser per chunk
//!
//! A pty hands a program's output over in whatever sizes the kernel felt like. `ESC [ 3 1 m`
//! arrives as `ESC [ 3` and then `1 m` constantly, and a character's three UTF-8 bytes are split
//! across two reads just as often. A parser that ran per chunk would print `[3` as **text** — a
//! screen showing the bytes of its own escape sequences, which is the defect this module exists
//! to make impossible. So the parser keeps what it has seen: a partial sequence, a partial
//! parameter list, a partial character, an unterminated string.
//!
//! The tests below feed the same bytes in one call and at **every** split point, and assert the
//! same events come out. That is the property, and it is asserted rather than assumed.
//!
//! # The subset, stated
//!
//! | family | handled |
//! |---|---|
//! | `C0` | `BEL BS HT LF VT FF CR ESC` executed or dropped, never printed |
//! | `ESC` | `7 8 D E M c = > H N O \` and the designators `( ) * + # %` |
//! | `CSI` | parameters, `;` and `:` separators, one intermediate, the private markers `< = > ?` |
//! | `OSC` | consumed to `BEL` or `ST`, content dropped |
//! | `DCS SOS PM APC` | consumed to `ST`, content dropped |
//! | `C1` | `CSI` (`0x9b`), `OSC` (`0x9d`), `DCS`/`SOS`/`PM`/`APC`, the rest dropped |
//! | UTF-8 | decoded across feeds; a byte that cannot complete becomes one `U+FFFD` |
//!
//! What is *not* handled, and why it costs nothing here:
//!
//! - **`SO`/`SI` and the `G1`–`G3` designators.** `ESC ( 0` is honoured — that is terminfo's
//!   `smacs`, which is how `mc` and `less` draw a box — and the rest of the charset machinery is
//!   not, because a program that means to switch charsets on this box sends `smacs`.
//! - **A reply to anything.** `CSI 6n` (report cursor position), `CSI 5n` and `CSI c` (device
//!   attributes) are *received and dropped*: this crate has no output path by design, so a
//!   program that waits for an answer waits. `mc` does not ask; see `Screen`'s header for the
//!   one place that is a real gap.
//! - **`DECALN`** (`ESC # 8`) and the other test sequences: consumed, no effect.

/// The most parameters a sequence can carry, which is `xterm`'s own order of magnitude.
///
/// A longer list is **not** a parse error: the extra parameters are dropped and the ones before
/// them still apply. Dropping rather than refusing matters for `SGR`, where a truncated list must
/// not turn a later `1` into a bold — see [`crate::attr::apply_sgr`], which consumes the
/// extended-colour forms whole for the same reason.
pub const MAX_PARAMS: usize = 16;

/// One complete `CSI` sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Csi {
    /// The private marker, one of `< = > ?`, if the program wrote one.
    pub private: Option<u8>,
    /// The intermediate byte — the space of `CSI 2 SP q` is the one that occurs in practice.
    pub intermediate: Option<u8>,
    /// The final byte, `0x40`–`0x7e`. What the sequence *is*.
    pub final_byte: u8,
    /// The parameters, `;`-separated, with an empty field read as `0`.
    pub params: [u16; MAX_PARAMS],
    /// How many of `params` were set.
    pub len: u8,
    /// More parameters arrived than [`MAX_PARAMS`], and were dropped.
    pub overflow: bool,
}

impl Csi {
    /// The parameters the program wrote, in order.
    pub fn params(&self) -> &[u16] {
        &self.params[..self.len as usize]
    }

    /// Parameter `i`, or `None` when the program wrote none.
    ///
    /// **`None` and `Some(0)` are different**, and the sequences that act on them need the
    /// difference: `CSI J` erases below and `CSI 1 J` erases above, while `CSI H` is row 1 and
    /// `CSI 0 H` is row 1 because a zero parameter means the default. [`Csi::n`] is the accessor
    /// for the sequences where the two agree.
    pub fn param(&self, i: usize) -> Option<u16> {
        self.params().get(i).copied()
    }

    /// Parameter `i` as a count, with an absent or zero parameter read as `1` — the rule ECMA-48
    /// gives for every cursor movement and every insert/delete count.
    pub fn n(&self, i: usize) -> usize {
        match self.param(i) {
            None | Some(0) => 1,
            Some(v) => v as usize,
        }
    }

    /// Parameter `i` as a value for the sequences where `0` is real (`ED`, `EL`, `SGR`), with an
    /// absent one read as `0`.
    pub fn mode(&self, i: usize) -> u16 {
        self.param(i).unwrap_or(0)
    }

    /// Whether the private marker is `byte`.
    pub fn is_private(&self, byte: u8) -> bool {
        self.private == Some(byte)
    }
}

/// Something the byte stream asked the screen to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// A printable character, decoded and — under a designated graphics charset — translated.
    Print(char),
    /// A `C0` control to execute.
    Control(u8),
    /// A complete `CSI` sequence.
    Csi(Csi),
    /// The byte after `ESC`, for the sequences that are one byte long: `7 8 D E M c = > H N O \`.
    Esc(u8),
}

/// Where the machine is between bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum State {
    #[default]
    Ground,
    /// `ESC` seen; the next byte says which family.
    Esc,
    /// `ESC` and a byte in `0x20..=0x2f` — the designator of a charset. One more byte follows.
    Designator(u8),
    /// `CSI` seen; parameters, intermediates and the final byte follow.
    Csi,
    /// `ESC ]`: an `OSC`, ended by `BEL` or `ST`.
    Osc,
    /// `ESC P`/`X`/`^`/`_`: a string, ended by `ST` only.
    Str,
    /// `ESC` inside a string: `\` closes it as `ST`, anything else closes it too.
    StrEsc,
}

/// The incremental parser. Feed it bytes; it keeps whatever it could not finish.
#[derive(Clone, Debug, Default)]
pub struct Parser {
    state: State,
    params: [u16; MAX_PARAMS],
    len: u8,
    overflow: bool,
    /// The parameter being accumulated. `None` when no digit has been seen, which is how an
    /// absent parameter is told from a zero one.
    cur: Option<u32>,
    private: Option<u8>,
    intermediate: Option<u8>,
    /// Bytes of a character that is not complete yet.
    utf8: [u8; 4],
    utf8_len: u8,
    /// `G0` is the DEC special graphics set, as `ESC ( 0` selects it.
    graphics: bool,
}

/// What the pending bytes turned out to be.
enum Decided {
    /// A character that cannot be finished yet: keep the bytes.
    Incomplete,
    /// A complete character, `n` bytes long.
    Done(usize),
    /// `bad` bytes that are not a character, after `valid` bytes that are.
    Bad { valid: usize, bad: usize },
}

impl Parser {
    /// A parser at the start of a stream.
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Feed bytes, handing each complete thing to `f`.
    ///
    /// Bytes that complete nothing yet are **kept**, which is the whole point: a sequence split
    /// across two calls is one sequence.
    pub fn advance(&mut self, bytes: &[u8], mut f: impl FnMut(Event)) {
        for &b in bytes {
            if self.utf8_len > 0 {
                // **A continuation byte belongs to the character under way**, and it is checked
                // before the state machine because `0x80`–`0x9f` is both a continuation range and
                // the C1 controls: without this, the second byte of `日` would be read as a C1
                // control and the character would never arrive.
                if (0x80..=0xbf).contains(&b) && self.state == State::Ground {
                    self.utf8(b, &mut f);
                    continue;
                }
                // **A byte that cannot continue it ends it.** The pending bytes can never complete
                // now, and holding them would let a byte from much later finish a character the
                // program never sent — a wrong glyph rather than a replacement one.
                f(Event::Print('\u{fffd}'));
                self.utf8_len = 0;
            }
            match self.state {
                State::Ground => self.ground(b, &mut f),
                State::Esc => self.escape(b, &mut f),
                State::Designator(d) => self.designate(d, b),
                State::Csi => self.csi(b, &mut f),
                State::Osc => self.string(b, true),
                State::Str => self.string(b, false),
                State::StrEsc => match b {
                    0x1b => {}
                    b'\\' => self.state = State::Ground,
                    // An `ESC` in a string that is not followed by `ST` ends it, and the byte
                    // goes with it: a title is not something this crate keeps.
                    _ => self.state = State::Ground,
                },
            }
        }
    }

    fn ground(&mut self, b: u8, f: &mut impl FnMut(Event)) {
        match b {
            0x1b => self.state = State::Esc,
            // A C0 control is executed, never printed.
            0x00..=0x1f => f(Event::Control(b)),
            0x7f => {}
            // ASCII printable. `ESC ( 0` puts the DEC graphics glyphs in this range, which is how
            // a box gets drawn by a program that has no Unicode.
            0x20..=0x7e => {
                let c = if self.graphics {
                    dec_graphics(b)
                } else {
                    b as char
                };
                f(Event::Print(c));
            }
            // **The C1 controls are controls, not text.** The tree's sanitiser already reads them
            // that way, and `0x9b` is the 8-bit spelling of `CSI` — a real program writes it, and
            // rendering it as `U+FFFD` would be this crate printing junk where a terminal acts.
            0x80..=0x9f => match b {
                0x9b => self.start_csi(),
                0x9d => self.state = State::Osc,
                0x90 | 0x98 | 0x9e | 0x9f => self.state = State::Str,
                _ => {}
            },
            // A continuation byte or an overlong lead with nothing pending cannot start a
            // character, so it is one replacement character and no state.
            0xa0..=0xc1 => f(Event::Print('\u{fffd}')),
            0xc2..=0xff => self.utf8(b, f),
        }
    }

    fn escape(&mut self, b: u8, f: &mut impl FnMut(Event)) {
        match b {
            // A second `ESC` restarts: `ESC ESC [ 3 1 m` is a colour.
            0x1b => {}
            0x00..=0x1f => {
                f(Event::Control(b));
                self.state = State::Ground;
            }
            b'[' => self.start_csi(),
            b']' => self.state = State::Osc,
            b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
            // The designator byte of a charset, and every other intermediate: one more byte
            // follows it, and only `ESC ( 0` means anything here.
            0x20..=0x2f => self.state = State::Designator(b),
            // A final byte: the one-byte escape sequences.
            0x30..=0x7e => {
                f(Event::Esc(b));
                self.state = State::Ground;
            }
            0x7f => {}
            // Not a byte that can follow `ESC`: the escape is abandoned, and the byte with it.
            0x80..=0xff => self.state = State::Ground,
        }
    }

    fn designate(&mut self, designator: u8, b: u8) {
        // `G0` is the set a program on this box actually selects, because that is what terminfo's
        // `smacs` sends; `G1`–`G3` are consumed and ignored.
        if designator == b'(' {
            self.graphics = b == b'0';
        }
        self.state = State::Ground;
    }

    fn start_csi(&mut self) {
        self.state = State::Csi;
        self.params = [0; MAX_PARAMS];
        self.len = 0;
        self.overflow = false;
        self.cur = None;
        self.private = None;
        self.intermediate = None;
    }

    fn csi(&mut self, b: u8, f: &mut impl FnMut(Event)) {
        match b {
            // A new escape: the sequence under way is abandoned, as a terminal does.
            0x1b => self.state = State::Esc,
            // **A C0 control inside a sequence executes and the sequence continues.** ECMA-48
            // says so, and `ESC [ 3 \n H` is then a `CUP` with a line feed in the middle rather
            // than a lost sequence.
            0x00..=0x1f => f(Event::Control(b)),
            0x30..=0x39 => {
                // Saturating rather than wrapping: `ESC [ 99999999 H` is a row nobody has, and a
                // wrapped small number would be a *wrong* row rather than a clamped one.
                let d = (b - b'0') as u32;
                self.cur = Some(
                    self.cur
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(d)
                        .min(0xffff),
                );
            }
            // `:` is the sub-parameter separator. Read as `;`, which is what the one family of
            // programs that writes it means by it: `SGR 38:5:1` and `38;5;1` are one colour.
            b';' | b':' => self.push_param(),
            0x20..=0x2f => {
                // Only if a parameter was actually being written: `CSI 2 SP q` has one, and a
                // stray intermediate does not invent a zero.
                if self.cur.is_some() {
                    self.push_param();
                }
                self.intermediate = Some(b);
            }
            0x3c..=0x3f => {
                if self.private.is_none() && self.len == 0 && self.cur.is_none() {
                    self.private = Some(b);
                }
            }
            0x40..=0x7e => {
                // **Only a parameter that was written is a parameter.** `CSI H` has none and
                // `CSI 0 H` has one zero, and the difference is what makes `CSI J` (erase below)
                // different from `CSI 1 J` (erase above) and `CSI m` a reset.
                if self.cur.is_some() {
                    self.push_param();
                }
                f(Event::Csi(Csi {
                    private: self.private,
                    intermediate: self.intermediate,
                    final_byte: b,
                    params: self.params,
                    len: self.len,
                    overflow: self.overflow,
                }));
                self.state = State::Ground;
            }
            0x7f => {}
            // A byte that cannot appear in a sequence: the sequence is abandoned. Dropping it
            // rather than printing it is the rule of the whole module — half a sequence is never
            // text.
            0x80..=0xff => self.state = State::Ground,
        }
    }

    fn push_param(&mut self) {
        let v = self.cur.unwrap_or(0).min(0xffff) as u16;
        self.cur = None;
        if (self.len as usize) < MAX_PARAMS {
            self.params[self.len as usize] = v;
            self.len += 1;
        } else {
            self.overflow = true;
        }
    }

    /// `OSC` and the `DCS` family: consumed, and the content dropped.
    ///
    /// `bel_ends` is the one difference between them: a `BEL` ends an `OSC` and is content in a
    /// `DCS`.
    fn string(&mut self, b: u8, bel_ends: bool) {
        match b {
            0x1b => self.state = State::StrEsc,
            0x9c => self.state = State::Ground,
            0x07 if bel_ends => self.state = State::Ground,
            _ => {}
        }
    }

    /// Accumulate a character, emitting it only once it is complete.
    fn utf8(&mut self, b: u8, f: &mut impl FnMut(Event)) {
        if self.utf8_len as usize == self.utf8.len() {
            // Unreachable for well-formed input, and the guard is what keeps a bug in this
            // function from being an unbounded buffer.
            f(Event::Print('\u{fffd}'));
            self.utf8_len = 0;
        }
        self.utf8[self.utf8_len as usize] = b;
        self.utf8_len += 1;
        loop {
            let len = self.utf8_len as usize;
            // A local copy, so the borrow of `self.utf8` is over before anything is emitted and
            // the bytes can be examined again without allocating.
            let mut local = [0u8; 4];
            local.copy_from_slice(&self.utf8);
            let decided = match std::str::from_utf8(&local[..len]) {
                Ok(s) => Decided::Done(s.len()),
                Err(e) => match e.error_len() {
                    None => Decided::Incomplete,
                    Some(n) => Decided::Bad {
                        valid: e.valid_up_to(),
                        bad: n,
                    },
                },
            };
            match decided {
                Decided::Incomplete => return,
                Decided::Done(n) => {
                    self.utf8_len = 0;
                    for c in std::str::from_utf8(&local[..n]).unwrap_or_default().chars() {
                        f(Event::Print(c));
                    }
                    return;
                }
                Decided::Bad { valid, bad } => {
                    let keep = len - valid - bad;
                    for c in std::str::from_utf8(&local[..valid])
                        .unwrap_or_default()
                        .chars()
                    {
                        f(Event::Print(c));
                    }
                    // One replacement character per byte that is not a character, and the bytes
                    // after them are examined again rather than dropped.
                    for _ in 0..bad {
                        f(Event::Print('\u{fffd}'));
                    }
                    let mut tail = [0u8; 4];
                    tail[..keep].copy_from_slice(&local[valid + bad..len]);
                    self.utf8 = tail;
                    self.utf8_len = keep as u8;
                    if keep == 0 {
                        return;
                    }
                    // The tail is a character of its own; the next turn decides about it.
                }
            }
        }
    }
}

/// The DEC Special Graphics set, which `ESC ( 0` puts in place of ASCII `0x5f`–`0x7e`.
///
/// This is the table terminfo calls `acsc`, and it is why a box drawn by `mc` or `less` on a
/// terminal without Unicode arrives as `lqqqk` and has to come out as `┌───┐`.
fn dec_graphics(b: u8) -> char {
    match b {
        b'_' => ' ',
        b'`' => '◆',
        b'a' => '▒',
        b'b' => '␉',
        b'c' => '␌',
        b'd' => '␍',
        b'e' => '␊',
        b'f' => '°',
        b'g' => '±',
        b'h' => '␤',
        b'i' => '␋',
        b'j' => '┘',
        b'k' => '┐',
        b'l' => '┌',
        b'm' => '└',
        b'n' => '┼',
        b'o' => '⎺',
        b'p' => '⎻',
        b'q' => '─',
        b'r' => '⎼',
        b's' => '⎽',
        b't' => '├',
        b'u' => '┤',
        b'v' => '┴',
        b'w' => '┬',
        b'x' => '│',
        b'y' => '≤',
        b'z' => '≥',
        b'{' => 'π',
        b'|' => '≠',
        b'}' => '£',
        b'~' => '·',
        other => other as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything a stream produced, fed in the given chunks.
    fn events(chunks: &[&[u8]]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        for c in chunks {
            p.advance(c, |e| out.push(e));
        }
        out
    }

    /// The events a stream produces when it arrives in one piece.
    fn whole(bytes: &[u8]) -> Vec<Event> {
        events(&[bytes])
    }

    /// The one `CSI` a stream contains.
    fn csi(bytes: &[u8]) -> Csi {
        match whole(bytes)[0] {
            Event::Csi(c) => c,
            other => panic!("{bytes:?} is not a CSI: {other:?}"),
        }
    }

    /// The text a stream prints, and nothing else.
    fn printed(bytes: &[u8]) -> String {
        whole(bytes)
            .iter()
            .filter_map(|e| match e {
                Event::Print(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    /// **A stream is the same however it is split.**
    ///
    /// This is the classic defect and the reason this is a state machine: a `read` boundary inside
    /// `ESC [ 3 1 m`, inside a parameter list, inside an `OSC`, inside the bytes of a character.
    /// Feeding one byte at a time is the worst case a pty can produce, and it is asserted here
    /// alongside every single split point.
    #[test]
    fn a_stream_is_the_same_however_it_is_split() {
        let streams: Vec<&[u8]> = vec![
            b"\x1b[1;31mred\x1b[0m plain",
            b"\x1b[?1049h\x1b[2J\x1b[1;1H\x1b[?25lmc\x1b[?25h\x1b[?1049l",
            b"\x1b[38;5;167;48;2;1;2;3;4mX",
            b"\x1b]0;a title with \x1b inside\x07after",
            b"\x1bP1$r0m\x1b\\after",
            b"\x1b(0lqqk\x1b(B plain",
            b"caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e done",
            b"\x1b[12;34H\x1b[3;4r\x1b[2L\x1b[1M\x1b[K",
        ];
        for s in streams {
            let want = whole(s);
            let bytes: Vec<&[u8]> = s.chunks(1).collect();
            assert_eq!(events(&bytes), want, "byte-at-a-time differs for {s:?}");
            for i in 0..s.len() {
                let got = events(&[&s[..i], &s[i..]]);
                assert_eq!(got, want, "split at {i} differs for {s:?}");
            }
        }
    }

    /// **A character split across two feeds is one character**, including the four-byte case and
    /// the case where the split lands on a continuation byte.
    #[test]
    fn a_character_split_across_two_feeds_is_one_character() {
        let text = "caf\u{e9} \u{65e5}\u{672c} \u{1f980}";
        let bytes = text.as_bytes();
        let want = whole(bytes);
        assert_eq!(
            want.iter().filter(|e| matches!(e, Event::Print(_))).count(),
            text.chars().count(),
            "one event per character, not per byte"
        );
        for i in 0..bytes.len() {
            assert_eq!(events(&[&bytes[..i], &bytes[i..]]), want, "split at {i}");
        }
        // The three-byte character, one byte at a time.
        assert_eq!(
            events(&[b"\xe6", b"\x97", b"\xa5"]),
            vec![Event::Print('\u{65e5}')]
        );
    }

    /// **A character that can never complete becomes one replacement character**, rather than
    /// being held until some later byte finishes it — which would be a glyph the program never
    /// sent, and worse than a visible `�`.
    #[test]
    fn an_incomplete_character_is_abandoned_when_a_byte_that_cannot_continue_it_arrives() {
        assert_eq!(
            events(&[b"\xe6\x97", b"\x1b", b"[m"]),
            vec![
                Event::Print('\u{fffd}'),
                Event::Csi(Csi {
                    final_byte: b'm',
                    ..Csi::default()
                })
            ]
        );
        // And an invalid byte becomes one replacement character per byte, with the text after it
        // kept: `\xff` then `abc` is not one lost character.
        assert_eq!(printed(b"\xffabc"), "\u{fffd}abc");
        assert_eq!(printed(b"a\xc3\xc3\xa9b"), "a\u{fffd}\u{e9}b");
    }

    /// **A long parameter list, split inside it.** The parameters survive, and the ones past the
    /// cap are dropped rather than read as something else.
    #[test]
    fn a_long_parameter_list_survives_being_split() {
        let long = b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20m";
        let want = whole(long);
        for i in 0..long.len() {
            assert_eq!(events(&[&long[..i], &long[i..]]), want, "split at {i}");
        }
        let c = csi(long);
        assert_eq!(c.len as usize, MAX_PARAMS);
        assert!(c.overflow, "the drop must be visible to the caller");
        assert_eq!(c.param(0), Some(1));
        assert_eq!(c.param(15), Some(16));
        assert_eq!(c.param(16), None, "past the cap there is nothing");
        // A split *inside a number*, which is the case a per-chunk parse turns into two rows.
        assert_eq!(csi(b"\x1b[123H").param(0), Some(123));
        assert_eq!(events(&[b"\x1b[12", b"3H"]), whole(b"\x1b[123H"));
        // A number larger than a row is clamped, not wrapped.
        assert_eq!(csi(b"\x1b[99999999H").param(0), Some(0xffff));
    }

    /// **An absent parameter and a zero one are different**, which is why they are kept apart all
    /// the way through: `ESC [ H` is row 1 by default and `ESC [ 0 H` is row 1 because zero means
    /// default, but `ESC [ J` erases below while `ESC [ 1 J` erases above.
    #[test]
    fn an_absent_parameter_is_not_a_zero_parameter() {
        assert_eq!(csi(b"\x1b[H").param(0), None);
        assert_eq!(csi(b"\x1b[H").n(0), 1);
        assert_eq!(csi(b"\x1b[0H").param(0), Some(0));
        assert_eq!(csi(b"\x1b[0H").n(0), 1);
        assert_eq!(csi(b"\x1b[J").mode(0), 0);
        assert_eq!(csi(b"\x1b[1J").mode(0), 1);
        // An empty field inside a list is a zero, as ECMA-48 says and as the tree's own sanitiser
        // already reads it.
        assert_eq!(csi(b"\x1b[;5H").params(), &[0u16, 5][..]);
        assert_eq!(csi(b"\x1b[5;H").params(), &[5u16][..]);
        assert_eq!(
            csi(b"\x1b[5;H").n(1),
            1,
            "a trailing empty field is the default"
        );
    }

    /// **The markers around the parameters are kept**, because `?1049h` and `1049h` are two
    /// different things and `CSI 2 SP q` is not `CSI 2 q`.
    #[test]
    fn the_private_marker_and_the_intermediate_are_part_of_the_sequence() {
        let c = csi(b"\x1b[?1049h");
        assert!(c.is_private(b'?'));
        assert_eq!(c.final_byte, b'h');
        assert_eq!(c.params(), &[1049u16][..]);
        let c = csi(b"\x1b[2 q");
        assert_eq!(c.intermediate, Some(b' '));
        assert_eq!(c.final_byte, b'q');
        assert_eq!(c.private, None);
        // `CSI > 4 ; 2 m` is xterm's keyboard mode and is marked, so the screen can drop it
        // instead of reading it as an SGR.
        let c = csi(b"\x1b[>4;2m");
        assert!(c.is_private(b'>'));
        assert_eq!(c.params(), &[4u16, 2][..]);
        // The sub-parameter form, which one family of programs writes for a colour.
        assert_eq!(csi(b"\x1b[38:5:167m").params(), &[38u16, 5, 167][..]);
    }

    /// **A malformed or unknown sequence produces no text.** Every one of these is a byte pattern
    /// a real program or a hostile payload can produce, and none of them may reach a cell as
    /// printable characters.
    #[test]
    fn a_malformed_sequence_is_dropped_and_never_printed() {
        for (hostile, forbidden, text) in [
            (&b"a\x1b[12b"[..], &["[12", "[1"][..], "a"),
            (b"a\x1b\xff b", &["\u{fffd}"], "a b"),
            (b"a\x1b[1;\x80 b", &["[1;", "[1"], "a b"),
            (
                b"a\x1b]0;unterminated title",
                &["]0;", "unterminated", "title"],
                "a",
            ),
            (b"a\x1bP unended dcs", &["P ", "unended", "dcs"], "a"),
            (b"a\x1b", &["["], "a"),
            (b"a\x1b\x07b", &["\u{7}"], "ab"),
        ] {
            let got = printed(hostile);
            for bad in forbidden {
                assert!(
                    !got.contains(bad),
                    "{bad:?} reached the screen as text for {hostile:?}: {got:?}"
                );
            }
            assert_eq!(
                got, text,
                "the text around the sequence is kept and nothing else is: {hostile:?}"
            );
        }
    }

    /// **`ESC ( 0` is the box-drawing charset, and it is why `mc`'s frames arrive as letters.**
    ///
    /// `lqqqk` is `┌───┐` in DEC Special Graphics, which is what terminfo's `smacs`/`acsc` pair
    /// exists for. `ESC ( B` puts ASCII back.
    #[test]
    fn the_dec_graphics_charset_turns_a_box_drawn_in_ascii_into_a_box() {
        assert_eq!(printed(b"\x1b(0lqqqk\x1b(B"), "┌───┐");
        assert_eq!(printed(b"\x1b(0x\x1b(B"), "│");
        assert_eq!(printed(b"\x1b(0_"), " ");
        assert_eq!(events(&[b"\x1b(", b"0lqqk"]), whole(b"\x1b(0lqqk"));
        // A charset that is not the graphics one is ASCII, and `lqqk` stays `lqqk`.
        assert_eq!(printed(b"\x1b(Blqqk"), "lqqk");
        // `G1` is not honoured, and saying so is the point: a program that selects it here gets
        // ASCII rather than a guess at what it meant.
        assert_eq!(printed(b"\x1b)0lqqk"), "lqqk");
    }

    /// **The controls a screen acts on arrive as controls**, and a C0 inside a sequence does not
    /// take the sequence with it.
    #[test]
    fn controls_arrive_as_controls_and_survive_a_sequence() {
        assert_eq!(
            whole(b"\r\n\x08\x07"),
            vec![
                Event::Control(b'\r'),
                Event::Control(b'\n'),
                Event::Control(0x08),
                Event::Control(0x07),
            ]
        );
        // `ESC [ 3 \n H` is a CUP with a line feed in the middle, per ECMA-48.
        let got = whole(b"\x1b[3\nH");
        assert_eq!(got[0], Event::Control(b'\n'));
        assert_eq!(got[1], Event::Csi(csi(b"\x1b[3H")));
    }

    /// **The 8-bit `CSI` is a `CSI`**, because a program writes it and because the tree's own
    /// sanitiser already reads it that way — rendering it as a replacement character would be
    /// this crate printing junk where a terminal acts.
    #[test]
    fn the_eight_bit_c1_forms_are_the_same_sequences() {
        assert_eq!(whole(b"\x9b31m"), whole(b"\x1b[31m"));
        assert_eq!(
            whole(b"\x9d0;t\x07"),
            vec![],
            "an OSC is consumed either way"
        );
        assert_eq!(whole(b"\x9b"), vec![], "a lone CSI at the end is nothing");
        // And a C1 byte that is not one of the families is dropped rather than printed.
        assert_eq!(whole(b"a\x85b"), vec![Event::Print('a'), Event::Print('b')]);
    }
}
