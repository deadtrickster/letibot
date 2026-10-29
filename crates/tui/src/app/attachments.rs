//! **What the operator pastes, and what the composer makes of it** — `PROTOCOL_VERSION` 50.
//!
//! The operator: *"how i can paste an image to letibot?"* The composer was text and nothing
//! else, so the only way to show a session a picture was to type its path and hope the model
//! called `read`. Three doors now:
//!
//! * **ctrl-v, or the terminal's paste key, with an image on the clipboard** — the bytes are
//!   taken from the clipboard, written under [`crate::app::panes::head_runtime_dir`], and a chip
//!   goes into the composer: `[image 1 · png · 92 KiB]`.
//! * **a bracketed paste whose whole text is one or more existing files** — what a terminal
//!   sends when a file is dragged in (macOS Terminal and iTerm2 paste an absolute path, with the
//!   spaces escaped) — becomes chips too, image or file by what the BYTES are.
//! * **typing nothing at all** — an empty bracketed paste with an image on the clipboard is the
//!   same act as ctrl-v, because that is what a terminal sends for a Cmd+V it has no text for.
//!
//! # Why the chips are TEXT and not a second list beside the editor
//!
//! A chip lives in the composer's own buffer, as the string the operator sees. That is the shape
//! the paste-placeholder ledger already has (`letibot_ui::editor::Editor::pastes`), and it buys
//! three things for free: the cursor moves through it, undo covers it, and what is SUBMITTED
//! carries the same words the screen showed. The ledger here is only what turns a chip back into
//! an [`Attachment`] at send time — a chip deleted from the text is an attachment that is simply
//! not there, with no second bookkeeping to get out of step.
//!
//! **The message keeps its chips.** The text that reaches the model carries `[image 1 · png ·
//! 92 KiB]` and the bytes ride beside it as a `UserPart::Image`; the transcript's user row then
//! shows exactly what the operator put in the composer, which is the same fact drawn from the
//! same words.
//!
//! # Deleting one is one press
//!
//! Backspace inside or at the end of a chip removes the whole chip. A chip is eleven characters
//! of punctuation and digits; deleting it a character at a time would leave `[image 1 · png · 9`
//! in the message — a sentence the model reads and the operator did not write — and the operator
//! would have to count the presses to get it right.
//!
//! # The clipboard is behind a trait, and the tests never touch the real one
//!
//! A test that reads the operator's real clipboard is a test about their desktop, and it would
//! either attach their password manager's picture to a fixture or fail on a headless box.
//! [`Clipboard`] has one production implementor ([`SystemClipboard`]) and the tests use
//! [`FakeClipboard`].

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_sessionlog::protocol::{Attachment, AttachmentKind};

/// **One chip in the composer** — the text the operator sees and the attachment it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chip {
    /// The exact string inserted into the editor, and the thing this chip is found by.
    pub(crate) text: String,
    pub(crate) attachment: Attachment,
}

/// **What the terminal's clipboard holds**, as far as this head is concerned: an image, or
/// nothing it can use.
///
/// `Err` is *the reader could not be asked* — no `osascript`/`wl-paste`/`xclip` on this box — and
/// it is a different answer from `Ok(None)` (*the clipboard holds no picture*), because one has an
/// install to suggest and the other does not. Collapsing them would either hide a missing tool
/// behind silence or tell somebody to install `xclip` because their clipboard held text.
pub(crate) trait Clipboard {
    /// PNG bytes, when the clipboard holds a picture.
    fn image(&self) -> Result<Option<Vec<u8>>, String>;
}

/// The real clipboard: `osascript` on macOS, `wl-paste` and then `xclip` on Linux.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn image(&self) -> Result<Option<Vec<u8>>, String> {
        #[cfg(target_os = "macos")]
        {
            macos_image()
        }
        #[cfg(not(target_os = "macos"))]
        {
            linux_image()
        }
    }
}

/// **The clipboard a head starts with.**
///
/// A TEST BUILD STARTS WITH THE FAKE ONE, and that rule lives here rather than in each test's
/// `app()`: a real clipboard read is a subprocess against the operator's desktop, so a test that
/// reached it would either attach their picture to a fixture or answer about a box with no
/// `osascript` — a test about their machine, not about this head.
///
/// A test that wants a picture sets one with [`crate::app::App::set_clipboard`].
pub(crate) fn default_clipboard() -> Box<dyn Clipboard> {
    #[cfg(test)]
    {
        Box::new(FakeClipboard(Ok(None)))
    }
    #[cfg(not(test))]
    {
        Box::new(SystemClipboard)
    }
}

/// **macOS: the two types a picture is put on the clipboard as.**
///
/// A screenshot is `PNGf`; a copy out of Preview or Safari is `TIFF`, and a head that only asked
/// for one of them would work on half the copies this box makes. `sips` is the converter — part of
/// macOS, and this tree already reaches for system tools (`osascript`, `curl`) rather than
/// linking a decoder for one case.
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_image() -> Result<Option<Vec<u8>>, String> {
    macos_image_in(&attachments_dir())
}

/// **The same read, with the intermediate written where the caller says** — the seam a test would
/// need, and the reason `macos_image` has one: the two files below are named after this PROCESS, so
/// a head a test built and a head the operator runs would write the same path in the same directory.
/// Nothing in a test build may name that directory; see [`attachments_dir`].
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_image_in(dir: &Path) -> Result<Option<Vec<u8>>, String> {
    if let Some(png) = applescript_data("«class PNGf»")? {
        return Ok(Some(png));
    }
    let Some(tiff) = applescript_data("«class TIFF»")? else {
        return Ok(None);
    };
    // A scratch path of this head's own, so two pastes in one session cannot cross and the
    // intermediate is not left in `/tmp`.
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("nowhere to put the picture ({}): {e}", dir.display()))?;
    let stamp = std::process::id();
    let tiff_path = dir.join(format!("clipboard-{stamp}.tiff"));
    let png_path = dir.join(format!("clipboard-{stamp}.png"));
    std::fs::write(&tiff_path, &tiff).map_err(|e| {
        format!(
            "the picture could not be written to {}: {e}",
            tiff_path.display()
        )
    })?;
    let converted = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(&tiff_path)
        .arg("--out")
        .arg(&png_path)
        .output();
    let out = std::fs::read(&png_path).ok();
    let _ = std::fs::remove_file(&tiff_path);
    match converted {
        Ok(o) if !o.status.success() => Err(format!(
            "the clipboard holds a TIFF and `sips` could not convert it: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        _ => Ok(out),
    }
}

/// **Linux: `wl-paste` on Wayland, `xclip` on X11**, and a sentence naming what to install when
/// neither is there — the only useful answer a head can give on a box that has neither.
#[cfg(not(target_os = "macos"))]
#[cfg_attr(test, allow(dead_code))]
fn linux_image() -> Result<Option<Vec<u8>>, String> {
    for (program, args) in [
        ("wl-paste", vec!["--type", "image/png", "--no-newline"]),
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ] {
        match run(program, &args) {
            Ok(Some(bytes)) => return Ok(Some(bytes)),
            Ok(None) => continue,
            Err(Missing::Tool) => continue,
            Err(Missing::Other(e)) => return Err(e),
        }
    }
    Err(
        "no clipboard reader on this box: install `wl-clipboard` (wl-paste) for Wayland or \
         `xclip` for X11, then paste again"
            .to_string(),
    )
}

/// macOS: `osascript -e 'the clipboard as «class PNGf»'` answers `«data PNGf89504E47…»`, or the
/// string `missing value` when the clipboard holds no picture of that type.
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn applescript_data(class: &str) -> Result<Option<Vec<u8>>, String> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(format!("the clipboard as {class}"))
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "no `osascript` on this box, so the clipboard cannot be read — copy the file's \
                 path and paste that instead"
                    .to_string()
            }
            _ => format!("the clipboard could not be read: {e}"),
        })?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_applescript_data(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// **`«data PNGf89504E47…»` as bytes**, or `None` for anything else — `missing value`, an empty
/// clipboard, a type this build does not ask for.
///
/// Split out from the subprocess so the parse is testable on a box with no clipboard at all: the
/// half that goes wrong is the hex, not the `osascript`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_applescript_data(out: &str) -> Option<Vec<u8>> {
    let body = out.trim();
    let body = body.strip_prefix("«data ")?;
    let body = body.strip_suffix('»')?;
    // The four-byte type tag, then hex.
    let hex = body.get(4..)?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let digits = hex.as_bytes();
    for pair in digits.chunks(2) {
        let s = std::str::from_utf8(pair).ok()?;
        bytes.push(u8::from_str_radix(s, 16).ok()?);
    }
    Some(bytes)
}

/// A subprocess's stdout, with the two answers worth telling apart: [`Missing::Tool`] is *install
/// it*, and `Ok(None)` from [`run`] is *it ran and had nothing*.
#[cfg_attr(target_os = "macos", allow(dead_code))]
enum Missing {
    /// The program is not installed.
    Tool,
    /// It ran and said something else went wrong.
    Other(String),
}

/// Run a program and take its stdout, or say which of the two failures it was.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Result<Option<Vec<u8>>, Missing> {
    let out = Command::new(program).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Missing::Tool
        } else {
            Missing::Other(format!("the clipboard could not be read: {e}"))
        }
    })?;
    if !out.status.success() || out.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(out.stdout))
}

/// **The files a paste names** — one path, several, quoted, or with their spaces escaped, which
/// is what every terminal on this box sends when something is dragged in.
///
/// Answers `[]` for a paste that is not entirely made of existing files, and that is the whole of
/// the rule: a sentence that happens to contain `README.md` stays a sentence, because a chip
/// inserted in the middle of somebody's prose would rewrite what they wrote.
pub(crate) fn dropped_paths(text: &str) -> Vec<PathBuf> {
    let words = split_escaped(text);
    if words.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(words.len());
    for w in words {
        let p = Path::new(&w);
        if !p.is_file() {
            return Vec::new();
        }
        // Absolute, so what the head stat'ed is what the daemon opens: its working directory is
        // its own.
        match std::fs::canonicalize(p) {
            Ok(abs) => out.push(abs),
            Err(_) => return Vec::new(),
        }
    }
    out
}

/// Split on whitespace, honouring `'…'`, `"…"` and backslash escapes.
fn split_escaped(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = text.trim().chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => {
                cur.push(c);
                any = true;
            }
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => {
                cur.push(c);
                any = true;
            }
        }
    }
    // **An unterminated quote is not a path list** — a paste that started a `"` is prose that
    // happens to be shaped like one, and guessing at where it ends would swallow the rest.
    if quote.is_some() {
        return Vec::new();
    }
    if any {
        out.push(cur);
    }
    out
}

/// **One file as a chip**, or the reason it cannot be one.
///
/// The kind is what the BYTES are, not what the name says: a `.png` that is really a PDF is a
/// file here and a `FileRef` in the transcript, and the two ends agreeing matters more than the
/// name the operator saw.
pub(crate) fn attachment_of(path: &Path) -> Result<Attachment, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("could not be read: {e}"))?;
    if meta.is_dir() {
        return Err("that is a directory; attach a file".to_string());
    }
    let path_s = path.display().to_string();
    // Four kilobytes is more than every magic number this tree sniffs, and it keeps a chip from
    // reading a 200 MB shard to decide what to call it.
    let head = read_head(path, 4096);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path_s.clone());
    let media = letibot_transcript::media::sniff_mime(&path_s, &head);
    Ok(Attachment {
        kind: match media {
            Some(_) => AttachmentKind::Image,
            None => AttachmentKind::File,
        },
        path: path_s,
        media_type: match media {
            Some(m) => m.to_string(),
            None => text_mime(&name).to_string(),
        },
        bytes_len: meta.len(),
        name,
    })
}

/// The first `n` bytes of a file, or nothing it could read — a chip is not refused for a header
/// this cannot read, it is just called a file.
fn read_head(path: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; n];
    match f.read(&mut buf) {
        Ok(got) => {
            buf.truncate(got);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// A text file's mime, from its name. Advisory only — the daemon reads the bytes — so a type
/// this does not know is `""` rather than an invented one.
fn text_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "log" | "rs" | "py" | "go" | "c" | "h" | "sh" | "toml" | "lisp" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" | "tsv" => "text/csv",
        "html" | "htm" => "text/html",
        "sql" => "text/plain",
        _ => "",
    }
}

/// **The chip the composer draws** — `[image 1 · png · 92 KiB]`, `[file 2 · report.csv · 12 KiB]`.
///
/// The number is the attachment's position in the message as it was minted. It is a LABEL and not
/// an index: a chip deleted from the middle leaves the ones after it numbered as they were, which
/// is the same trade the paste placeholder makes — renumbering would mean rewriting text the
/// operator may have edited.
pub(crate) fn chip_text(nth: usize, a: &Attachment) -> String {
    let kind = match a.kind {
        AttachmentKind::Image => "image",
        AttachmentKind::File => "file",
    };
    let label = match a.kind {
        // `image/png` → `png`: the chip is 28 columns wide and the type is what a person reads.
        AttachmentKind::Image => a
            .media_type
            .split_once('/')
            .map(|(_, t)| t.to_string())
            .unwrap_or_else(|| a.media_type.clone()),
        AttachmentKind::File => a.name.clone(),
    };
    format!("[{kind} {nth} · {label} · {}]", size_said(a.bytes_len))
}

/// A byte count a person reads — `92 KiB`. The same shape the daemon's own sentences use.
pub(crate) fn size_said(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes >= K * K {
        format!("{:.1} MiB", bytes as f64 / (K * K) as f64)
    } else if bytes >= K {
        format!("{} KiB", bytes / K)
    } else {
        format!("{bytes} B")
    }
}

/// **Where a head keeps the pictures it was given** — one function, so a test can be given a
/// directory of its own and no test can reach the operator's.
///
/// In production this is [`crate::app::panes::head_runtime_dir`] + `attachments`: whatever the
/// person had on their clipboard, in a per-user 0700 directory, never `/tmp` at a predictable
/// name. That is the rule the runtime directory already exists for.
///
/// **In a test build it is a directory of the test process's own, and that is a fix rather than a
/// convenience.** MEASURED 2026-10-30: `App::new` starts with `session_id: ""`, so a head a test
/// built and pasted into wrote `$TMPDIR/letibot-<uid>/attachments/-1.png` — into the operator's own
/// paste directory. The bytes it wrote were a 33-byte PNG header, and when `read` later called that
/// file an image the session it was read in was dead for good. So the default here is resolved
/// once, under `$TMPDIR`, and a test that wants its own directory says so with
/// [`crate::app::App::set_attachments_dir`]; the guard is
/// `tests::a_test_build_cannot_name_the_operators_attachment_directory`.
pub(crate) fn attachments_dir() -> PathBuf {
    #[cfg(test)]
    {
        // A directory per test PROCESS, so two test binaries cannot collide either.
        std::env::temp_dir().join(format!(
            "letibot-tui-tests-{}/attachments",
            std::process::id()
        ))
    }
    #[cfg(not(test))]
    {
        crate::app::panes::head_runtime_dir().join("attachments")
    }
}

/// **A clipboard picture kept for the session** — the file the chip points at.
///
/// Under [`crate::app::panes::head_runtime_dir`] and not `/tmp`: this is whatever the person had
/// on their clipboard, and a world-readable file at a predictable name is both a disclosure and
/// the classic symlink target — the rule that directory already exists for.
///
/// The name is the session and a counter, so two pastes never collide and a chip's path says
/// which session asked for it.
pub(crate) fn save_clipboard_image(
    session_id: &str,
    n: u64,
    png: &[u8],
) -> Result<PathBuf, String> {
    save_clipboard_image_under(&attachments_dir(), session_id, n, png)
}

/// **The same write, into a directory the caller names** — the seam `spill_sub_out_under` is, and
/// for the same reason: a test must be able to put the file somewhere of its own, and there must be
/// one implementation of the write for it to reach.
pub(crate) fn save_clipboard_image_under(
    dir: &Path,
    session_id: &str,
    n: u64,
    png: &[u8],
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("nowhere to put the picture ({}): {e}", dir.display()))?;
    let safe: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = dir.join(format!("{safe}-{n}.png"));
    std::fs::write(&path, png).map_err(|e| {
        format!(
            "the picture could not be written to {}: {e}",
            path.display()
        )
    })?;
    Ok(path)
}

/// The tests' clipboard: whatever the test put in it, and nothing else.
pub(crate) struct FakeClipboard(pub(crate) Result<Option<Vec<u8>>, String>);

impl Clipboard for FakeClipboard {
    fn image(&self) -> Result<Option<Vec<u8>>, String> {
        self.0.clone()
    }
}

impl crate::app::App {
    /// **A test's clipboard** — the one door, so no test can reach the operator's own.
    #[cfg(test)]
    pub(crate) fn set_clipboard(&mut self, c: FakeClipboard) {
        self.clipboard = Box::new(c);
    }

    /// **A test's directory for pasted pictures** — the other door, and the one that keeps a
    /// fixture's screenshot out of the operator's paste directory. See [`attachments_dir`].
    #[cfg(test)]
    pub(crate) fn set_attachments_dir(&mut self, dir: std::path::PathBuf) {
        self.attachments_dir = Some(dir);
    }

    /// **Put one file in the composer as a chip.**
    ///
    /// The chip is inserted at the caret, with one space before it unless the line already ends in
    /// whitespace — so `explain <chip>` reads as one sentence and a chip at the start of an empty
    /// composer gets no leading space to trip over.
    pub(crate) fn chip_in(&mut self, a: Attachment) {
        let nth = self.chips.len() + 1;
        let text = chip_text(nth, &a);
        let before = self.input()[..self.editor.cursor().min(self.input().len())].to_string();
        let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        self.editor.insert(&format!("{lead}{text} "));
        self.chips.push(Chip {
            text,
            attachment: a,
        });
        self.redraw = true;
    }

    /// **A bracketed paste, read as what it is** — files when it names files, a picture when the
    /// terminal sent nothing (that is what a Cmd+V on an image produces: no text at all), and
    /// text in every other case.
    ///
    /// Answers whether the paste was consumed. `false` means *the editor should take it*, which is
    /// the ordinary paste of a sentence or a stack trace.
    pub(crate) fn paste_arrived(&mut self, pasted: String) -> bool {
        let files = crate::app::attachments::dropped_paths(&pasted);
        if !files.is_empty() {
            let mut refused: Vec<String> = Vec::new();
            for f in files {
                match crate::app::attachments::attachment_of(&f) {
                    Ok(a) => self.chip_in(a),
                    Err(why) => refused.push(format!("{}: {why}", f.display())),
                }
            }
            if !refused.is_empty() {
                self.say(&format!("not attached — {}", refused.join("; ")));
            }
            return true;
        }
        // **An empty paste is the terminal's way of saying “no text”.** A clipboard holding a
        // picture is exactly that case, and it is the one the operator's question was about.
        if pasted.trim().is_empty() && self.attach_from_clipboard() {
            return true;
        }
        false
    }

    /// **Take the picture on the clipboard into the composer**, if there is one.
    ///
    /// `true` means something happened — a chip, or a sentence saying why not. `false` is *the
    /// clipboard holds no picture*, and it is silent: ctrl-v with text on the clipboard is the
    /// payload window it always was, and a notice about that would be noise on every press.
    pub(crate) fn attach_from_clipboard(&mut self) -> bool {
        let png = match self.clipboard.image() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return false,
            Err(why) => {
                self.say(&why);
                return true;
            }
        };
        // **What the clipboard hands over has to be a picture, not a claim of one.** MEASURED
        // 2026-10-30: a 33-byte PNG header went into the composer as an image chip, was sent, was
        // stored, and a provider refused every request that carried it for ever. The check is the
        // same one `read` and the daemon make — see `letibot_transcript::media::complete_image` —
        // so a truncated copy is refused here, at the one moment a person can paste again.
        if !letibot_transcript::media::complete_image("image/png", &png) {
            self.say(
                "the clipboard's picture is not a complete PNG — nothing was attached. Copy it \
                 again, or save it and paste the file's path instead.",
            );
            return true;
        }
        self.pasted_images += 1;
        let n = self.pasted_images;
        let session = self.session_id.clone();
        let path = match &self.attachments_dir {
            Some(dir) => {
                crate::app::attachments::save_clipboard_image_under(dir, &session, n, &png)
            }
            None => crate::app::attachments::save_clipboard_image(&session, n, &png),
        };
        let path = match path {
            Ok(p) => p,
            Err(why) => {
                self.say(&why);
                return true;
            }
        };
        match crate::app::attachments::attachment_of(&path) {
            Ok(a) => {
                let chip = crate::app::attachments::chip_text(self.chips.len() + 1, &a);
                self.chip_in(a);
                self.say(&format!(
                    "attached {chip} — enter sends it with your message"
                ));
            }
            Err(why) => self.say(&format!(
                "the clipboard picture could not be attached: {why}"
            )),
        }
        true
    }

    /// **Backspace inside a chip takes the whole chip** — see the module's own note.
    ///
    /// Answers whether it did. The chip's ledger entry goes with it, and so does the attachment:
    /// what is left in the composer is what will be sent.
    pub(crate) fn backspace_chip(&mut self) -> bool {
        let text = self.input().to_string();
        let caret = self.editor.cursor().min(text.len());
        let Some((start, end, chip_text)) = self
            .chips
            .iter()
            .filter_map(|c| {
                // The occurrence the caret is at the end of, or inside of: a chip is a span and the
                // caret is where the next character would go. The space a chip is inserted with
                // counts as part of it, so the press right after a paste takes the chip and not
                // just the space.
                text.match_indices(c.text.as_str())
                    .map(|(i, m)| {
                        let e = i + m.len();
                        (i, if text[e..].starts_with(' ') { e + 1 } else { e })
                    })
                    .find(|(i, e)| caret > *i && caret <= *e)
                    .map(|(i, e)| (i, e, c.text.clone()))
            })
            .next()
        else {
            return false;
        };
        // **And the space it was inserted with**, when that is what stands between it and the word
        // before it: a chip deleted out of `explain [image 1 …]` must not leave `explain ` where a
        // caret was.
        let start = if start > 0 && text.as_bytes()[start - 1] == b' ' {
            start - 1
        } else {
            start
        };
        self.editor.kill_range(start..end);
        self.chips.retain(|c| c.text != chip_text);
        self.redraw = true;
        true
    }

    /// **The chips this message carries**, or `None` when one of them has gone and the send must
    /// not happen.
    ///
    /// A chip whose file is gone is refused with a sentence and the whole line is kept, which is
    /// the operator's rule for every refusal here: the words are theirs, and a send that quietly
    /// dropped the picture would be the worst of the three outcomes — the model answering about a
    /// file it never saw while the composer looked like it had sent it.
    pub(crate) fn chips_to_send(&mut self, text: &str) -> Option<Vec<Attachment>> {
        let mut out = Vec::new();
        for chip in &self.chips {
            if !text.contains(&chip.text) {
                continue;
            }
            let path = Path::new(&chip.attachment.path);
            if !path.is_file() {
                let name = chip.attachment.name.clone();
                self.set_composer(text);
                self.say(&format!(
                    "{name} is gone — nothing was sent. Take the chip out and send again, or put \
                     the file back where it was."
                ));
                self.redraw = true;
                return None;
            }
            out.push(chip.attachment.clone());
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-paste-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).expect("a temp dir");
        d
    }

    /// A **real** PNG — the shared fixture, so this module's tests cannot drift back into a header
    /// that is not a picture. See `letibot_transcript::testing` for why that matters.
    fn png() -> Vec<u8> {
        letibot_transcript::testing::png(2, 2)
    }

    /// **The write goes where it is told**, and the seam is the reason a test can say so.
    #[test]
    fn a_clipboard_picture_is_written_into_the_directory_it_is_given() {
        let d = dir("under");
        let path = save_clipboard_image_under(&d, "s-1", 3, &png()).expect("a write");
        assert_eq!(path, d.join("s-1-3.png"), "{} ", path.display());
        assert_eq!(std::fs::read(&path).expect("the file"), png());
        // A session id with punctuation is a NAME, not a path: a separator here would be a write
        // somewhere else entirely.
        let odd = save_clipboard_image_under(&d, "a/b", 1, &png()).expect("a write");
        assert_eq!(odd, d.join("a-b-1.png"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **The default this build resolves is not the operator's directory** — the guard, from the
    /// module that owns the answer. `tests/paste.rs` holds the same rule as behaviour.
    #[test]
    fn a_test_build_resolves_a_directory_of_its_own() {
        let mine = attachments_dir();
        assert_ne!(
            mine,
            crate::app::panes::head_runtime_dir().join("attachments")
        );
        assert!(
            mine.starts_with(std::env::temp_dir()),
            "a test build's directory is not under the temp dir: {}",
            mine.display()
        );
        assert!(
            !mine.starts_with(crate::app::panes::head_runtime_dir()),
            "{}",
            mine.display()
        );
    }

    /// **One path is one chip.**
    #[test]
    fn a_single_path_is_one_dropped_file() {
        let d = dir("single");
        let p = d.join("report.csv");
        std::fs::write(&p, "a,b\n").unwrap();
        let got = dropped_paths(&p.display().to_string());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0], std::fs::canonicalize(&p).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A drag-and-drop pastes SEVERAL, space-separated with the spaces escaped** — the shape
    /// every terminal on this box produces, and the one a naive `split_whitespace` breaks.
    #[test]
    fn several_names_and_an_escaped_space_are_all_paths() {
        let d = dir("several");
        let one = d.join("one file.txt");
        let two = d.join("two.png");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, png()).unwrap();
        // Every space escaped, which is what Finder and iTerm2 paste for a dragged file.
        let pasted = format!(
            "{} {}",
            one.display().to_string().replace(' ', "\\ "),
            two.display()
        );
        let got = dropped_paths(&pasted);
        assert_eq!(got.len(), 2, "a path with a space is ONE path: {got:?}");
        assert_eq!(got[0], std::fs::canonicalize(&one).unwrap());
        assert_eq!(got[1], std::fs::canonicalize(&two).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **Quoted paths, both kinds of quote** — Finder quotes a name with a space when the
    /// terminal's setting says to.
    #[test]
    fn a_quoted_path_is_one_path() {
        let d = dir("quoted");
        let p = d.join("my file.txt");
        std::fs::write(&p, "x").unwrap();
        for quoted in [format!("\"{}\"", p.display()), format!("'{}'", p.display())] {
            let got = dropped_paths(&quoted);
            assert_eq!(got.len(), 1, "{quoted} → {got:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A paste that is not a file stays text**, which is the rule that keeps a sentence from
    /// being rewritten: the words were the operator's.
    #[test]
    fn text_that_is_not_a_file_stays_text() {
        assert!(dropped_paths("explain what README.md says about retries").is_empty());
        assert!(dropped_paths("").is_empty());
        assert!(dropped_paths("/definitely/not/here.png").is_empty());
        // One path and one word is a sentence, not a list of files — no chip is minted from a
        // half-recognised paste.
        let d = dir("mixed");
        let p = d.join("real.txt");
        std::fs::write(&p, "x").unwrap();
        assert!(
            dropped_paths(&format!("{} please", p.display())).is_empty(),
            "a paste that is only PARTLY files is not a paste of files"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **The kind comes from the bytes, and then from the name** — the same order `read` keeps,
    /// so the chip and the part the daemon builds agree. A picture named `.txt` is an image chip
    /// (its magic number says so); a text file with a picture's extension is one too, because the
    /// far end is the authority on what it can read and a refusal from it names the file.
    #[test]
    fn the_chip_is_named_for_what_the_bytes_are() {
        let d = dir("kinds");
        let picture = d.join("shot.txt");
        std::fs::write(&picture, png()).unwrap();
        let a = attachment_of(&std::fs::canonicalize(&picture).unwrap()).unwrap();
        assert_eq!(a.kind, AttachmentKind::Image);
        assert_eq!(a.media_type, "image/png");
        // Bytes that are neither a picture nor a `.png` by name are a FILE, which is the case
        // that matters: a log or a CSV must never be announced as a picture the model cannot read.
        let text = d.join("notes.txt");
        std::fs::write(&text, "this is not a picture at all").unwrap();
        let b = attachment_of(&std::fs::canonicalize(&text).unwrap()).unwrap();
        assert_eq!(b.kind, AttachmentKind::File);
        assert_eq!(b.media_type, "text/plain");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **The chip's words**, which the row in the transcript will show too.
    #[test]
    fn a_chip_says_what_it_is_and_how_big() {
        let image = Attachment {
            kind: AttachmentKind::Image,
            path: "/tmp/shot.png".into(),
            media_type: "image/png".into(),
            bytes_len: 94_208,
            name: "shot.png".into(),
        };
        assert_eq!(chip_text(1, &image), "[image 1 · png · 92 KiB]");
        let file = Attachment {
            kind: AttachmentKind::File,
            path: "/tmp/report.csv".into(),
            media_type: "text/csv".into(),
            bytes_len: 12_288,
            name: "report.csv".into(),
        };
        assert_eq!(chip_text(2, &file), "[file 2 · report.csv · 12 KiB]");
    }

    /// **`osascript`'s answer as bytes**, and every shape that is not one as `None` — measured
    /// against the string the real one prints.
    #[test]
    fn the_applescript_data_is_hex_between_the_guillemets() {
        let out = "«data PNGf89504E470D0A1A0A»\n";
        assert_eq!(
            parse_applescript_data(out),
            Some(vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        );
        assert_eq!(parse_applescript_data("missing value\n"), None);
        assert_eq!(parse_applescript_data(""), None);
        assert_eq!(parse_applescript_data("«data PNGf»"), None);
        assert_eq!(parse_applescript_data("«data PNGfXYZ»"), None);
    }
}
