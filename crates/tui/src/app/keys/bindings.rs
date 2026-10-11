//! **Each surface's keys, declared once** (R69): which keys it takes, under what guard, what each
//! one does, and the words the hint bar and `/help` say about it — both GENERATED from here.
//!
//! # What this replaces
//!
//! Twelve `key_*` handlers, each a `match` on the key with its own guards, and three sets of
//! words about them written by hand somewhere else: rano's pinned hint-bar sentences, this
//! head's own half of the bar, and the help screen. They disagreed. The queue pane's `a`/`v`/`d`
//! had no empty-composer guard where the subagents pane's `o`/`p` did, so typing "deploy" with
//! `/queue` open sent `queue rm` and `queue approve` (R67 item 1); the help said a click on an
//! edit row opened rano when it opens the diff popup; rano's todos sentence said *enter or tab
//! unfolds* about a pane where Enter reads the row; and nothing at all said what the subagent
//! output view's Enter did (R67 item 9).
//!
//! # The rule
//!
//! A [`Binding`] is `(keys, guard, act, says)`. [`App::key_surface`] finds the first binding of
//! the focused surface that the key matches and whose guard holds, and runs its act; the bar
//! ([`App::surface_hint`]) and the help rows ([`App::surface_help`]) are the same table read as
//! words. **A plain letter always carries [`Guard::EmptyComposer`]** — the letters are the
//! operator's to type first — and a test walks every table to hold that.

use super::*;
use crate::ui::panes::queue::Verb;

/// **Which keys a binding answers to.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum On {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Tab,
    Char(char),
    /// PageUp and PageDown.
    Page,
    /// A wheel notch with no position — what the chords hand a surface.
    Wheel,
    /// Every key but Esc and ctrl-c, which are the stack's: a surface with no field on it that
    /// lets nothing reach the composer behind it.
    Any,
}

impl On {
    pub(crate) fn matches(self, k: &Key) -> bool {
        match (self, k) {
            (On::Up, Key::Up)
            | (On::Down, Key::Down)
            | (On::Left, Key::Left)
            | (On::Right, Key::Right)
            | (On::Enter, Key::Enter)
            | (On::Tab, Key::Tab)
            | (On::Page, Key::PageUp | Key::PageDown)
            | (On::Wheel, Key::WheelUp | Key::WheelDown) => true,
            (On::Char(c), Key::Char(d)) => c == *d,
            (On::Any, Key::Esc | Key::CtrlC) => false,
            (On::Any, _) => true,
            _ => false,
        }
    }

    /// The key as the bar and the help spell it.
    fn label(self) -> String {
        match self {
            On::Up => "↑".into(),
            On::Down => "↓".into(),
            On::Left => "←".into(),
            On::Right => "→".into(),
            On::Enter => "enter".into(),
            On::Tab => "tab".into(),
            On::Char(c) => c.to_string(),
            On::Page => "pgup/pgdn".into(),
            On::Wheel => "the wheel".into(),
            On::Any => String::new(),
        }
    }
}

/// **When a binding holds.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guard {
    Always,
    /// Only while the composer is empty: with a line typed the key is the composer's.
    EmptyComposer,
}

/// **What a binding does.** The surface decides what each means for it (`App::surface_act`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Act {
    /// ↑↓ move the cursor through the list's stops.
    Cursor,
    /// Move the surface's window — a line for an arrow, a page for a page key, a notch.
    Scroll,
    /// The row's act: switch, open, read, change.
    Open,
    /// The todos pane's second act on the row (Tab).
    Alt,
    /// Read a subagent's output without leaving.
    Peek,
    /// Read the open output again.
    Reread,
    /// The next page of a job's output (→, enter) or back the way it came (←).
    PageOut,
    /// Open a conversation's subagents in the picker (→), or close them (←).
    Expand,
    Collapse,
    /// The queue's other view.
    View,
    /// One of the person's queue verbs on the entry under the cursor.
    Verb(Verb),
    /// Take the key and do nothing — it is not the composer's while this surface is up.
    Swallow,
}

/// **What the bar and the help say about a binding.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Says {
    /// Nothing: a key that is there to be swallowed, or that another binding already names.
    Nothing,
    Text(&'static str),
    /// It depends on the row under the cursor; the surface says ([`App::row_says`]).
    Row,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) on: &'static [On],
    pub(crate) guard: Guard,
    pub(crate) act: Act,
    pub(crate) says: Says,
}

const fn b(on: &'static [On], guard: Guard, act: Act, says: Says) -> Binding {
    Binding {
        on,
        guard,
        act,
        says,
    }
}

use Guard::{Always, EmptyComposer as Empty};
use Says::{Nothing, Row, Text};

const UPDOWN: &[On] = &[On::Up, On::Down];
const PAGE: Binding = b(&[On::Page], Always, Act::Scroll, Text("scroll"));
const WHEEL: Binding = b(&[On::Wheel], Always, Act::Scroll, Nothing);
const LIST: Binding = b(UPDOWN, Always, Act::Cursor, Text("moves"));

const READING: &[Binding] = &[PAGE, WHEEL];
const PICKER: &[Binding] = &[
    LIST,
    b(
        &[On::Right],
        Empty,
        Act::Expand,
        Text("opens its subagents"),
    ),
    b(&[On::Left], Empty, Act::Collapse, Text("closes them")),
    b(&[On::Enter], Empty, Act::Open, Text("switches")),
    // Short, and its cursor keeps itself in view: a page key moves nothing under it.
    b(&[On::Page, On::Wheel], Always, Act::Swallow, Nothing),
];
const CONFIG: &[Binding] = &[
    LIST,
    b(
        &[On::Enter],
        Always,
        Act::Open,
        Text("changes a row marked ✎"),
    ),
    PAGE,
    WHEEL,
];
const TODOS: &[Binding] = &[
    LIST,
    b(&[On::Enter], Empty, Act::Open, Row),
    b(&[On::Tab], Empty, Act::Alt, Row),
    PAGE,
    WHEEL,
];
const TODO_ROW: &[Binding] = &[
    b(UPDOWN, Always, Act::Scroll, Text("scroll")),
    PAGE,
    WHEEL,
    // A reading surface with no field on it: a draft must not grow in the composer under it.
    b(&[On::Any], Always, Act::Swallow, Nothing),
];
const QUEUE: &[Binding] = &[
    LIST,
    b(
        &[On::Enter],
        Always,
        Act::Open,
        Text("opens the row: the ask, the verdict, the gate's words, every move"),
    ),
    b(&[On::Tab], Always, Act::View, Text("switches the view")),
    b(
        &[On::Char('a')],
        Empty,
        Act::Verb(Verb::Approve),
        Text("approve"),
    ),
    b(&[On::Char('v')], Empty, Act::Verb(Verb::Veto), Text("veto")),
    b(&[On::Char('d')], Empty, Act::Verb(Verb::Rm), Text("drop")),
    b(
        &[On::Char('r')],
        Empty,
        Act::Verb(Verb::Restart),
        Text("restart"),
    ),
    PAGE,
    WHEEL,
];
const QUEUE_ENTRY: &[Binding] = &[b(UPDOWN, Always, Act::Scroll, Text("scroll")), PAGE, WHEEL];
const STANDING: &[Binding] = &[
    LIST,
    b(
        &[On::Enter],
        Always,
        Act::Open,
        Text("opens the note in the editor"),
    ),
    PAGE,
    WHEEL,
];
const SUBAGENTS: &[Binding] = &[
    LIST,
    b(&[On::Enter], Always, Act::Open, Row),
    b(&[On::Char('o')], Empty, Act::Open, Nothing),
    b(&[On::Char('p')], Empty, Act::Peek, Text("reads its output")),
    PAGE,
    WHEEL,
];
const SUB_OUT: &[Binding] = &[
    b(UPDOWN, Always, Act::Scroll, Text("scroll")),
    b(&[On::Enter], Always, Act::Reread, Text("reads it again")),
    PAGE,
    WHEEL,
];
const JOBS: &[Binding] = &[LIST, b(&[On::Enter], Always, Act::Open, Row), PAGE, WHEEL];
const JOB_OUT: &[Binding] = &[
    b(UPDOWN, Always, Act::Scroll, Text("scroll")),
    b(&[On::Right], Empty, Act::PageOut, Text("next page")),
    b(&[On::Left], Empty, Act::PageOut, Text("back")),
    b(&[On::Enter], Always, Act::PageOut, Text("reads on")),
    PAGE,
    WHEEL,
];
const LISTING: &[Binding] = &[b(UPDOWN, Always, Act::Scroll, Text("scroll")), PAGE, WHEEL];

impl Surface {
    /// **The surface's keys** — the one table its dispatch, its bar and its help read.
    pub(crate) fn bindings(self) -> &'static [Binding] {
        match self {
            Surface::Help | Surface::Stats => READING,
            Surface::Picker => PICKER,
            Surface::Config => CONFIG,
            Surface::Todos => TODOS,
            Surface::TodoRow => TODO_ROW,
            Surface::Queue => QUEUE,
            Surface::QueueEntry => QUEUE_ENTRY,
            Surface::Standing => STANDING,
            Surface::Subagents => SUBAGENTS,
            Surface::SubOut => SUB_OUT,
            Surface::Jobs => JOBS,
            Surface::JobOut => JOB_OUT,
            Surface::Listing => LISTING,
        }
    }

    /// **What Esc does here**, said last on the bar: a pane closes; an overlay goes back to the
    /// list it was opened from, because that list is the next entry on the stack.
    pub(crate) fn esc_says(self) -> &'static str {
        match self {
            Surface::TodoRow => "esc back to the list",
            Surface::QueueEntry => "esc back to the queue",
            Surface::SubOut => "esc back to the subagents",
            Surface::JobOut => "esc back to the jobs",
            _ => "esc closes",
        }
    }

    /// **What a surface takes that is not a key**: the typed path a list advertises.
    pub(crate) fn typed_says(self) -> Option<&'static str> {
        match self {
            Surface::Picker => Some("type a number or a name to switch · /new [title]"),
            Surface::Todos => Some("/todo rm N removes one of yours"),
            _ => None,
        }
    }

    /// **The chord that opens it**, read by `key_chords` to dispatch and by the bar and the help
    /// to say — one entry, so a chord cannot be advertised and do nothing.
    pub(crate) fn chord(self) -> Option<(&'static str, Key)> {
        match self {
            Surface::Picker => Some(("ctrl-s", Key::CtrlS)),
            Surface::Todos => Some(("ctrl-t", Key::CtrlT)),
            Surface::Subagents => Some(("ctrl-g", Key::CtrlG)),
            Surface::Jobs => Some(("ctrl-q", Key::CtrlQ)),
            _ => None,
        }
    }

    /// The surface a key is the chord of.
    pub(crate) fn of_chord(k: &Key) -> Option<Surface> {
        Surface::ALL
            .into_iter()
            .find(|s| s.chord().is_some_and(|(_, c)| &c == k))
    }

    /// **The verb that opens it**, for the help.
    pub(crate) fn verb(self) -> Option<&'static str> {
        match self {
            Surface::Help => Some("/help"),
            Surface::Stats => Some("/status"),
            Surface::Picker => Some("/sessions"),
            Surface::Config => Some("/config"),
            Surface::Todos => Some("/todos"),
            Surface::Queue => Some("/queue"),
            Surface::Standing => Some("/standing"),
            Surface::Subagents => Some("/subagents"),
            Surface::Jobs => Some("/jobs"),
            Surface::SubOut => Some("/peek ID"),
            _ => None,
        }
    }

    /// **What it is**, in the help's words.
    pub(crate) fn about(self) -> &'static str {
        match self {
            Surface::Help => "this list",
            Surface::Stats => {
                "this head's counters — dropped, scrubbed, resync — and what each means"
            }
            Surface::Picker => "the session list",
            Surface::Config => {
                "every setting and where it came from; the first row toggles the diff view \
                 between split and unified"
            }
            Surface::Todos => {
                "the todos pane: the model's plan, your own rows, and the repo's TODO.md read-only"
            }
            Surface::TodoRow => "one todo row, read whole",
            Surface::Queue => {
                "the merge queue — every entry being served toward main, in a review and a \
                 merge view"
            }
            Surface::QueueEntry => "one queue entry, whole",
            Surface::Standing => {
                "the notes the harness reads into the prompt, one row each, with the form the \
                 budget gave it (`verbatim`, or `indexed` when only its headings fit). NOT \
                 /notes, which is what this head has shown"
            }
            Surface::Subagents => {
                "the subagent tree; enter goes into that subagent's session, and esc inside a \
                 subagent goes back up"
            }
            Surface::SubOut => "one subagent's output, read without leaving",
            Surface::Jobs => "the background jobs this session started",
            Surface::JobOut => "one job's output",
            Surface::Listing => "a slash command's listing",
        }
    }
}

impl App {
    /// **The binding a key reaches on surface `s`**, guard and all — the dispatch's question.
    pub(crate) fn binding_for(&self, s: Surface, k: &Key) -> Option<Binding> {
        let empty = self.editor.text().is_empty();
        s.bindings()
            .iter()
            .copied()
            .find(|b| b.on.iter().any(|o| o.matches(k)) && (b.guard == Guard::Always || empty))
    }

    /// **The words for a row-dependent binding**, from the row under the cursor.
    fn row_says(&self, s: Surface, b: &Binding) -> Option<String> {
        match (s, b.act) {
            (Surface::Todos, act) => {
                let stops = self.todos_stops();
                let at = self.todos_sel.min(stops.len().saturating_sub(1));
                Some(
                    match (stops.get(at), act) {
                        (Some(TodoStop::Add), _) => "opens the card",
                        (_, Act::Open) => "reads it whole",
                        (Some(TodoStop::Mine(_)), _) => "toggles it done",
                        (Some(TodoStop::Repo(_)), _) => "unfolds the item",
                        _ => return None,
                    }
                    .to_string(),
                )
            }
            (Surface::Subagents, _) => Some(
                match self.subagent_stops().get(self.subagents_sel) {
                    Some(SubStop::Finished) => "unfolds finished",
                    _ => "(or o) goes into it",
                }
                .to_string(),
            ),
            (Surface::Jobs, _) => Some(
                match self.job_stops().get(self.jobs_sel) {
                    Some(JobStop::Finished) => "unfolds finished",
                    _ => "reads the job's output",
                }
                .to_string(),
            ),
            _ => None,
        }
    }

    /// **The bar's words for surface `s`, generated from its table**: each binding that says
    /// something, two bindings that say the same thing joined (`enter or tab opens the card`),
    /// the typed path the surface advertises, and what Esc does.
    pub(crate) fn surface_hint(&self, s: Surface) -> String {
        let mut parts: Vec<(String, String)> = Vec::new();
        for b in s.bindings() {
            let says = match b.says {
                Says::Nothing => continue,
                Says::Text(t) => t.to_string(),
                Says::Row => match self.row_says(s, b) {
                    Some(t) => t,
                    None => continue,
                },
            };
            let label = keys_label(b.on);
            // Navigation keys that do the same thing read as one run (`↑↓ pgup/pgdn scroll`);
            // two keys for one act read as a choice (`enter or tab opens the card`).
            let moving = b.on.iter().all(|o| {
                matches!(
                    o,
                    On::Up | On::Down | On::Left | On::Right | On::Page | On::Wheel
                )
            });
            match parts.last_mut() {
                Some((l, last)) if *last == says && moving => *l = format!("{l} {label}"),
                Some((l, last)) if *last == says => *l = format!("{l} or {label}"),
                _ => parts.push((label, says)),
            }
        }
        let mut out: Vec<String> = parts.into_iter().map(|(l, s)| format!("{l} {s}")).collect();
        out.extend(s.typed_says().map(str::to_string));
        out.push(s.esc_says().to_string());
        out.join(" · ")
    }

    /// **The help's row for surface `s`**: how it is opened, and what it is and its keys —
    /// generated, so the help cannot name a key the surface does not take. `None` for a surface
    /// nothing opens by name (an overlay its list opens).
    pub(crate) fn surface_help(s: Surface) -> Option<(String, String)> {
        let opened = match (s.chord(), s.verb()) {
            (Some((c, _)), Some(v)) => format!("{c}  {v}"),
            (None, Some(v)) => v.to_string(),
            (Some((c, _)), None) => c.to_string(),
            (None, None) => return None,
        };
        let keys: Vec<String> = s
            .bindings()
            .iter()
            .filter_map(|b| match b.says {
                Says::Text(t) => Some(format!("{} {t}", keys_label(b.on))),
                Says::Row => Some(format!("{} acts on the row", keys_label(b.on))),
                Says::Nothing => None,
            })
            .collect();
        let guarded = s
            .bindings()
            .iter()
            .any(|b| b.on.iter().any(|o| matches!(o, On::Char(_))));
        let mut text = s.about().to_string();
        if !keys.is_empty() {
            text.push_str(": ");
            text.push_str(&keys.join(", "));
        }
        if guarded {
            text.push_str(" (letters act only on an empty prompt)");
        }
        Some((opened, text))
    }
}

/// The keys of a binding as one label: `↑↓`, `enter`, `pgup/pgdn`.
fn keys_label(on: &[On]) -> String {
    let labels: Vec<String> = on.iter().map(|o| o.label()).collect();
    if on
        .iter()
        .all(|o| matches!(o, On::Up | On::Down | On::Left | On::Right))
    {
        labels.concat()
    } else {
        labels.join(" or ")
    }
}
