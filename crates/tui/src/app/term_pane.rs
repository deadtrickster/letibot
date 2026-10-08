//! **The terminal pane's state**: the program it shows, what it said last, and the ask it puts
//! to the person. The pane's way out is [`WAY_OUT`].

use super::*;

/// **The way out of a pane: `Ctrl-\`.** See [`TermPane`] for why this byte and not `Esc`.
///
/// It is looked for on the raw byte stream, before anything is forwarded, so the program never
/// receives it — see [`App::pane_keys`], and `Link::tick`, which looks for it on every read and
/// not only on the reads where the pane was already open. `0x1c` is `FS` in ASCII and `QUIT`
/// only under `ISIG`, which raw mode clears; it is one of the three bytes `term.rs`'s decoder
/// has no arm for, and its own comment says so.
///
/// `pub` because the interception is the driver's as much as the app's: the byte is a fact
/// about the stream, and `Link::tick` is where the stream is routed.
pub const WAY_OUT: u8 = 0x1c;

/// **The pane: a program that owns the screen, drawn in the conversation's rectangle.**
///
/// # What it is, and the two things it is not
///
/// It is [`letibot_vt::Screen`] — a rectangle of cells, a cursor, a pen and an alternate
/// buffer, driven by the bytes a pty's far end wrote — plus the three facts a head needs about
/// the program that is drawing in it. **It is not an emulator of this head's own**: there is one
/// in `letibot-vt`, it is a crate *below* this one, and this head's half of it is
/// `letibot_ui::ansi::pane_rows`. And it
/// is not the conversation: nothing here is a transcript row, and when the pane closes the
/// transcript is exactly what it was.
///
/// # The rectangle is the contract
///
/// [`TermPane::rows`] is `letibot_ui::ansi::pane_rows(&mut screen, cols, room, palette)` and it
/// returns **exactly `room` rows** — the same property `ansi.rs` keeps for the pane it was written for. That is the
/// whole of *"the composer, header and status keep their rows"*: the pane takes the
/// conversation's rectangle and gives it back, so nothing above it moves by a line when it
/// opens and nothing below it loses a row it had.
///
/// # The way out, and why it is `ctrl-\`
///
/// **`Ctrl-\` (0x1c), and it is intercepted on the raw byte stream before a single byte is
/// forwarded**, so the program never receives it and cannot trap it — which is the whole
/// requirement. The tree chose it long before this branch: `term.rs`'s own decoder lists the
/// bytes with no arm and says *"`0x1c`-`0x1e` are the only bytes left in this table with no
/// arm, and none of the three has a mnemonic worth having"*. It is not a tty control character
/// (`cfmakeraw` clears `IXON`/`IEXTEN`, and `0x1c` is `QUIT` only under `ISIG`, which raw mode
/// clears), it is not a chord this head binds, and it is not one a program expects to be
/// typed at it.
///
/// **Esc was the other candidate and it is wrong**: Esc is a key `vi`, `mc`, `nano` and every
/// `less` read on purpose — it is *cancel*, it is the first byte of every meta sequence, and a
/// pane that ate it would be a pane the program could not be driven from. `Ctrl-\` is the one
/// key whose whole meaning is *stop this*, and the operator asked for exactly that: *"one
/// unambiguous way out."*
///
/// # What `ctrl-\` does, and what it deliberately does not
///
/// **It detaches. It does not end anything.** The rectangle goes, the conversation comes back,
/// the composer has its rows and its keys — and **nothing is sent at all**: the program keeps
/// running on the daemon's pty, the daemon keeps the screen it has been keeping since protocol
/// 32, the slot stays occupied, and a later `!term` attaches back to the same run.
///
/// **That is a correction, and the operator's words are the reason:** *"but i dont want it to
/// exit"*. This key used to send [`Action::TermClose`], which ends the pane's cgroup — so
/// leaving `nano` killed it, and the attach work bought nothing for anything a person cares
/// about. **The default is the non-destructive act on purpose**: a person reaching for *get me
/// out of here* must not lose an hour's editing, and a program that must be ended can be asked
/// for by name.
///
/// **Ending is `!term close`**, typed at the composer (so it works whether the pane is on the
/// screen or not), and it is confirmed first — see [`App::term_ask`]. The two acts are as
/// different as they can be: one sends nothing and keeps everything, the other sends one frame
/// and kills a process tree, and neither is reachable by the other's key.
pub(crate) struct TermPane {
    /// The program's screen, fed the daemon's bytes and asked for rows.
    pub(crate) screen: letibot_vt::Screen,
    /// The line the operator submitted, verb included — kept for the one sentence this head
    /// says when the pane ends, so *what ended* is not a mystery.
    pub(crate) line: String,
    /// **The rectangle last sent to the daemon.** A resize is a frame, and a frame per tick
    /// would be a frame per keystroke — this is what makes `TermResize` fire on a change and
    /// not on a redraw.
    pub(crate) sent: (usize, usize),
    /// **The operator has left and an ending is on its way.** Set when the head sends
    /// [`Action::TermClose`] — the confirmed `!term close` — and it stops the keys: between the
    /// frame and the daemon's `TermEnded` there is a kill in flight, and a byte written into a
    /// pty whose program is being signalled is a byte nobody will read. The window is
    /// milliseconds, and this is what makes it closed rather than merely short.
    ///
    /// **Not the detach flag.** Leaving (`ctrl-\`) sends nothing and waits for nothing — see
    /// [`TermPane::detached`].
    pub(crate) closing: bool,
    /// **The operator left with `ctrl-\`, and the program is still running.**
    ///
    /// The rectangle is not drawn, the composer has its rows and its keys back — and the pane
    /// is **kept**: the bytes keep arriving into this screen, and the ending, whenever it comes,
    /// is filed as the row it would have been had the operator been looking. That is the whole
    /// of *detaching is not ending*, and it is why [`App::detach`] drops nothing.
    pub(crate) detached: bool,
}

impl TermPane {
    pub(crate) fn new(line: &str, cols: usize, rows: usize) -> TermPane {
        TermPane {
            screen: letibot_vt::Screen::new(rows, cols),
            line: line.to_string(),
            sent: (cols, rows),
            closing: false,
            detached: false,
        }
    }

    /// **The pane's rows for this frame** — exactly `room` of them, which is the property the
    /// composer's own row budget depends on. See the type's note.
    pub(crate) fn rows(
        &mut self,
        cols: usize,
        room: usize,
        palette: letibot_ui::style::Palette,
    ) -> Vec<String> {
        letibot_ui::ansi::pane_rows(&mut self.screen, cols, room, palette)
    }

    /// **The last rows the program left on the screen**, for the row its ending becomes.
    ///
    /// **The screen and not the byte stream**: what a program *printed* is what a person can
    /// read, and the bytes behind it are cursor addressing, `\r` and half a UTF-8 character —
    /// the raw stream is for [`letibot_vt::Screen`] and this is for a transcript row.
    ///
    /// **Blank rows are dropped and the tail is kept**, because the two shapes are different
    /// and the same rule answers both: a program that dies with a sentence about why put that
    /// sentence last (and its rectangle is otherwise empty), and a full-screen program leaves
    /// a grid of mostly blanks whose last rows are where its status line is. Capped at
    /// [`PANE_LAST_LINES`], which is what makes an unfolded note safe to draw.
    pub(crate) fn last_rows(&self) -> Vec<String> {
        let (rows, _) = self.screen.size();
        let mut said: Vec<String> = (0..rows)
            .map(|r| self.screen.line(r))
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if said.len() > PANE_LAST_LINES {
            said.drain(..said.len() - PANE_LAST_LINES);
        }
        said
    }
}

/// **What this head believes about the session's pane** — the daemon's answer to
/// [`ClientFrame::TermStatus`](letibot_sessionlog::protocol::ClientFrame::TermStatus), as this
/// head holds it.
///
/// # Why it is a three-state and not an `Option`
///
/// `None` would mean two different things at once, and the difference decides whether a
/// `!term close` **asks or refuses**:
///
/// * **`Unasked`** — nobody has answered yet. The head has just attached, or switched, and the
///   question is in flight. A verb that read this as *no pane* would refuse to end a program
///   the operator can see, which is the one case the read exists for;
/// * **`None`** — the daemon said there is no live pane. That is a fact, and it is what makes
///   `!term close` a sentence rather than a card;
/// * **`Running(command)`** — the daemon said this is running in it, and it is what a
///   confirmation names.
///
/// **A fact about NOW, refreshed rather than accumulated.** It is set by the three frames that
/// can know ([`ServerFrame::TermStatus`], `TermAttached`, `TermEnded`), reset to `Unasked` on
/// every `Hello` — a head that has just been seated somewhere does not know what is in the
/// pane there — and never derived from anything durable, because there is nothing durable about
/// it: a detach is not an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneFact {
    /// Nobody has asked yet: the read is in flight, or this head has not attached.
    Unasked,
    /// The daemon said this session has no live pane.
    None,
    /// The daemon said this is running in it.
    Running(String),
}

/// **The confirmation that ends a pane** — the daemon's own question about killing something,
/// and never the program's question about itself.
///
/// # Why it is not the prompt card
///
/// `SessionEvent::PromptRequested`'s card answers **the program**: it is a line written into a
/// pipe the daemon holds, drawn in the open, answered by typing and Enter. This card answers
/// **the daemon**: whether a process tree should die. The operator's rule is that the two must
/// not be confusable — *"a person must never be unsure which one they are looking at, and
/// neither may be answerable by the other's keystroke"* — so they are kept apart in every way a
/// person can see or type:
///
/// * **different words** — this one names the program and says what ending it does, and it
///   never quotes the program's own output (see [`App::term_ask_lines`]);
/// * **different key** — the yes here is `y`, and it is deliberately **not Enter**, because
///   Enter is the prompt card's own (an empty line is a real answer there) and the composer's.
///   A stray Enter cannot kill anything;
/// * **and the safe default** — *anything that is not a deliberate yes* cancels, Esc included.
///   A confirmation whose default is the destructive answer is not a confirmation.
///
/// **A prompt that arrives while this is up takes the screen back**, and the arm that does it
/// says why: a program that has just asked a question must not be killable by the answer to it.
///
/// **And it outranks a card that was already waiting**, which is this head's rule for two
/// questions at once rather than a choice made here: the newest question owns the screen and the
/// older one waits its turn (`App::decision_card`'s slot says the same thing about a decision
/// card under a picker). So a decision card that was up when the operator typed `!term close`
/// comes back when this is answered, cancelled or confirmed — and `esc` is the way out of the
/// stack, one question at a time, because *anything that is not a yes* cancels.
#[derive(Debug, Clone)]
pub(crate) struct TermAsk {
    /// **What is about to end, as a line a person reads** — `!term nano notes.txt`, the spelling
    /// the operator typed at the composer when there is one, or `!term <command>` rebuilt from
    /// the daemon's own word when the pane is another head's.
    pub(crate) line: String,
}
