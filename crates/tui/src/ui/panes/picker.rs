//! **The pickers**: sessions, and a setting's values.

use crate::app::*;
use crate::render::{sgr, trim_to, wrap};
use crate::ui::*;
use letibot_sessionlog::registry::short_id;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

impl App {
    pub(crate) fn picker_lines(&self, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let mut out = vec![
            colour(&self.cfg, sgr::BOLD, "sessions in this daemon"),
            String::new(),
        ];
        if self.sessions.is_empty() {
            out.push(dim(
                &self.cfg,
                "  none listed yet — the daemon has not answered, or this head is \
                 replaying a recorded log and has no daemon to ask.",
            ));
        }
        // **Through the one enumeration.** A sub-session is indented under the conversation that
        // spawned it, and a conversation with children carries the fold glyph every tree in this
        // head uses. The numbering is the ROW's, so the number a reader counts to is the number
        // `/switch N` takes.
        let rows = self.session_rows();
        let kids_of = |id: &str| {
            self.sessions
                .iter()
                .filter(|s| s.parent_session_id.as_deref() == Some(id))
                .count()
        };
        for (at, row) in rows.iter().enumerate() {
            let i = row.idx;
            let s = &self.sessions[i];
            let here = s.session_id == self.session_id;
            // The same ladder the decision prompt draws: the mark IS the thing Enter
            // takes, and the row it sits on is inverse. The session this head is in
            // keeps its bold name, so "where am I" and "what Enter takes" stay two
            // readable facts even when they are different rows.
            let picked = at == self.picker_sel.min(rows.len().saturating_sub(1));
            let mark = if picked { "▸" } else { " " };
            let name = if s.title.is_empty() {
                short_id(&s.session_id)
            } else {
                s.title.clone()
            };
            // **The fold is its own glyph beside the mark, and its COLUMN IS RESERVED either
            // way** — R58's tree is why. An empty string for a childless row reads as one column
            // saved, and that is what this did, with a comment arguing that *"a row without any
            // keeps the exact columns it had before sub-sessions were listed"*: true of a list
            // that had no tree in it at all, and the whole of the defect the moment one exists.
            //
            // MEASURED on this box 2026-10-05 with the instrument beside this pane's tests
            // (`show_the_sessions_pane_tree`), on a root with two children of which only one has
            // a child of its own:
            //
            // ```text
            //   ▸+  1  this conversation      name at col 7    depth 0, has children
            //      -  2  the first child       name at col 9    depth 1, has a child
            //         3  the grandchild        name at col 10   depth 2, no children
            //       4  the second child        name at col 8    depth 1, NO children
            // ```
            //
            // **Two siblings at ONE depth, one column apart** (9 against 8), so the name column
            // was a function of *has children* and not of *depth* — a list that cannot be read as
            // a tree, which is the one thing this pane is for. And 174 of the store's 280
            // sessions are down there. With the column reserved the step is the indent's own two
            // columns at every level, which is the same two **this head already steps by
            // everywhere else** (`card::REASONING_RAIL_WIDTH`, and the frame's own gutter): the
            // page reads as one repeated step rather than as a second, unrelated one.
            //
            // What it costs, said rather than left to be discovered: a screen with no children on
            // it moves one column right, because the column is now reserved for a glyph that is
            // not there. That is the trade — a flat list one column in, against a tree that can be
            // read.
            let kids = kids_of(&s.session_id);
            // **The fold is `+`/`-`, and it stopped being a triangle on purpose.**
            //
            // It was `▾`/`▸` — the same shapes the cursor is made of — so the one row where the
            // fold most needs to be read was the one row where it could not be: a PICKED row of a
            // folded conversation drew `▸▸`, two identical glyphs side by side doing two different
            // jobs, and the operator's report is exactly that (`▸▸` on the row the cursor is on).
            // The cursor keeps `▸` because it is the mark Enter takes, and the fold takes the pair
            // a tree has used for decades: one cell each, ASCII, and nothing like a cursor. The
            // columns do not move — this is a character, not a layout change.
            let fold = if kids == 0 {
                " "
            } else if self.family_open(&s.session_id, row.depth)
                || self.expanded.iter().any(|e| *e == s.session_id)
            {
                "-"
            } else {
                "+"
            };
            // **And the number's field is as wide as the list is long.** `{:>2}` is right for the
            // nine-row lists this was written against and wrong for this box, whose header reads
            // `1/106`: row 100 renders `100` in a two-wide field, so **every row from 100 on sits
            // one column right of every row before it** — the same defect as the fold above, in the
            // same frame, and MEASURED there too (rows 3–99 at column 6, rows 100–104 at column 7).
            // `max(2)` keeps a short list's frame exactly as it was.
            let digit_w = rows.len().to_string().len().max(2);
            let indent = "  ".repeat(row.depth.min(3));
            let left = format!(
                "{indent}{mark}{fold} {:>digit_w$}  {}",
                at + 1,
                p.paint(
                    if here { Role::Strong } else { Role::Plain },
                    &without_control_lines(&name),
                ),
            );
            let left = if picked {
                colour(&self.cfg, sgr::REVERSE, &left)
            } else {
                left
            };
            // Busy is the fact a picker exists to show: switching away from a
            // running turn is fine — the daemon keeps generating — and switching
            // *into* one is how you go back and watch it.
            let mut facts: Vec<String> = Vec::new();
            if s.status.running {
                facts.push("generating".into());
            }
            if !s.live {
                // The whole difference between a row that costs one keystroke and a
                // row that costs a resume. Said in a word rather than implied by an
                // absent "generating".
                facts.push("on disk".into());
            }
            // The store's count when there is one: the view's `items` is bounded by
            // `ViewBounds` and is the length of what a head is *holding*, not the
            // length of the conversation. Reporting the smaller number as "rows"
            // makes a long session look short.
            let rows = s.stored_items as usize;
            let rows = if rows > 0 { rows } else { s.status.items };
            if rows > 0 {
                facts.push(format!("{rows} rows"));
            }
            if s.status.heads > 0 {
                facts.push(format!(
                    "{} head{}",
                    s.status.heads,
                    if s.status.heads == 1 { "" } else { "s" }
                ));
            }
            if !s.wiring.model.is_empty() {
                facts.push(s.wiring.model.clone());
            }
            let right = p.paint(
                if s.status.running {
                    Role::Pending
                } else {
                    Role::Faint
                },
                &facts.join(" · "),
            );
            out.push(trim_to(&split_row(&left, &right, w), w));
            // The full id under **every** row, not only the named ones. It used to be
            // printed only when a title had displaced it, so the sessions whose id
            // you might actually need to type — the unnamed ones, the ones you would
            // pass to `letibot --session` — were the ones showing a truncation.
            //
            // The workspace goes on the same line. For a stored session it is the
            // only thing on the row that says what the conversation was *about*: an
            // unnamed session shows a short id and a model alias every other row also
            // has, and two of those are indistinguishable until you switch into one.
            let under = if s.wiring.workspace.is_empty() {
                format!("      {}", s.session_id)
            } else {
                format!("      {}  {}", s.session_id, tilde(&s.wiring.workspace))
            };
            out.push(trim_to(&dim(&self.cfg, &under), w));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  ↑↓ moves · enter switches · or type a number or part of a name and press \
             enter · /new [title] makes one · /rename NAME names this one · esc closes",
        ));
        out.push(dim(
            &self.cfg,
            "  switching does not stop anything: a turn keeps running in the session you \
             left, and it is still there when you come back.",
        ));
        out
    }

    /// **The setting card** — one renderer for every setting a reader chooses from (R38).
    ///
    /// `/mode`, `/models`, `/verbosity` and `/diff` are four questions of one kind, so they get
    /// one card in one place. That is not tidiness: this file has been burned twice by a second
    /// copy of a list that then drifted (the `mode` settings row it used to keep, and the
    /// completion table R32 found), and a third card would be a third thing to keep in step.
    ///
    /// # What R38 requires of it, and where each one is
    ///
    /// * **Every value, the current one marked.** The marker is `← now` in the faint register
    ///   and the value itself is bold, which is the split the session picker draws between its
    ///   bold row and its inverse one: *where am I* and *what does Enter take* stay two
    ///   readable facts even when they are different rows.
    /// * **What each value MEANS.** For the head's own two settings the sentence is on the row
    ///   (see [`Pick::values`]); a daemon's setting carries none, because the head does not
    ///   know what `automode-edits` means and a gloss it invented would be the other half's
    ///   documentation written wrongly.
    /// * **It takes effect on what is ALREADY DRAWN**, which for the ladder is the surprising
    ///   half and is why [`Pick::consequence`] says so under the list.
    /// * **`esc` is a real answer** — it closes the card and leaves the setting alone; the
    ///   silence is the answer, and the arm that handles it says so.
    ///
    /// The shape is the mode card's, deliberately: title, one row per value with its number on
    /// the left, `← now` on the right of the current one, then the keys, then the consequence.
    /// The click arithmetic in [`App::screen`] counts on this: the title is one row and the
    /// first value is the next one.
    pub(crate) fn setting_picker_lines(&self, w: usize) -> Vec<String> {
        let Some(subject) = self.pick else {
            return Vec::new();
        };
        let p = self.cfg.palette();
        let mut out = vec![colour(&self.cfg, sgr::BOLD, subject.title())];
        let values = self.pick_values();
        if values.is_empty() {
            out.push(dim(
                &self.cfg,
                match subject.row_key() {
                    // The daemon's two, in the words the config pane already uses for a list
                    // it was not sent.
                    Some("model") => {
                        "  this daemon has not named its models — `/models PROVIDER/MODEL` \
                         still works, if you know the name."
                    }
                    _ => {
                        "  this daemon has not named its modes — `/mode NAME` still works, if \
                         you know the name."
                    }
                },
            ));
        }
        let current = self.pick_current();
        for (i, (name, why)) in values.iter().enumerate() {
            let here = *name == current;
            let picked = i == self.mode_sel.min(values.len().saturating_sub(1));
            let mark = if picked { "▸" } else { " " };
            // **Greened when this box can actually take the row** — the operator's ask. Only the
            // model card: a mode has nothing to authenticate, and a green rung would be a claim
            // about a key on a row that has none.
            let ready = subject == Pick::Model && self.choice_ready(name);
            let role = if ready {
                Role::Success
            } else if here {
                Role::Strong
            } else {
                Role::Plain
            };
            let left = format!("{mark} {:>2}  {}", i + 1, p.paint(role, name),);
            let right = if here {
                p.paint(Role::Faint, "← now")
            } else {
                String::new()
            };
            let left = if picked {
                colour(&self.cfg, sgr::REVERSE, &left)
            } else {
                left
            };
            out.push(trim_to(&split_row(&left, &right, w), w));
            // **The meaning, wrapped and indented under its value.** One fact per line is the
            // rule the model card already records: these are not trimmed, so a sentence
            // carrying two facts would lose the second one — measured at 110 columns, where a
            // two-fact version read "It also become…" and its useful half never reached the
            // screen.
            if !why.is_empty() {
                for l in wrap(why, w.saturating_sub(9)) {
                    out.push(dim(&self.cfg, &format!("        {l}")));
                }
            }
        }
        out.push(dim(
            &self.cfg,
            "  ↑↓ moves · enter takes · or type a name or the row number · esc leaves it alone",
        ));
        // **What the colour means, in words** — because `Palette::None` is not a monochrome theme
        // but the `--replay`, pipe and CI case, and there a colour says nothing at all. The legend
        // carries exactly the claim the green does and the ask backs: taking a row without one
        // opens the key-ask card, so the sentence the operator's own row asked for is on the card
        // itself rather than only in the colour.
        if subject == Pick::Model {
            out.push(dim(
                &self.cfg,
                "  green: this box holds a key for it; the others ask for one when you take them",
            ));
        }
        for line in subject.consequence() {
            out.push(dim(&self.cfg, &format!("  {line}")));
        }
        out
    }
}
