//! What the operator pastes and what the composer makes of it: an image on the clipboard, a
//! bracketed paste of existing files, and the empty bracketed paste a Cmd+V on an image sends. A
//! chip is text in the composer's own buffer, so cursor, undo and submit carry the words shown.

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_sessionlog::protocol::{Attachment, AttachmentKind};

/// One chip in the composer - the text the operator sees and the attachment it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chip {
    /// The exact string inserted into the editor, and the thing this chip is found by.
    pub(crate) text: String,
    pub(crate) attachment: Attachment,
}

/// What the terminal's clipboard holds as far as this head is concerned: an image, or nothing it
/// can use. `Err` is *the reader could not be asked* - the one answer with an install to suggest -
/// and collapsing it into `Ok(None)` would hide a missing tool behind silence.
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

/// The clipboard a head starts with. A test build starts with the fake one, and the rule lives here
/// rather than in each test's `app()`, because a real read is a subprocess against the operator's
/// desktop: it would attach their picture to a fixture or answer about their box, not this head.
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

/// macOS: the two types a picture is put on the clipboard as - `PNGf` from a screenshot, `TIFF` from
/// Preview or Safari - and a head asking for one would work on half the copies this box makes.
/// `sips` converts, because this tree reaches for system tools rather than linking a decoder.
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_image() -> Result<Option<Vec<u8>>, String> {
    macos_image_in(&attachments_dir())
}

/// The same read with the intermediate written where the caller says: the two files are named after
/// this PROCESS, so a head a test built would write the operator's own path. Nothing in a test build
/// may name that directory; see [`attachments_dir`].
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

/// Linux: `wl-paste` on Wayland, `xclip` on X11, and a sentence naming what to install when
/// neither is there - the only useful answer a head can give on a box that has neither.
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
        // A refusal is not an empty clipboard: `missing value` with exit 0 means no picture, and a
        // non-zero exit is `osascript` saying why it could not read. Answering `Ok(None)` to that
        // made the two indistinguishable and said nothing at all.
        if let Some(Missing::Other(why)) = refused(
            &out,
            &format!("`osascript` could not read the clipboard as {class}"),
        ) {
            return Err(why);
        }
        return Ok(None);
    }
    Ok(parse_applescript_data(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// A reader that ran and said why it failed - the answer between *install it* and *there is no
/// picture*. A non-zero exit with a sentence is not an empty clipboard, and answering `Ok(None)` to
/// it leaves the operator pressing the chord again; an empty stderr has no sentence to show.
#[cfg(target_os = "macos")]
fn refused(out: &std::process::Output, what: &str) -> Option<Missing> {
    if out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim();
    if said.is_empty() {
        return None;
    }
    Some(Missing::Other(format!("{what}: {said}")))
}

/// `«data PNGf89504E47…»` as bytes, or `None` for anything else - `missing value`, an empty
/// clipboard, a type this build does not ask for. Split from the subprocess so the parse is
/// testable where there is no clipboard: the half that goes wrong is the hex.
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

/// How long a clipboard reader may take. It runs on the head's event loop, and a reader whose
/// clipboard owner never answers would otherwise freeze the head, keys included.
const CLIPBOARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Run a program and take its stdout, or say which of the two failures it was.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Result<Option<Vec<u8>>, Missing> {
    run_within(program, args, CLIPBOARD_DEADLINE)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run_within(
    program: &str,
    args: &[&str],
    limit: std::time::Duration,
) -> Result<Option<Vec<u8>>, Missing> {
    use crate::gitfield::Ran;
    let mut cmd = Command::new(program);
    cmd.args(args);
    match crate::gitfield::run_until(cmd, limit) {
        Ran::Ok(out) if !out.is_empty() => Ok(Some(out)),
        Ran::Ok(_) | Ran::Failed => Ok(None),
        Ran::NotStarted(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Missing::Tool),
        Ran::NotStarted(e) => Err(Missing::Other(format!(
            "the clipboard could not be read: {e}"
        ))),
        Ran::TimedOut => Err(Missing::Other(format!(
            "`{program}` did not answer within {}s, so nothing was pasted - the program that \
             owns the clipboard may be stuck",
            limit.as_secs()
        ))),
    }
}

/// The files a paste names - one path, several, quoted, or with their spaces escaped. `[]` for a
/// paste that is not entirely existing files, which is the whole rule: a chip in the middle of
/// somebody's prose would rewrite what they wrote.
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
    // An unterminated quote is not a path list - a paste that started a `"` is prose that
    // happens to be shaped like one, and guessing at where it ends would swallow the rest.
    if quote.is_some() {
        return Vec::new();
    }
    if any {
        out.push(cur);
    }
    out
}

/// One file as a chip, or the reason it cannot be one. The kind is what the BYTES are, not what the
/// name says, because the two ends agreeing matters more than the name the operator saw.
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

/// The first `n` bytes of a file, or nothing it could read - a chip is not refused for a header
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

/// A text file's mime, from its name. Advisory only - the daemon reads the bytes - so a type
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

/// The chip the composer draws, from the attachment alone. The number is a LABEL and not an index:
/// a chip deleted from the middle leaves the ones after it numbered as they were, because
/// renumbering would mean rewriting text the operator may have edited.
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

/// A byte count a person reads - `92 KiB`. The same shape the daemon's own sentences use.
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

/// Where a head keeps the pictures it was given: one function, so a test can be given a directory of
/// its own and no test can reach the operator's. Production is a per-user 0700 runtime directory and
/// never `/tmp`; a test build resolves a directory of its own under `$TMPDIR` instead.
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

/// A clipboard picture kept for the session - the file the chip points at. Under the runtime
/// directory and not `/tmp`, because whatever was on somebody's clipboard at a world-readable
/// predictable name is a disclosure and the classic symlink target. The name is session + counter.
pub(crate) fn save_clipboard_image(
    session_id: &str,
    n: u64,
    png: &[u8],
) -> Result<PathBuf, String> {
    save_clipboard_image_under(&attachments_dir(), session_id, n, png)
}

/// The same write, into a directory the caller names - the seam `spill_sub_out_under` is, and
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
#[cfg(test)]
pub(crate) struct FakeClipboard(pub(crate) Result<Option<Vec<u8>>, String>);

#[cfg(test)]
impl Clipboard for FakeClipboard {
    fn image(&self) -> Result<Option<Vec<u8>>, String> {
        self.0.clone()
    }
}

impl crate::app::App {
    /// A test's clipboard - the one door, so no test can reach the operator's own.
    #[cfg(test)]
    pub(crate) fn set_clipboard(&mut self, c: FakeClipboard) {
        self.clipboard = Box::new(c);
    }

    /// A test's directory for pasted pictures - the other door, and the one that keeps a
    /// fixture's screenshot out of the operator's paste directory. See [`attachments_dir`].
    #[cfg(test)]
    pub(crate) fn set_attachments_dir(&mut self, dir: std::path::PathBuf) {
        self.attachments_dir = Some(dir);
    }

    /// Put one file in the composer as a chip, at the caret, with one space before it unless the
    /// line already ends in whitespace: so `explain <chip>` reads as one sentence and a chip in an
    /// empty composer gets no leading space to trip over.
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

    /// A bracketed paste read as what it is: files when it names files, a picture when the terminal
    /// sent no text at all (a Cmd+V on an image), and text in every other case. Answers whether the
    /// paste was consumed - `false` means the editor should take it.
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
        // An empty paste is the terminal's way of saying “no text”. A clipboard holding a
        // picture is exactly that case, and it is the one the operator's question was about.
        if pasted.trim().is_empty() && self.attach_from_clipboard() {
            return true;
        }
        false
    }

    /// Take the picture on the clipboard into the composer, if there is one. `true` means something
    /// happened - a chip, or a sentence saying why not. `false` is silent, because ctrl-v with text
    /// on the clipboard is the payload window it always was.
    pub(crate) fn attach_from_clipboard(&mut self) -> bool {
        let png = match self.clipboard.image() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return false,
            Err(why) => {
                self.say(&why);
                return true;
            }
        };
        // What the clipboard hands over has to be a picture, not a claim of one: a header with no
        // body was sent and stored once, and a provider then refused every request carrying it for
        // ever. The check is the one `read` and the daemon make, at the one moment a person can re-paste.
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

    /// Backspace inside a chip takes the whole chip. Answers whether it did: the ledger entry goes
    /// with it, and so does the attachment, so what is left in the composer is what will be sent.
    pub(crate) fn backspace_chip(&mut self) -> bool {
        let text = self.input().to_string();
        let caret = self.editor.cursor().min(text.len());
        let Some((start, end, chip_text)) = self
            .chips
            .iter()
            .filter_map(|c| {
                // The occurrence the caret is at the end of, or inside of: a chip is a span and the
                // caret is where the next character would go. The space a chip is inserted with
                // counts as part of it, so the press right after a paste takes the chip, not the space.
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
        // And the space it was inserted with, when that is what stands between it and the word
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

    /// The chips this message carries, or `None` when one of them has gone and the send must not
    /// happen. A chip whose file is gone refuses the send and keeps the line: the words are the
    /// operator's, and a send that dropped the picture silently would answer about a file never seen.
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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_clipboard_reader_that_never_answers_is_cut_off() {
        let began = std::time::Instant::now();
        let r = run_within("sleep", &["30"], std::time::Duration::from_millis(300));
        assert!(began.elapsed() < std::time::Duration::from_secs(5));
        assert!(matches!(r, Err(Missing::Other(ref e)) if e.contains("did not answer")));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_missing_reader_is_missing_and_an_empty_one_is_no_picture() {
        let limit = std::time::Duration::from_secs(5);
        assert!(matches!(
            run_within("letibot-no-such-clipboard-tool", &[], limit),
            Err(Missing::Tool)
        ));
        assert!(matches!(run_within("true", &[], limit), Ok(None)));
        assert!(matches!(run_within("false", &[], limit), Ok(None)));
        assert!(matches!(run_within("echo", &["x"], limit), Ok(Some(ref b)) if b == b"x\n"));
    }
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

    /// A real PNG - the shared fixture, so this module's tests cannot drift back into a header
    /// that is not a picture. See `letibot_transcript::testing` for why that matters.
    fn png() -> Vec<u8> {
        letibot_transcript::testing::png(2, 2)
    }

    /// The write goes where it is told, and the seam is the reason a test can say so.
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

    /// The default this build resolves is not the operator's directory - the guard, from the
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

    /// One path is one chip.
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

    /// A drag-and-drop pastes SEVERAL, space-separated with the spaces escaped - the shape
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

    /// Quoted paths, both kinds of quote - Finder quotes a name with a space when the
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

    /// A paste that is not a file stays text, which is the rule that keeps a sentence from
    /// being rewritten: the words were the operator's.
    #[test]
    fn text_that_is_not_a_file_stays_text() {
        assert!(dropped_paths("explain what README.md says about retries").is_empty());
        assert!(dropped_paths("").is_empty());
        assert!(dropped_paths("/definitely/not/here.png").is_empty());
        // One path and one word is a sentence, not a list of files - no chip is minted from a
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

    /// The kind comes from the bytes, and then from the name - the same order `read` keeps, so the
    /// chip and the part the daemon builds agree. The far end is the authority on what it can read.
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

    /// The chip's words, which the row in the transcript will show too.
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

    /// `osascript`'s answer as bytes, and every shape that is not one as `None` - measured
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

    /// A refusal is not an empty clipboard. `osascript` exits non-zero when the automation permission
    /// is denied, and answering `Ok(None)` to that made the chord do nothing and say nothing: exit 0
    /// is no picture, a failure with a sentence is that sentence, and an empty stderr is no picture.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_refused_clipboard_read_says_why_and_a_silent_one_is_no_picture() {
        use std::os::unix::process::ExitStatusExt;
        let out = |code: i32, stderr: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        // Exit 0 is the reader answering, whatever it said on stderr.
        assert!(refused(&out(0, "a warning nobody asked for"), "the clipboard").is_none());
        // A failure with a sentence is that sentence, named.
        let said = match refused(
            &out(
                1 << 8,
                "execution error: Not authorized to send Apple events. (-1743)\n",
            ),
            "`osascript` could not read the clipboard as «class PNGf»",
        ) {
            Some(Missing::Other(why)) => why,
            _ => panic!("a refusal that said why is a sentence"),
        };
        assert_eq!(
            said,
            "`osascript` could not read the clipboard as «class PNGf»: execution error: Not \
             authorized to send Apple events. (-1743)"
        );
        // A failure that said nothing has nothing to say.
        assert!(refused(&out(1 << 8, "  \n"), "the clipboard").is_none());
    }
}
