//! `screen`: run a program on a pty inside the seat and look at it, as text with a style map or as
//! a picture, so a UI change can be seen. The pty is `exec::term`'s, the grid is `letibot_vt`'s,
//! and the picture is `render`'s. Screens belong to the tool, so they end with the session.

mod keys;
mod render;
mod tab;
mod web;

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use letibot_vt::Screen;
use serde_json::Value;

use crate::exec::term::{self, TermConfig, TermSession, TermSink};
use crate::exec::{ScopeId, ScopeKind, ScopeTree};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub use render::{CELL_H, CELL_W};
pub use tab::browser_socket;

const DEFAULT_COLS: usize = 100;
const DEFAULT_ROWS: usize = 30;
const MAX_COLS: usize = 250;
const MAX_ROWS: usize = 80;
const IDLE_MS: u64 = 300;
const MAX_WAIT_MS: u64 = 3_000;
const WAIT_CEILING_MS: u64 = 15_000;
const STYLE_LINES: usize = 120;

/// How many screens at once: a program per two cores, never more than four, never none.
pub fn screen_cap() -> usize {
    let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
    (cpus / 2).clamp(1, 4)
}

/// What the program drew, fed by the pty's reader thread.
struct Feed {
    vt: Mutex<Screen>,
    last: Mutex<Instant>,
    ended: Mutex<Option<String>>,
    cv: Condvar,
}

impl TermSink for Feed {
    fn output(&self, bytes: &[u8]) {
        if let Ok(mut vt) = self.vt.lock() {
            vt.feed(bytes);
        }
        if let Ok(mut last) = self.last.lock() {
            *last = Instant::now();
        }
        self.cv.notify_all();
    }

    fn ended(&self, reason: &str) {
        if let Ok(mut e) = self.ended.lock() {
            *e = Some(reason.to_string());
        }
        self.cv.notify_all();
    }
}

impl Feed {
    fn ended(&self) -> Option<String> {
        self.ended.lock().ok().and_then(|e| e.clone())
    }

    /// Wait until the program has been quiet for `idle`, it ends, or `max` passes. `true` means
    /// the screen settled.
    fn settle(&self, idle: Duration, max: Duration) -> bool {
        let start = Instant::now();
        let mut guard = self.ended.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if guard.is_some() {
                return true;
            }
            let quiet = self.last.lock().map(|l| l.elapsed()).unwrap_or(idle);
            if quiet >= idle {
                return true;
            }
            let left = max.saturating_sub(start.elapsed());
            if left.is_zero() {
                return false;
            }
            let nap = (idle - quiet).min(left);
            guard = self
                .cv
                .wait_timeout(guard, nap)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

struct Live {
    session: TermSession,
    feed: Arc<Feed>,
    command: String,
}

#[derive(Default)]
struct State {
    next: u32,
    screens: BTreeMap<u32, Live>,
}

#[derive(Default)]
pub struct ScreenTool {
    state: Mutex<State>,
    tree: OnceLock<Option<Arc<dyn ScopeTree>>>,
    /// Who this seat is to `letibot browser`: a tab is allowed per session.
    session: Option<String>,
    browser: Option<std::path::PathBuf>,
}

impl ScreenTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session(mut self, id: impl Into<String>) -> Self {
        self.session = Some(id.into());
        self
    }

    /// The bridge's socket, when it is not [`browser_socket`]'s answer (tests).
    pub fn with_browser_socket(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.browser = Some(path.into());
        self
    }

    fn tree(&self) -> Option<Arc<dyn ScopeTree>> {
        self.tree
            .get_or_init(|| crate::exec::host_tree().ok().map(Arc::from))
            .clone()
    }
}

fn num(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

impl Tool for ScreenTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "screen",
            "Run a program on a terminal screen in this seat, then look at it. `start` \
             {cmd, cols, rows, cwd}: default cmd is the letibot head on a throwaway session; \
             returns an id. `keys` {id, keys}: text is literal, `<Enter> <Esc> <Tab> <Up> \
             <C-c> <M-x>` are keys, `<<` is `<`. `mouse` {id, x, y, button, move, scroll}: \
             cells from 0. `read` {id, idle_ms}: rows plus a style map. `shot` {id}: a PNG. \
             `web` {url, width, height, full_page, wait_ms}: a page in headless Chrome as a \
             PNG; `dom` {url}: its text. `list`, `stop` {id}. `tab` {act}: the operator's \
             Chrome tab, once allowed for this session: grants, shot, read (text and numbered \
             elements), click {n|selector|x,y}, type {text, n|selector}, key {keys}, navigate \
             {url}, scroll {dy}.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "start, keys, mouse, read, shot, web, dom, list, stop or tab."},
                    "act": {"type": "string", "description": "tab: grants, shot, read, click, type, key, navigate or scroll."},
                    "n": {"type": "integer", "description": "tab: an element number from read."},
                    "selector": {"type": "string", "description": "tab: a CSS selector."},
                    "text": {"type": "string", "description": "tab type: inserted at the focus, replacing the field."},
                    "dy": {"type": "integer", "description": "tab scroll: CSS pixels, negative is up."},
                    "id": {"type": "integer", "description": "The screen, from start."},
                    "cmd": {"type": "string", "description": "start: a shell line."},
                    "cols": {"type": "integer"},
                    "rows": {"type": "integer"},
                    "cwd": {"type": "string"},
                    "keys": {"type": "string", "description": "tab key: Enter, Tab, Escape, Backspace, arrows, Shift+Tab, Ctrl+a."},
                    "x": {"type": "integer", "description": "mouse: a cell; tab click: CSS pixels as in shot."},
                    "y": {"type": "integer"},
                    "button": {"type": "string", "description": "left, middle or right; the program must enable mouse."},
                    "move": {"type": "boolean"},
                    "scroll": {"type": "string"},
                    "url": {"type": "string", "description": "web, dom: http(s) or file URL; tab navigate: http(s)."},
                    "width": {"type": "integer"},
                    "height": {"type": "integer"},
                    "full_page": {"type": "boolean"},
                    "wait_ms": {"type": "integer"},
                    "idle_ms": {"type": "integer", "description": "read, shot: quiet time that counts as settled (default 300)."}
                },
                "required": ["action"]
            }),
            Access::Exec,
        )
    }

    fn access_for(&self, args: &Value) -> Option<Access> {
        match text(args, "action") {
            Some("read" | "shot" | "list") => Some(Access::Read),
            Some("web" | "dom") => Some(Access::Network),
            // Looking at the tab reads; anything that acts on it acts on the world.
            Some("tab") => match text(args, "act") {
                Some("grants" | "shot" | "read") => Some(Access::Read),
                _ => Some(Access::Network),
            },
            _ => None,
        }
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        match text(args, "action").unwrap_or("") {
            "start" => self.start(ctx, args),
            "keys" => self.with(args, |id, live| {
                let app = live
                    .feed
                    .vt
                    .lock()
                    .map(|v| v.app_cursor_keys())
                    .unwrap_or(false);
                let bytes = match keys::encode(text(args, "keys").unwrap_or(""), app) {
                    Ok(b) => b,
                    Err(e) => return Invocation::failed("bad keys", e),
                };
                send(id, live, &bytes)
            }),
            "mouse" => self.with(args, |id, live| {
                let pointer = if args.get("move").and_then(|v| v.as_bool()) == Some(true) {
                    keys::Pointer::Move
                } else if let Some(s) = text(args, "scroll") {
                    keys::Pointer::Wheel { up: s == "up" }
                } else {
                    match text(args, "button").unwrap_or("left") {
                        "left" => keys::Pointer::Click(0),
                        "middle" => keys::Pointer::Click(1),
                        "right" => keys::Pointer::Click(2),
                        other => {
                            return Invocation::failed(
                                "bad button",
                                format!("`{other}`: use left, middle or right"),
                            );
                        }
                    }
                };
                let (Some(x), Some(y)) = (num(args, "x"), num(args, "y")) else {
                    return Invocation::failed("mouse needs x and y", "cells, counted from 0");
                };
                let mode = live.feed.vt.lock().map(|v| v.mouse()).unwrap_or_default();
                match keys::mouse(mode, pointer, x as usize, y as usize) {
                    Ok(bytes) => send(id, live, &bytes),
                    Err(e) => Invocation::failed("no mouse report sent", e),
                }
            }),
            "read" => self.with(args, |id, live| {
                let settled = settle(live, args);
                Invocation::ok(view(id, live, settled))
            }),
            "shot" => self.with(args, |id, live| {
                let settled = settle(live, args);
                let png = match live.feed.vt.lock() {
                    Ok(vt) => render::png(&vt),
                    Err(_) => return Invocation::failed("screen unavailable", "try read"),
                };
                let Some(media) = crate::media::Media::of("screen.png", &png) else {
                    return Invocation::failed("no picture", "the PNG did not form; use `read`");
                };
                let note = if settled { "" } else { " (still changing)" };
                let mut inv = Invocation::ok(format!(
                    "screen {id}: {}{note}\nThe picture is attached to this result. If it did not \
                     arrive, this model cannot see pictures: `read` gives the same screen as text.",
                    media.summary()
                ));
                inv.media = Some(media);
                inv
            }),
            "web" => web::web(ctx, args),
            "dom" => web::dom(ctx, args),
            "tab" => {
                let socket = self.browser.clone().unwrap_or_else(browser_socket);
                tab::tab(ctx, self.session.as_deref(), &socket, args)
            }
            "list" => {
                let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if st.screens.is_empty() {
                    return Invocation::ok("no screens");
                }
                let mut out = String::new();
                for (id, l) in &st.screens {
                    let (r, c) = l.feed.vt.lock().map(|v| v.size()).unwrap_or((0, 0));
                    let state = l.feed.ended().unwrap_or_else(|| "running".into());
                    out.push_str(&format!("{id}: {c}x{r} {state}: {}\n", l.command));
                }
                Invocation::ok(out)
            }
            "stop" => {
                let Some(id) = num(args, "id").map(|i| i as u32) else {
                    return Invocation::failed("stop needs `id`", "from `start` or `list`");
                };
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match st.screens.remove(&id) {
                    Some(mut live) => {
                        live.session.close("screen stopped");
                        Invocation::ok(format!("screen {id} stopped"))
                    }
                    None => Invocation::failed("no such screen", format!("{id} is not open")),
                }
            }
            other => Invocation::failed(
                format!("unknown action `{other}`"),
                "start, keys, mouse, read, shot, web, dom, list, stop or tab",
            ),
        }
    }
}

fn settle(live: &Live, args: &Value) -> bool {
    let idle = num(args, "idle_ms").unwrap_or(IDLE_MS).min(WAIT_CEILING_MS);
    let max = num(args, "max_ms")
        .unwrap_or(MAX_WAIT_MS)
        .clamp(idle, WAIT_CEILING_MS);
    live.feed
        .settle(Duration::from_millis(idle), Duration::from_millis(max))
}

fn send(id: u32, live: &Live, bytes: &[u8]) -> Invocation {
    if let Some(why) = live.feed.ended() {
        return Invocation::failed("the program is over", format!("screen {id}: {why}"));
    }
    match live.session.input(bytes) {
        Ok(()) => Invocation::ok(format!("sent {} bytes to screen {id}", bytes.len())),
        Err(e) => Invocation::failed("keys not sent", e.to_string()),
    }
}

impl ScreenTool {
    fn with(&self, args: &Value, f: impl FnOnce(u32, &Live) -> Invocation) -> Invocation {
        let Some(id) = num(args, "id").map(|i| i as u32) else {
            return Invocation::failed("this action needs `id`", "from `start` or `list`");
        };
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match st.screens.get(&id) {
            Some(live) => f(id, live),
            None => Invocation::failed("no such screen", format!("{id} is not open; see `list`")),
        }
    }

    fn start(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let cols = (num(args, "cols").unwrap_or(DEFAULT_COLS as u64) as usize).clamp(10, MAX_COLS);
        let rows = (num(args, "rows").unwrap_or(DEFAULT_ROWS as u64) as usize).clamp(3, MAX_ROWS);
        let Some(host) = ctx.backend.processes() else {
            return Invocation::failed(
                "no screens on this seat",
                "this seat has no local process host; run the program with `bash` instead",
            );
        };
        let cwd = text(args, "cwd")
            .map(std::path::PathBuf::from)
            .or_else(|| ctx.backend.workspace_path().map(Into::into))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let mut env = term::env();
        if let Ok(socket) = std::env::var("LETIBOT_SOCKET") {
            env.push(("LETIBOT_SOCKET".into(), socket));
        }
        let confine = match host.terminal_argv(&cwd, &term::confined_env(&env)) {
            None => {
                return Invocation::failed(
                    "no screens on this seat",
                    "this host cannot run a pty program; use `bash`",
                );
            }
            Some(Err(why)) => {
                return Invocation::failed("the boundary refused the program", why);
            }
            Some(Ok(argv)) => argv,
        };

        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.screens.retain(|_, l| l.session.live());
        let cap = screen_cap();
        if st.screens.len() >= cap {
            let open: Vec<String> = st.screens.keys().map(|i| i.to_string()).collect();
            return Invocation::failed(
                format!("{cap} screens are open"),
                format!("stop one first: {}", open.join(", ")),
            );
        }
        st.next += 1;
        let id = st.next;
        let command = match text(args, "cmd").map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => c.to_string(),
            None => format!("${{LETIBOT_HEAD:-letibot-tui}} --new-session screen-{id}"),
        };
        let tree = self.tree();
        let scope: Option<ScopeId> = tree.as_ref().and_then(|t| {
            t.open(ScopeKind::Session, &format!("screen-{id}"), None)
                .ok()
        });
        let cfg = TermConfig {
            command: command.clone(),
            env,
            cwd,
            cols,
            rows,
            scope,
            tree,
            confine,
        };
        let feed = Arc::new(Feed {
            vt: Mutex::new(Screen::new(rows, cols)),
            last: Mutex::new(Instant::now()),
            ended: Mutex::new(None),
            cv: Condvar::new(),
        });
        match TermSession::start(&cfg, feed.clone()) {
            Ok(session) => {
                st.screens.insert(
                    id,
                    Live {
                        session,
                        feed,
                        command: command.clone(),
                    },
                );
                Invocation::ok(format!(
                    "screen {id} started ({cols}x{rows}): {command}\nUse `read` to see it."
                ))
            }
            Err(e) => Invocation::failed("the program did not start", e.to_string()),
        }
    }
}

/// The screen as rows and a style map.
fn view(id: u32, live: &Live, settled: bool) -> String {
    let Ok(vt) = live.feed.vt.lock() else {
        return "screen unavailable".into();
    };
    let (rows, cols) = vt.size();
    let (cr, cc) = vt.cursor();
    let state = match live.feed.ended() {
        Some(why) => format!("ended: {why}"),
        None if settled => "running".to_string(),
        None => "running, output still arriving".to_string(),
    };
    let mut out = format!(
        "screen {id} {cols}x{rows}, cursor row {cr} col {cc}{}, {state}{}\n",
        if vt.cursor_visible() { "" } else { " (hidden)" },
        if vt.alternate() {
            ", alternate screen"
        } else {
            ""
        },
    );
    let lines: Vec<String> = (0..rows)
        .map(|r| vt.line(r).trim_end().to_string())
        .collect();
    let last = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    for (r, l) in lines.iter().take(last).enumerate() {
        out.push_str(&format!("{r:>2}| {l}\n"));
    }
    if last < rows {
        out.push_str(&format!("   ({} blank rows below)\n", rows - last));
    }
    out.push_str("styles:\n");
    let mut n = 0;
    for (r, row) in vt.rows().enumerate() {
        let mut c = 0;
        while c < row.len() {
            let a = row[c].attr;
            let mut end = c;
            while end + 1 < row.len() && row[end + 1].attr == a {
                end += 1;
            }
            if !a.is_default() {
                if n == STYLE_LINES {
                    out.push_str("  ... more runs not listed\n");
                    return out;
                }
                n += 1;
                let mut tags: Vec<String> = Vec::new();
                for (on, name) in [(a.bold, "bold"), (a.dim, "faint"), (a.reverse, "inverse")] {
                    if on {
                        tags.push(name.into());
                    }
                }
                if let Some(f) = a.fg {
                    tags.push(format!("fg={}", render::SLOT_NAMES[(f % 16) as usize]));
                }
                if let Some(b) = a.bg {
                    tags.push(format!("bg={}", render::SLOT_NAMES[(b % 16) as usize]));
                }
                out.push_str(&format!("  r{r} c{c}-{end} {}\n", tags.join(" ")));
            }
            c = end + 1;
        }
    }
    if n == 0 {
        out.push_str("  (none: default colors only)\n");
    }
    out
}
