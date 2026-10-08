//! **The help screen** (`/help`): this head's keys and verbs, drawn by
//! `rano::agent::help`. The rows are the head's — they are what it does — and the layout is
//! rano's.

use crate::ui::render::{RenderConfig, row_strings};
use rano::agent::help::HelpPane;

pub(crate) fn help_lines(cfg: &RenderConfig, w: usize) -> Vec<String> {
    let rows = [
        (
            "enter",
            "send what you typed; while a turn runs it is queued as a follow-up",
        ),
        (
            "alt+enter",
            "a newline inside the prompt, without sending it",
        ),
        (
            "esc esc",
            "interrupt the running turn — twice, within five seconds",
        ),
        (
            "ctrl-c",
            "clear what you typed; twice on an empty prompt, within a second, quits",
        ),
        (
            "↑ ↓",
            "move inside the prompt, then walk the prompts you have sent",
        ),
        (
            "pgup pgdn",
            "scroll the transcript; esc returns to following the stream",
        ),
        ("wheel", "scroll the transcript; shift+drag selects text"),
        (
            "ctrl-a ctrl-e",
            "start and end of the line; ctrl-w and ctrl-u kill, ctrl-y yanks",
        ),
        (
            "ctrl-z",
            "undo — a word at a time, and a kill is always its own step",
        ),
        (
            "paste",
            "five lines or more collapses to a marker and is sent in full",
        ),
        (
            "! COMMAND",
            "run it as YOUR shell command — no gate, and the line and its output go into the \
             conversation",
        ),
        (
            "!term COMMAND",
            "run a program that owns the screen IN THE PANE — `!term mc`, `!term nano notes.txt`, \
             `!term top`. The conversation's rectangle is given to the program and the composer \
             keeps its rows; your keys go to it verbatim, and `ctrl-\\` LEAVES it running (the \
             program never sees that key, so it cannot trap it) — `!term` comes back to the same \
             run, and `!term close` ends it, after asking",
        ),
        (
            "!send LINE",
            "send a line to a command of yours that is still running — `!send Y` answers a \
             `Continue? [Y/n]`, and a bare `!send` is an Enter, which is a real answer. This is \
             the way in when no card is up: a card appears by itself when the daemon can see \
             the command blocked on its stdin, and this verb needs no such signal",
        ),
        (
            "ctrl-s",
            "the session list: type a number or part of a name to switch",
        ),
        (
            "tab",
            "complete the /command being typed; more tabs walk the matches",
        ),
        (
            "click",
            "in the session list, picks the row under the pointer; enter still switches",
        ),
        (
            "ctrl-t",
            "the todos pane: the model's plan, and the repo's TODO.md read-only — ↑↓ moves (or click a row), enter acts on the row under the cursor, pgup/pgdn scrolls",
        ),
        (
            "/new [title]",
            "start a session in this daemon and go there",
        ),
        (
            "/switch WHAT",
            "go to a session by number, id or part of its name",
        ),
        ("ctrl-r", "fold or unfold the model's thinking"),
        (
            "ctrl-v",
            "open the rest of the newest long tool result — ↓ pages it, esc closes. \
             It is one row, not a switch: `/t` unfolds every tool row at once",
        ),
        (
            "ctrl-]",
            "open the newest file this conversation changed in rano, on the change — a click \
             on an edit or write row opens that one. In the editor, ctrl-] comes back to the \
             composer (and goes again), alt-s puts your place — and the selection, fenced — \
             in the composer for you to finish and send, alt-p shows the change again, and \
             ctrl-q or ^X closes it, asking first about unsaved edits",
        ),
        (
            "ctrl-p",
            "hold the view: while it is held the head writes nothing, so a text selection \
             survives a streaming turn. Press again to release — it says how many rows arrived",
        ),
        (
            "/notes",
            "the disclosures this head has shown; /notes dismiss [N|all] retires one \
             or every one, /notes restore brings them back",
        ),
        (
            "ctrl-x",
            "show the raw <function=…> text of tool calls, as the model wrote it",
        ),
        ("ctrl-l", "repaint the screen"),
        (
            "ctrl-n",
            "retire every note this head is showing — hidden, still counted on /status, \
             `/notes` prints them and `/notes restore` brings them back. Retired is not \
             deleted: a head that can silently drop a warning is a head whose warnings \
             cannot be trusted to be complete",
        ),
        (
            "/status",
            "this head's counters — dropped, scrubbed, resync — and what each means",
        ),
        (
            "/verbosity",
            "how much reaches the transcript: bare, a card stating every rung — conversation, \
             terse, normal, loud — and what each gives you; `/verbosity NAME` sets one. \
             /status counts what has been filtered",
        ),
        (
            "/diff",
            "how an edit card is drawn: bare, a card stating both; `/diff unified` or \
             `/diff split` sets one",
        ),
        ("/interrupt", "interrupt, when a key is awkward"),
        (
            "/config",
            "every setting and where it came from; the first row toggles the diff \
             view between split and unified, which `/diff NAME` also sets",
        ),
        (
            "/compact",
            "summarize this session down to one record; the old transcript is forked, not lost",
        ),
        (
            "/mode",
            "move this project to a point: read-only, always-ask, writes allowed, automode, automode-edits, allow-all (next session)",
        ),
        (
            "/supervise",
            "the guard model answers every gated call before you do, from the next call — on, off, status",
        ),
        (
            "/gate",
            "what the gate decided, and rule on it afterwards: recent, todo, corpus, ok|grant|revoke ID",
        ),
        (
            "/flowy",
            "the seat on the fabric: /flowy status · /flowy login [SEAT] [--token T] · /flowy logout",
        ),
        (
            "/models",
            "which model answers: /models lists them with their auth; /models deepseek/deepseek-chat switches and sticks; /models local",
        ),
        (
            "/job",
            "read a background job's output: /job lists them, /job ID prints it, --offset N resumes",
        ),
        (
            "/tools",
            "the tools seated here — and any the prompt has never been told about, which the model cannot call",
        ),
        (
            "/default-model",
            "what a NEW session starts on: /default-model PROVIDER/MODEL, or `local` to clear it. Not this conversation — that is /models",
        ),
        (
            "/resync",
            "throw this head's state away and take a fresh snapshot",
        ),
        (
            "/quit",
            "detach. The turn keeps running: idle means quiet, not unwatched",
        ),
    ];
    let pane = HelpPane {
        title: "keys and commands".into(),
        rows: rows
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        footer: "/help or esc closes this".into(),
    };
    row_strings(&pane.lines(w), cfg.palette())
}
