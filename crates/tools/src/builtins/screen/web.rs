//! `screen web` and `screen dom`: a page rendered by headless Chrome or Chromium inside the seat.
//! One process per call, a throwaway profile under the session's tmp, deleted after. The PNG
//! comes back on stdout as base64, so no file crosses the boundary. Clicking and typing would
//! take a DevTools session (`--remote-debugging-pipe`); this is look-only.

use std::time::Duration;

use serde_json::Value;

use crate::exec::{ScopeKind, SpawnRequest, Waited};
use crate::runtime::{Invocation, InvokeCtx};

const DEFAULT_W: u64 = 1280;
const DEFAULT_H: u64 = 800;
const MAX_W: u64 = 4096;
const MAX_H: u64 = 16_000;
/// The window `full_page` renders in: tall, then the page's own height is what shows.
const FULL_PAGE_H: u64 = 6_000;
const DEFAULT_WAIT_MS: u64 = 1_500;
const MAX_WAIT_MS: u64 = 20_000;
const DEADLINE: Duration = Duration::from_secs(60);
const DOM_BYTES: usize = 48 * 1024;

/// The browser search and the run, as one `sh` script reading its inputs from the environment so
/// no value is ever quoted into it.
const SCRIPT: &str = r#"
for c in "$SCREEN_CHROME" \
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  "/Applications/Chromium.app/Contents/MacOS/Chromium" \
  "$HOME/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
  [ -n "$c" ] && [ -x "$c" ] && { [ -r "$c" ] && chrome=$c || unreadable=$c; } && [ -n "$chrome" ] && break
done
if [ -z "$chrome" ]; then
  for n in google-chrome-stable google-chrome chromium chromium-browser chrome; do
    p=$(command -v "$n" 2>/dev/null) && chrome=$p && break
  done
fi
[ -n "$chrome" ] || { echo "---NOCHROME---"; echo "$unreadable"; exit 0; }
d=$(mktemp -d "${TMPDIR:-/tmp}/screen-web.XXXXXX") || { echo "---NOTMP---"; exit 0; }
trap 'rm -rf "$d"' EXIT
set -- --headless=new --no-sandbox --disable-gpu --hide-scrollbars --no-first-run \
  --no-default-browser-check --disable-crash-reporter --disable-breakpad --disable-extensions \
  --use-mock-keychain --password-store=basic --disable-features=MediaRouter --disable-background-networking \
  --user-data-dir="$d/profile" --window-size="$SCREEN_W,$SCREEN_H" \
  --virtual-time-budget="$SCREEN_WAIT"
if [ "$SCREEN_MODE" = dom ]; then
  "$chrome" "$@" --dump-dom "$SCREEN_URL" >"$d/out.png" 2>"$d/log"
else
  "$chrome" "$@" --screenshot="$d/out.png" "$SCREEN_URL" >"$d/log" 2>&1
fi
code=$?
if [ -s "$d/out.png" ]; then
  echo "---OK---"
  if [ "$SCREEN_MODE" = dom ]; then head -c "$SCREEN_DOM_BYTES" "$d/out.png"; else base64 <"$d/out.png"; fi
else
  echo "---FAILED $code---"
  tail -c 1500 "$d/log"
fi
"#;

/// Run Chrome on `url` in `mode` (`shot` or `dom`) and return what the script printed after its
/// marker, or the sentence for why there is none. The failure is boxed: an `Invocation` is what the
/// caller returns, and this `Result` carries it only on a cold path.
fn run(ctx: &mut InvokeCtx<'_>, args: &Value, mode: &str) -> Result<String, Box<Invocation>> {
    let Some(url) = args.get("url").and_then(|v| v.as_str()).map(str::trim) else {
        return Err(Box::new(Invocation::failed(
            "this action needs `url`",
            "an http(s) or file URL",
        )));
    };
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://"))
    {
        return Err(Box::new(Invocation::failed(
            "unsupported URL",
            "only http://, https:// and file:// are rendered",
        )));
    }
    let Some(host) = ctx.backend.processes() else {
        return Err(Box::new(Invocation::failed(
            "no browser on this seat",
            "this seat has no local process host",
        )));
    };
    let num = |k: &str| args.get(k).and_then(|v| v.as_u64());
    let width = num("width").unwrap_or(DEFAULT_W).clamp(100, MAX_W);
    let full = args.get("full_page").and_then(|v| v.as_bool()) == Some(true);
    let height = match (num("height"), full) {
        (Some(h), _) => h.clamp(100, MAX_H),
        (None, true) => FULL_PAGE_H,
        (None, false) => DEFAULT_H,
    };
    let wait = num("wait_ms").unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS);
    let cwd = ctx
        .backend
        .workdir(".")
        .map_err(|e| Box::new(Invocation::failed("no working directory", e.to_string())))?;
    let env = vec![
        ("SCREEN_URL".to_string(), url.to_string()),
        ("SCREEN_MODE".to_string(), mode.to_string()),
        ("SCREEN_W".to_string(), width.to_string()),
        ("SCREEN_H".to_string(), height.to_string()),
        ("SCREEN_WAIT".to_string(), wait.to_string()),
        ("SCREEN_DOM_BYTES".to_string(), DOM_BYTES.to_string()),
    ];
    let id = host
        .spawn(&SpawnRequest {
            command: SCRIPT.to_string(),
            slug: Some("screen-web".to_string()),
            cwd,
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env,
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .map_err(|e| {
            Box::new(Invocation::failed(
                "the browser did not start",
                e.to_string(),
            ))
        })?;
    let waited = host
        .wait_job(&id, DEADLINE)
        .map_err(|e| Box::new(Invocation::failed("the browser run failed", e.to_string())))?;
    if matches!(waited, Waited::Deadline { .. }) {
        let _ = host.kill_job(&id);
        return Err(Box::new(Invocation::failed(
            "the page took too long",
            format!(
                "no picture inside {}s; the browser was killed",
                DEADLINE.as_secs()
            ),
        )));
    }
    let out = host
        .output(&id, 0, usize::MAX)
        .map_err(|e| Box::new(Invocation::failed("no output", e.to_string())))?;
    if out.dropped > 0 {
        return Err(Box::new(Invocation::failed(
            "the picture was too large to carry",
            "use a smaller `height`, or drop `full_page`",
        )));
    }
    let text = String::from_utf8_lossy(&out.bytes).into_owned();
    let (_, rest) = text.split_once("---").unwrap_or(("", &text));
    let (marker, body) = rest.split_once("---\n").unwrap_or((rest, ""));
    match marker.trim() {
        "OK" => Ok(body.to_string()),
        "NOCHROME" => Err(Box::new(Invocation::failed(
            "no Chrome or Chromium in this seat",
            format!(
                "looked for Google Chrome and Chromium under /Applications and for chromium and \
                 google-chrome on PATH.{} Install chromium where this seat runs (the firecode VM \
                 image), or run `screen web` from a session whose seat has it.",
                match body.trim() {
                    "" => String::new(),
                    p => format!(" `{p}` exists but this seat's view cannot read it."),
                }
            ),
        ))),
        m => {
            let hint = if m.contains("134") {
                "\nChrome aborted while starting. On macOS it needs the window server and \
                 LaunchServices, which the seatbelt profile does not grant and this tool does not \
                 ask for. Use a seat with Linux chromium."
            } else {
                ""
            };
            Err(Box::new(Invocation::failed(
                "the browser produced nothing",
                format!("{m}\n{}{hint}", body.trim()),
            )))
        }
    }
}

pub fn web(ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
    let b64 = match run(ctx, args, "shot") {
        Ok(b) => b,
        Err(e) => return *e,
    };
    let bytes = letibot_transcript::media::decode_base64(&b64);
    let Some(media) = crate::media::Media::of("page.png", &bytes) else {
        return Invocation::failed("no picture", "the browser's PNG did not decode");
    };
    let mut inv = Invocation::ok(format!(
        "{}: {}\nThe picture is attached to this result. If it did not arrive, this model \
         cannot see pictures: `dom` gives the page as text.",
        args.get("url").and_then(|v| v.as_str()).unwrap_or(""),
        media.summary()
    ));
    inv.media = Some(media);
    inv
}

pub fn dom(ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
    let html = match run(ctx, args, "dom") {
        Ok(b) => b,
        Err(e) => return *e,
    };
    let body = if args.get("html").and_then(|v| v.as_bool()) == Some(true) {
        html
    } else {
        visible_text(&html)
    };
    Invocation::ok(body)
}

/// The text a page shows: scripts and styles dropped, tags removed, blank runs collapsed.
fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let rest = &lower[i..];
        if let Some(skip) = ["<script", "<style"].iter().find(|t| rest.starts_with(*t)) {
            let close = format!("</{}", &skip[1..]);
            i = rest.find(&close).map_or(html.len(), |e| i + e);
            i = lower[i..].find('>').map_or(html.len(), |e| i + e + 1);
            continue;
        }
        if rest.starts_with('<') {
            let end = rest.find('>').map_or(html.len(), |e| i + e + 1);
            let block = [
                "<p", "<div", "<br", "<li", "<h", "<tr", "<section", "<article",
            ]
            .iter()
            .any(|t| rest.starts_with(t));
            if block {
                out.push('\n');
            }
            i = end;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    let mut lines: Vec<String> = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{SCRIPT, visible_text};

    /// Chrome on macOS asks the login keychain for its storage key, and a seat that denies it
    /// raises a dialog whose "Reset To Defaults" resets that keychain. Nothing here launches
    /// Chrome without the flags that keep it away from it.
    #[test]
    fn the_command_line_keeps_chrome_off_the_keychain() {
        for flag in [
            "--use-mock-keychain",
            "--password-store=basic",
            "--disable-crash-reporter",
            "--no-first-run",
            "--no-default-browser-check",
            "--user-data-dir=",
        ] {
            assert!(SCRIPT.contains(flag), "{flag} is missing from the script");
        }
        // One launch line per mode, both through the shared `set --` list.
        assert_eq!(SCRIPT.matches("\"$chrome\" \"$@\"").count(), 2);
    }

    #[test]
    fn the_text_view_drops_markup_scripts_and_styles() {
        let html = "<html><head><style>p{}</style></head><body><h1>Title</h1><p>one  <b>two</b></p>\
                    <script>var x = '<p>no</p>';</script><div>three &amp; four</div></body></html>";
        assert_eq!(visible_text(html), "Title\none two\nthree & four");
    }
}
