//! `screen tab`: drive the Chrome tab the operator allowed for this session, or look at the one
//! it was asked about. The extension runs the act; `letibot browser` holds the grants and carries
//! it on `<rundir>/browser.sock`. The session can only name an act from a fixed list, never a script.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx};

pub const ACTS: &[&str] = &[
    "grants", "shot", "read", "click", "type", "key", "navigate", "scroll",
];

/// The bridge's take and answer windows, plus slack for the socket itself.
const WAIT: Duration = Duration::from_secs(55);

/// A result line larger than this is refused: a viewport PNG as base64 is far below it.
const LINE_MAX: u64 = 48 * 1024 * 1024;

/// `<rundir>/browser.sock`, with the daemon module's rundir rule: `$XDG_RUNTIME_DIR/letibot`,
/// else `/run/user/<uid>/letibot` on Linux and the per-user temp dir on macOS. The bridge binds
/// this same function's answer, so the two cannot disagree.
pub fn browser_socket() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => runtime_base(),
    };
    base.join("letibot").join("browser.sock")
}

#[cfg(not(target_os = "macos"))]
fn runtime_base() -> PathBuf {
    // SAFETY: getuid cannot fail.
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

#[cfg(target_os = "macos")]
fn runtime_base() -> PathBuf {
    let mut buf = vec![0u8; 1024];
    // SAFETY: `confstr` writes at most `buf.len()` bytes and returns how many it wanted.
    let n = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if n == 0 || n > buf.len() {
        return std::env::temp_dir();
    }
    buf.truncate(n - 1);
    PathBuf::from(String::from_utf8_lossy(&buf).into_owned())
}

fn fail(reason: impl Into<String>, payload: impl Into<String>) -> Box<Invocation> {
    Box::new(Invocation::failed(reason, payload))
}

/// One request to the bridge, one answer. `Err` is the finished refusal, boxed: it is large.
fn ask(socket: &Path, req: &Value) -> Result<Value, Box<Invocation>> {
    let not_running = || {
        fail(
            "`letibot browser` is not running",
            format!(
                "nothing answers on {}. The operator installs it once on this machine with \
                 `letibot browser install` (it then starts on demand), then allows a tab for \
                 this session from the extension popup.",
                socket.display()
            ),
        )
    };
    let mut s = UnixStream::connect(socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => not_running(),
        _ => fail("the browser bridge is unreachable", e.to_string()),
    })?;
    let _ = s.set_read_timeout(Some(WAIT));
    writeln!(s, "{req}").map_err(|e| fail("the request was not sent", e.to_string()))?;
    let mut line = String::new();
    BufReader::new(s)
        .take(LINE_MAX)
        .read_line(&mut line)
        .map_err(|e| fail("no answer from the browser bridge", e.to_string()))?;
    if line.trim().is_empty() {
        return Err(fail(
            "no answer from the browser bridge",
            "it closed the connection; is `letibot browser` still running?",
        ));
    }
    let v: Value = serde_json::from_str(&line)
        .map_err(|e| fail("the bridge's answer is not JSON", e.to_string()))?;
    if v["ok"].as_bool() != Some(true) {
        let why = v["error"]
            .as_str()
            .unwrap_or("the bridge said no")
            .to_string();
        let head = if why.starts_with("no tab is allowed") {
            "no tab allowed"
        } else if why.starts_with("this tab is shared for looking only") {
            "look only"
        } else {
            "the tab did not do it"
        };
        return Err(fail(head, why));
    }
    Ok(v)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, k: &str) -> i64 {
    v.get(k)
        .and_then(Value::as_f64)
        .map_or(0, |f| f.round() as i64)
}

/// Page-made text, fenced as data: the page is the one writing it.
fn fenced(ctx: &InvokeCtx<'_>, url: &str, body: &str) -> String {
    let (wrapped, note) = crate::builtins::external::quarantine(
        ctx.call_id(),
        &format!("the operator's Chrome tab, {url}"),
        body,
    );
    match note {
        Some(n) => format!("{wrapped}\n[note] {n}"),
        None => wrapped,
    }
}

pub fn tab(
    ctx: &mut InvokeCtx<'_>,
    session: Option<&str>,
    socket: &Path,
    args: &Value,
) -> Invocation {
    let act = s(args, "act");
    if !ACTS.contains(&act) {
        return Invocation::failed(
            if act.is_empty() {
                "tab needs `act`".to_string()
            } else {
                format!("`{act}` is not a tab act")
            },
            ACTS.join(", "),
        );
    }
    let Some(session) = session else {
        return Invocation::failed(
            "this seat has no session id",
            "a tab is allowed per session, and this tool was registered without one",
        );
    };
    if act == "navigate" {
        let u = s(args, "url").trim().to_ascii_lowercase();
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Invocation::failed("navigate needs an http(s) `url`", "no other scheme");
        }
    }
    let req = json!({ "session": session, "act": act, "args": args });
    let v = match ask(socket, &req) {
        Ok(v) => v,
        Err(inv) => return *inv,
    };
    let r = &v["result"];
    let url = match s(r, "url") {
        "" => s(&v["grant"], "url"),
        u => u,
    }
    .to_string();
    match act {
        "grants" => {
            let g = &v["grant"];
            Invocation::ok(if s(g, "kind") == "look" {
                format!(
                    "tab {} is shared with this session for looking only (`shot`, `read`) while \
                     it answers the operator's question about it, at {}",
                    n(g, "tab"),
                    s(g, "url")
                )
            } else {
                format!(
                    "tab {} may be driven by this session, at {}",
                    n(g, "tab"),
                    s(g, "url")
                )
            })
        }
        "shot" => {
            let bytes = letibot_transcript::media::decode_base64(s(r, "png"));
            let Some(media) = crate::media::Media::of("tab.png", &bytes) else {
                return Invocation::failed(
                    "no picture",
                    "the tab's PNG did not decode; use `read`",
                );
            };
            let mut inv = Invocation::ok(format!(
                "tab {url}: {}\nThe picture is attached. Its pixels are the page's CSS pixels, the \
                 ones `click` x,y take. If it did not arrive, this model cannot see pictures: \
                 `read` gives the page as text.",
                media.summary()
            ));
            inv.media = Some(media);
            inv
        }
        "read" => {
            let mut body = format!(
                "title: {}\nscrolled to y {} of {}, viewport {}x{}\n\n{}\n\n",
                s(r, "title"),
                n(r, "scroll_y"),
                n(r, "height"),
                n(r, "vw"),
                n(r, "vh"),
                s(r, "markdown").trim()
            );
            let els = r["elements"].as_array().cloned().unwrap_or_default();
            body.push_str(&format!(
                "elements in view ({} of {} on the page; `click {{n}}` takes the number):\n",
                els.len(),
                n(r, "total")
            ));
            for e in &els {
                body.push_str(&format!(
                    "[{}] {}{} \"{}\" {} @{},{} {}x{}{}\n",
                    n(e, "n"),
                    s(e, "tag"),
                    match s(e, "kind") {
                        "" => String::new(),
                        k => format!(":{k}"),
                    },
                    s(e, "label"),
                    s(e, "selector"),
                    n(e, "x"),
                    n(e, "y"),
                    n(e, "w"),
                    n(e, "h"),
                    if e["disabled"].as_bool() == Some(true) {
                        " disabled"
                    } else {
                        ""
                    },
                ));
            }
            Invocation::ok(format!("tab {url}\n{}", fenced(ctx, &url, &body)))
        }
        "click" | "key" => {
            let what = if act == "click" {
                format!("clicked at {},{}", n(r, "x"), n(r, "y"))
            } else {
                format!("pressed {}", s(r, "keys"))
            };
            let mut out = format!("{what}. {}", verdict(r, &url));
            if let Some(opened) = r["opened"].as_str() {
                out.push_str(&format!(
                    "\nA NEW TAB OPENED at {opened}. Only the allowed tab can be driven: carry on \
                     here, or ask the operator to allow that one."
                ));
            }
            let page = format!("{}\n{}", s(r, "label"), s(r, "fresh"));
            if !page.trim().is_empty() {
                out.push('\n');
                out.push_str(&fenced(
                    ctx,
                    &url,
                    &format!(
                        "the element: {}\nnew on the page: {}",
                        s(r, "label"),
                        s(r, "fresh")
                    ),
                ));
            }
            Invocation::ok(out)
        }
        "type" => Invocation::ok(format!(
            "typed {} characters{}; the field kept them (checked after a re-render). Now at {url}.",
            n(r, "typed"),
            match s(r, "into") {
                "" => String::new(),
                t => format!(" into {t}"),
            }
        )),
        "navigate" => Invocation::ok(format!(
            "now at {url}{}",
            if r["loaded"].as_bool() == Some(false) {
                " (still loading; `read` again shortly)"
            } else {
                ""
            }
        )),
        "scroll" => Invocation::ok(format!(
            "scrolled to y {} of {} (viewport {})",
            n(r, "scroll_y"),
            n(r, "height"),
            n(r, "vh")
        )),
        _ => Invocation::ok(v.to_string()),
    }
}

/// Whether the page changed, said plainly: an ambiguous answer after a click is a click repeated.
fn verdict(r: &Value, url: &str) -> String {
    match s(r, "verdict") {
        "navigated" => format!("THE PAGE CHANGED: navigated to {url}."),
        "changed" => "THE PAGE CHANGED (same URL, content updated): it DID take effect.".into(),
        _ => "THE PAGE DID NOT CHANGE. Do not simply repeat it: a button that creates or starts \
              something may do it twice. `read` to see what the page waits for."
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_lives_in_the_runtime_dir() {
        let p = browser_socket();
        assert!(p.ends_with("letibot/browser.sock"), "{}", p.display());
    }

    #[test]
    fn a_missing_bridge_is_said_plainly() {
        let gone = std::env::temp_dir().join(format!("lb-no-bridge-{}.sock", std::process::id()));
        let e = ask(&gone, &json!({})).unwrap_err();
        assert!(
            matches!(&e.outcome, letibot_transcript::ToolOutcome::Failed { reason, .. }
                if reason.contains("not running")),
            "{:?}",
            e.outcome
        );
        assert!(e.payload.contains("letibot browser"), "{}", e.payload);
    }
}
