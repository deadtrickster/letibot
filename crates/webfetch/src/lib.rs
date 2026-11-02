//! `web_fetch` behind `curl`, rendered for the model rather than for a browser.
//!
//! `letibot-tools` ships `web_fetch` as a tool that refuses, with [`Fetcher`] as the seam an
//! implementation attaches through; this crate is one implementation.
//!
//! The transport is a `curl` subprocess because the TLS stack, the compression and the protocol
//! edge cases belong to a binary already on the box: this crate's dependencies stay free of them,
//! and a hang is a dead child rather than a dead turn. The policy is here, in the open:
//!
//! - DNS must resolve to a public address, and the connection is pinned to the address that was
//!   checked: every resolved address is held against the private set, and the first public one is
//!   handed to curl as `--resolve`, so the name cannot resolve elsewhere between check and connect.
//!   Loopback is refused first because the model server itself listens on 127.0.0.1, and a fetch
//!   tool that would dial it on request is one the operator has to think about every time.
//! - Same-origin redirects only, followed by hand, re-resolving and re-pinning at every hop.
//!   `--location` would follow anywhere, and a redirect to an intranet host is how a fetch tool
//!   becomes a port scanner. The model can fetch the new address as its own call, visibly.
//! - The body is capped while it is read, and a fetch that stops early says so.
//!
//! The body is rendered, not dumped: raw HTML is the noise the quarantine envelope sits around,
//! so an HTML body goes through reader-mode extraction ([`dom_smoothie`]) and then markdown
//! ([`htmd`]) or prose ([`html2text`]). When extraction finds no confident main content the whole
//! document is converted instead and [`FetchedPage::notes`] says which path ran, because a silent
//! rewrite of a page is the defect the quarantine exists to make visible.
//!
//! It does not run JavaScript, keep state between calls, or cache, and it never presents a body it
//! did not receive whole without saying so.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::Duration;

use letibot_tools::builtins::external::web::{
    FetchError, FetchRequest, FetchedPage, Fetcher, PageFormat,
};

// ---------------------------------------------------------------------------
// github.com: the page is chrome, the file is the page
// ---------------------------------------------------------------------------

/// github.com's own first path segments: never an owner, always a site page. A name here means there
/// is no repo, so the rewrite cannot build `raw…/settings/profile/HEAD/README.md`.
const GITHUB_SITE_SEGMENTS: &[&str] = &[
    "about",
    "billing",
    "campaigns",
    "collections",
    "dashboard",
    "enterprise",
    "events",
    "explore",
    "features",
    "join",
    "login",
    "marketplace",
    "notifications",
    "orgs",
    "pricing",
    "security",
    "sessions",
    "settings",
    "site",
    "topics",
    "trending",
];

/// The `gh` argv that answers a github.com page better than the page: `gh issue view` and `gh pr
/// view` render the body and comments as markdown, and `gh api` answers a commit as JSON. Answers the
/// argv and a word for the note, or `None` when the address is not one of these.
fn gh_args(url: &UrlParts) -> Option<(Vec<String>, &'static str)> {
    if url.host != "github.com" && url.host != "www.github.com" {
        return None;
    }
    let path = url.path_query.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 4 || GITHUB_SITE_SEGMENTS.contains(&segs[0]) {
        return None;
    }
    let repo = format!(
        "{}/{}",
        segs[0],
        segs[1].strip_suffix(".git").unwrap_or(segs[1])
    );
    match (segs[2], segs[3].parse::<u64>().ok()) {
        ("issues", Some(n)) => Some((
            vec![
                "issue".into(),
                "view".into(),
                n.to_string(),
                "-R".into(),
                repo,
            ],
            "issue",
        )),
        ("pull" | "pulls", Some(n)) => Some((
            vec!["pr".into(), "view".into(), n.to_string(), "-R".into(), repo],
            "pull request",
        )),
        // No `gh commit view` exists; the API's JSON is the commit — message,
        // parents, and every hunk as a `patch` field the model reads as text.
        ("commit", _) if !segs[3].is_empty() => Some((
            vec!["api".into(), format!("repos/{repo}/commits/{}", segs[3])],
            "commit",
        )),
        _ => None,
    }
}

/// `gh` on PATH or not. Asked only for the shapes that want it, so the common fetch pays nothing
/// for a tool it will never spawn.
fn gh_present() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run `gh`, capped and timed out the way curl is. A hang is a dead child and a refusal, not a dead
/// turn, and `Err` carries gh's own stderr, where "not authenticated" says itself.
fn run_gh(argv: &[String], cap: usize) -> Result<(Vec<u8>, bool), String> {
    let mut child = Command::new("gh")
        .args(argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawning gh: {e}"))?;
    // stdout is drained on its own thread, capped at the reader: a gh that
    // streams past the cap cannot outlive the deadline through a full pipe.
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut limited = (&mut stdout).take(cap as u64 + 1);
        let _ = limited.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                return Err(format!("gh did not answer in {}s", TIMEOUT.as_secs()));
            }
        }
    };
    let mut out = reader.join().unwrap_or_default();
    if !status.success() {
        let mut err = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }
        let err = err.trim();
        return Err(if err.is_empty() {
            format!("gh exited {status}")
        } else {
            err.to_string()
        });
    }
    let truncated = out.len() > cap;
    if truncated {
        out.truncate(cap);
    }
    Ok((out, truncated))
}

/// Rewrite a github.com address to the raw file it names, or a bare repo to its README.
///
/// A repo page is chrome around a file and the raw host is a tenth of the bytes: `blob/REF/PATH`
/// keeps its ref and path, a bare repo becomes `HEAD/README.md` with the lowercase spelling on a 404,
/// and everything else stays a page. The rewrite is named in a note, not silent, and every check
/// that guards a fetch runs on the rewritten address too.
fn github_raw(url: &UrlParts) -> Option<Vec<String>> {
    if url.host != "github.com" && url.host != "www.github.com" {
        return None;
    }
    let path = url.path_query.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 2 {
        return None; // github.com's own pages — the feed, the login, the search
    }
    // A site page is not a repo, even when it wears two segments:
    // `settings/profile` must not become a readme lookup on a "settings" owner.
    if GITHUB_SITE_SEGMENTS.contains(&segs[0]) {
        return None;
    }
    let (owner, repo) = (segs[0], segs[1].strip_suffix(".git").unwrap_or(segs[1]));
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let raw = |rest: String| format!("https://raw.githubusercontent.com/{owner}/{repo}/{rest}");
    match segs.len() {
        // The repo root: its README, on the default branch, in two spellings.
        2 => Some(vec![
            raw("HEAD/README.md".into()),
            raw("HEAD/readme.md".into()),
        ]),
        // A file at a ref. `blob` is the HTML view, `raw` github's own alias.
        _ if (segs[2] == "blob" || segs[2] == "raw") && segs.len() >= 5 => {
            Some(vec![raw(segs[3..].join("/"))])
        }
        // A tree, an issue, a PR, a release, a wiki — a page, not a file.
        _ => None,
    }
}

/// The note that names a rewrite: both addresses and the reason. A silent rewrite is the defect the
/// quarantine exists to make visible, so this is the visibility, in a function a test can hold.
fn rewrite_note(asked: &UrlParts, first: &str) -> String {
    format!(
        "rewrote the github.com page to the raw file it names: {} → {first} \
         (the repo page is navigation chrome around this file)",
        asked
    )
}

/// How long one hop may take before it is a transport failure. A page the model is waiting on is
/// a turn nobody can interrupt, so this is short; 30s still covers a slow print view.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The body cap, in bytes read from curl. A page that will not fit is reported as `truncated`
/// rather than silently shortened.
pub const MAX_BODY: usize = 256 * 1024;

/// Redirect hops followed by hand. A longer chain is a loop or a tracker, and neither is a page.
pub const MAX_HOPS: usize = 5;

/// How wide `format: text` wraps. Wide enough that a sentence rarely breaks;
/// narrow enough that the result reads as prose rather than as one long line.
const TEXT_WIDTH: usize = 100;

const USER_AGENT: &str = concat!("letibot-webfetch/", env!("CARGO_PKG_VERSION"));

/// Appended to the body on stdout by curl's `--write-out`, and split off again by its last
/// occurrence: the real one is always written after the body, so a body that contains the
/// sentinel cannot forge it.
const SENTINEL: &str = "\n--letibot-fetch-meta--";

/// curl, attached.
pub struct CurlFetcher {
    /// The first line of `curl --version`, for the disclosure. Never parsed again.
    version: String,
    /// The private-address refusal. Off only for tests, which talk to a stand-in on loopback,
    /// the one host the policy exists to refuse.
    check_hosts: bool,
    /// The body cap. `MAX_BODY` in production, small in tests so truncation can be reached.
    cap: usize,
}

impl CurlFetcher {
    /// Attach, checking that curl is there. The error is the operator's to read: refusing at
    /// attach rather than at the first fetch, where it would be a transport error naming nothing.
    pub fn attach() -> Result<CurlFetcher, String> {
        let out = Command::new("curl")
            .arg("--version")
            .output()
            .map_err(|e| {
                format!(
                    "no curl on PATH: {e}. Install curl, or start the daemon \
                     without --web-fetch"
                )
            })?;
        if !out.status.success() {
            return Err(format!("`curl --version` failed with {}", out.status));
        }
        let first = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if first.is_empty() {
            return Err("`curl --version` printed nothing".into());
        }
        Ok(CurlFetcher {
            version: first,
            check_hosts: true,
            cap: MAX_BODY,
        })
    }

    /// The pieces given outright, for a test against a local stand-in: the host check is off because
    /// the stand-in lives on loopback, the one host the policy refuses.
    pub fn for_test(version: &str) -> CurlFetcher {
        CurlFetcher {
            version: version.into(),
            check_hosts: false,
            cap: MAX_BODY,
        }
    }
}

impl Fetcher for CurlFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<FetchedPage, FetchError> {
        // The tool has checked scheme and credentials; the trait doc is
        // explicit that a fetcher does its own checks, because the tool's
        // are one refactor away from gone.
        let mut url = parse_url(&request.url).map_err(FetchError::Refused)?;
        // A github.com address becomes the raw file it names before anything else runs, and the
        // note goes on the result rather than in a log: a caller must be able to cite what it
        // asked for against what came back.
        let (candidates, mut notes) = match github_raw(&url) {
            Some(raws) => {
                let note = rewrite_note(&url, &raws[0]);
                (raws, vec![note])
            }
            None => (vec![url.to_string()], Vec::new()),
        };
        // Issues, PRs and commits go to `gh` when it is on the box, on the same bargain as the raw
        // rewrite. gh absent leaves the page alone, and gh refusing still answers with the page and a
        // note saying why.
        if let Some((argv, kind)) = gh_args(&url)
            && gh_present()
        {
            match run_gh(&argv, self.cap) {
                Ok((body, truncated)) => {
                    let body = String::from_utf8_lossy(&body).to_string();
                    return Ok(FetchedPage {
                        final_url: url.to_string(),
                        status: 200,
                        content_type: "text/markdown".into(),
                        bytes: body.len(),
                        body,
                        truncated,
                        notes: vec![format!(
                            "answered by `gh {}` — the {kind}'s own view, without \
                                 the page's navigation chrome",
                            argv.join(" ")
                        )],
                    });
                }
                Err(why) => notes.push(format!(
                    "gh was tried and refused ({why}); the HTML page follows"
                )),
            }
        }
        // The README case carries two candidates; only a 404 tries the next, and the 404 that
        // survives names the first: a repo with no readme in either spelling answers with the
        // spelling everybody uses, not the fallback.
        let mut first_404 = None;
        for candidate in &candidates {
            url = parse_url(candidate).map_err(FetchError::Refused)?;
            match self.fetch_one(&url, request.format) {
                Ok(mut page) => {
                    let mut all = notes;
                    all.append(&mut page.notes);
                    page.notes = all;
                    return Ok(page);
                }
                Err(e) => {
                    let lost = matches!(&e, FetchError::Status { code: 404, .. });
                    if lost && first_404.is_none() && candidates.len() > 1 {
                        first_404 = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(first_404.expect("an empty candidate list cannot reach here"))
    }

    fn describe(&self) -> String {
        format!(
            "{}, reader-mode extraction to markdown, github.com files rewritten to their \
             raw form and issues/pulls/commits answered by gh when it is on the box, \
             private addresses refused, same-origin redirects only, {} KiB cap",
            self.version,
            self.cap / 1024
        )
    }
}

impl CurlFetcher {
    /// One address through the hop loop: pin, fetch, redirect policy, render. The trait impl above
    /// decides which address; this is everything that happens once it is decided.
    fn fetch_one(&self, url: &UrlParts, format: PageFormat) -> Result<FetchedPage, FetchError> {
        let mut url = url.clone();
        let mut hops = 0usize;
        loop {
            let pin = if self.check_hosts {
                Some(first_public_addr(&url.host, url.port).map_err(FetchError::Refused)?)
            } else {
                None
            };
            let hop = run_curl(&url, pin, self.cap)?;

            if (300..400).contains(&hop.status) {
                let loc = hop.location.ok_or(FetchError::Status {
                    code: hop.status,
                    url: hop.url.clone(),
                })?;
                let next = resolve_location(&url, &loc).map_err(FetchError::Refused)?;
                check_redirect(&url, &next).map_err(FetchError::Refused)?;
                hops += 1;
                if hops >= MAX_HOPS {
                    return Err(FetchError::Refused(format!(
                        "{MAX_HOPS} redirects deep and still redirecting; that is a \
                         loop or a tracker, and neither is a page"
                    )));
                }
                url = next;
                continue;
            }
            if !(200..300).contains(&hop.status) {
                return Err(FetchError::Status {
                    code: hop.status,
                    url: hop.url,
                });
            }

            let (body, notes) = render_body(&hop.body, &hop.content_type, &hop.url, format);
            return Ok(FetchedPage {
                final_url: hop.url,
                status: hop.status,
                content_type: hop.content_type,
                body,
                bytes: hop.body_len,
                truncated: hop.truncated,
                notes,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// one hop
// ---------------------------------------------------------------------------

/// What one curl run produced, before any redirect decision.
struct Hop {
    status: u16,
    content_type: String,
    /// Where these bytes came from — curl's `url_effective`, which for a
    /// single unfollowed hop is the address that was asked for.
    url: String,
    location: Option<String>,
    body: Vec<u8>,
    body_len: usize,
    truncated: bool,
}

fn run_curl(url: &UrlParts, pin: Option<IpAddr>, cap: usize) -> Result<Hop, FetchError> {
    let header_file = std::env::temp_dir().join(format!(
        "letibot-fetch-{}-{}.hdr",
        std::process::id(),
        unique()
    ));
    let mut cmd = Command::new("curl");
    cmd.arg("-sS") // silent; errors still reach stderr
        // A model's url may contain `{}` or `[]`; curl must not glob it.
        .arg("--globoff")
        .arg("--proto")
        .arg("=http,https")
        .arg("--max-time")
        .arg(TIMEOUT.as_secs().to_string())
        .arg("--compressed")
        .arg("--user-agent")
        .arg(USER_AGENT)
        // Headers to a file, the body to stdout, and the metadata line after the body on stdout,
        // so the page's bytes never touch the disk and the split has one unambiguous owner.
        .arg("--dump-header")
        .arg(&header_file)
        .arg("--write-out")
        .arg(format!(
            "{SENTINEL}%{{http_code}}\t%{{content_type}}\t%{{url_effective}}\n"
        ))
        .arg("--output")
        .arg("-");
    if let Some(ip) = pin {
        // The connection is pinned to the address that was checked, so the name cannot resolve
        // somewhere else between the check and the connect. With a pin, curl resolves nothing.
        cmd.arg("--resolve")
            .arg(format!("{}:{}:{}", url.host, url.port, ip));
    }
    cmd.arg("--").arg(url.to_string());

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FetchError::Transport(format!("curl could not start: {e}")))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut buf: Vec<u8> = Vec::with_capacity(cap.min(1 << 20));
    let mut truncated = false;
    let mut scratch = [0u8; 65_536];
    loop {
        match stdout.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() + n > cap {
                    buf.extend_from_slice(&scratch[..cap - buf.len()]);
                    truncated = true;
                    // The cap is ours, not curl's: stop the child, because a body that will not
                    // fit is not worth the rest of the download. A killed run never writes the
                    // metadata line, so the headers file carries the status instead.
                    let _ = child.kill();
                    break;
                }
                buf.extend_from_slice(&scratch[..n]);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&header_file);
                return Err(FetchError::Transport(format!(
                    "reading the body failed: {e}"
                )));
            }
        }
    }
    // stderr is drained only now: with `-sS` it carries at most one error
    // line, so it cannot fill its pipe and deadlock the child while the body
    // is being read.
    let mut stderr_text = String::new();
    if let Some(mut se) = child.stderr.take() {
        let _ = se.read_to_string(&mut stderr_text);
    }
    let headers_text = {
        let t = std::fs::read_to_string(&header_file).unwrap_or_default();
        let _ = std::fs::remove_file(&header_file);
        t
    };
    let (header_status, header_ct, header_loc) = parse_headers(&headers_text);

    if truncated {
        let status = header_status.ok_or_else(|| {
            FetchError::Transport("the fetch was cut off before any header arrived".into())
        })?;
        return Ok(Hop {
            status,
            content_type: header_ct.unwrap_or_default(),
            url: url.to_string(),
            location: header_loc,
            body_len: buf.len(),
            body: buf,
            truncated: true,
        });
    }
    let exit = child
        .wait()
        .map_err(|e| FetchError::Transport(format!("curl did not finish: {e}")))?;
    if !exit.success() {
        let why = stderr_text.trim();
        return Err(FetchError::Transport(if why.is_empty() {
            format!("curl exited with {exit}")
        } else {
            format!("curl: {why}")
        }));
    }

    let sent = SENTINEL.as_bytes();
    let meta_at = buf
        .windows(sent.len())
        .rposition(|w| w == sent)
        .ok_or_else(|| FetchError::Transport("curl produced no fetch metadata".into()))?;
    let body = buf[..meta_at].to_vec();
    let meta = std::str::from_utf8(&buf[meta_at + sent.len()..])
        .map_err(|_| FetchError::Transport("curl's metadata was not utf-8".into()))?;
    let mut parts = meta.trim().split('\t');
    let status: u16 = parts
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| FetchError::Transport("curl's status code did not parse".into()))?;
    let content_type = parts.next().unwrap_or("").trim().to_string();
    let effective = parts.next().unwrap_or("").trim().to_string();
    Ok(Hop {
        status,
        content_type,
        url: if effective.is_empty() {
            url.to_string()
        } else {
            effective
        },
        location: header_loc,
        body_len: body.len(),
        body,
        truncated: false,
    })
}

/// The parts of a header dump this crate acts on. The last status line wins, because a proxy's
/// `100 Continue` arrives before the real one.
fn parse_headers(text: &str) -> (Option<u16>, Option<String>, Option<String>) {
    let mut status = None;
    let mut content_type = None;
    let mut location = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("HTTP/") {
            if let Some(code) = rest.split_whitespace().nth(1).and_then(|c| c.parse().ok()) {
                status = Some(code);
            }
        } else if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_lowercase().as_str() {
                "content-type" => content_type = Some(value.trim().to_string()),
                "location" => location = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    (status, content_type, location)
}

/// A name for the header dump that cannot collide with a concurrent fetch in this process, let
/// alone another one.
fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let n = C.fetch_add(1, Ordering::Relaxed);
    ((std::process::id() as u128) << 32) | n as u128
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// The body rendered for the model, plus the notes that say what was done to it. A non-textual
/// content type renders to nothing, and the type the tool prints is the reason.
fn render_body(
    raw: &[u8],
    content_type: &str,
    final_url: &str,
    format: PageFormat,
) -> (String, Vec<String>) {
    if !is_textual(content_type) {
        return (String::new(), Vec::new());
    }
    let body = String::from_utf8_lossy(raw).into_owned();
    // A body that is not HTML is already plain, so it passes through: the reader-mode pipeline
    // would escape its markdown (`\#`, `\*\*`) and corrupt exactly the files the github rewrite
    // exists to deliver.
    if !content_type.to_lowercase().contains("html") {
        return (body, Vec::new());
    }
    match format {
        PageFormat::Html => (body, Vec::new()),
        PageFormat::Markdown | PageFormat::Text => extract_and_convert(&body, final_url, format),
    }
}

/// Reader mode first, the whole document as the fallback: the scoring finds the thread in a forum
/// page, and an index page or an API reference is what it was never written for. There is a note
/// either way, because a silent rewrite of a page is the defect the quarantine exists to expose.
fn extract_and_convert(html: &str, final_url: &str, format: PageFormat) -> (String, Vec<String>) {
    let total = html.len();
    let mut reader = match dom_smoothie::Readability::new(html, Some(final_url), None) {
        Ok(r) => r,
        Err(_) => return whole_document(html, format, total),
    };
    if !reader.is_probably_readable() {
        return whole_document(html, format, total);
    }
    match reader.parse() {
        Ok(article) => {
            let kept = article.content.len();
            let title = article.title.trim();
            let note = format!(
                "reader mode extracted the main content: {kept} of {total} bytes of \
                 html kept{}",
                if title.is_empty() {
                    String::new()
                } else {
                    format!(" (title: {title})")
                }
            );
            (convert(article.content.as_ref(), format), vec![note])
        }
        Err(_) => whole_document(html, format, total),
    }
}

fn whole_document(html: &str, format: PageFormat, total: usize) -> (String, Vec<String>) {
    let note = format!(
        "reader mode found no confident main content; the whole document \
         ({total} bytes) was converted"
    );
    (convert(html, format), vec![note])
}

fn convert(html: &str, format: PageFormat) -> String {
    match format {
        PageFormat::Html => html.to_string(),
        PageFormat::Markdown => markdown_converter()
            .convert(html)
            .unwrap_or_else(|_| plain_text(html)),
        PageFormat::Text => plain_text(html),
    }
}

/// The markdown converter, with the tags whose contents are program text and layout rather than
/// prose. A page's script reaching the model as text is the noise this pipeline exists to kill.
fn markdown_converter() -> htmd::HtmlToMarkdown {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript", "template"])
        .build()
}

fn plain_text(html: &str) -> String {
    html2text::config::plain()
        .string_from_read(html.as_bytes(), TEXT_WIDTH)
        .unwrap_or_else(|_| String::new())
}

/// Whether the bytes can be prose at all. The parameter half (`; charset=utf-8`) is beside the
/// point; the type decides.
fn is_textual(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("text/")
        || ["json", "xml", "html", "xhtml", "javascript", "csv"]
            .iter()
            .any(|s| ct.contains(s))
}

// ---------------------------------------------------------------------------
// urls, hosts, redirects
// ---------------------------------------------------------------------------

/// The pieces of an http(s) address this crate acts on.
struct UrlParts {
    scheme: String,
    host: String,
    port: u16,
    path_query: String,
}

impl Clone for UrlParts {
    fn clone(&self) -> Self {
        UrlParts {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port,
            path_query: self.path_query.clone(),
        }
    }
}

impl std::fmt::Display for UrlParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default_port(&self.scheme) {
            write!(f, "{}://{}{}", self.scheme, host, self.path_query)
        } else {
            write!(
                f,
                "{}://{}:{}{}",
                self.scheme, host, self.port, self.path_query
            )
        }
    }
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// The fetcher's own scheme and credential checks: the tool makes the same two before calling, and
/// the fetcher repeats them because the tool's are one refactor away from gone.
fn parse_url(url: &str) -> Result<UrlParts, String> {
    let (scheme, rest) = url.split_once("://").ok_or("the address has no scheme")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "web_fetch speaks http and https only, not `{scheme}`"
        ));
    }
    let (authority, path_query) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("the address has no host".into());
    }
    if authority.contains('@') {
        return Err("the address carries credentials".into());
    }
    let (host, port) = if let Some(rest6) = authority.strip_prefix('[') {
        let (h, after) = rest6
            .split_once(']')
            .ok_or("an [ipv6] address without its ]")?;
        let port = after
            .strip_prefix(':')
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| "the port did not parse".to_string())
            })
            .transpose()?
            .unwrap_or_else(|| default_port(&scheme));
        (h.to_lowercase(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
                h.to_lowercase(),
                p.parse::<u16>()
                    .map_err(|_| "the port did not parse".to_string())?,
            ),
            _ => (authority.to_lowercase(), default_port(&scheme)),
        }
    };
    if host.is_empty() {
        return Err("the address has no host".into());
    }
    Ok(UrlParts {
        scheme,
        host,
        port,
        path_query: path_query.to_string(),
    })
}

/// The private set, stated once. Loopback is refused first because the model server listens on
/// 127.0.0.1.
fn addr_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64/10, the carrier-grade NAT range: not routable on the
                // public internet, and std has no predicate for it.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0b1100_0000) == 0b0100_0000))
        }
        IpAddr::V6(v6) => {
            // ::ffff:10.0.0.1 is 10.0.0.1 wearing a sixth hat.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return addr_is_public(IpAddr::V4(mapped));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local())
        }
    }
}

/// Resolve, then refuse anything private. The returned address is the one curl is pinned to, so
/// this is also the only resolving that happens.
fn first_public_addr(host: &str, port: u16) -> Result<IpAddr, String> {
    let addrs: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("the host did not resolve: {e}"))?
        .map(|a| a.ip())
        .collect();
    if addrs.is_empty() {
        return Err("the host did not resolve to any address".into());
    }
    if let Some(public) = addrs.iter().copied().find(|ip| addr_is_public(*ip)) {
        return Ok(public);
    }
    Err(format!(
        "every address {host} resolves to is private ({})",
        addrs
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Same-origin redirects only, and never a scheme downgrade: a redirect off the origin is how a
/// fetch tool becomes a port scanner, and the model can fetch the new address as its own call.
fn check_redirect(from: &UrlParts, to: &UrlParts) -> Result<(), String> {
    if from.host != to.host {
        return Err(format!(
            "the redirect leaves the origin: {} is not {}",
            to.host, from.host
        ));
    }
    if from.scheme == "https" && to.scheme != "https" {
        return Err("the redirect downgrades https to http".into());
    }
    // A port is part of the origin only within one scheme: an http to https upgrade moves 80 to
    // 443 by definition, and that is not a different door.
    if from.scheme == to.scheme && from.port != to.port {
        return Err(format!(
            "the redirect changes the port: {} is not {}",
            to.port, from.port
        ));
    }
    Ok(())
}

/// A `Location` against the url it came from. Absolute, scheme-relative, root-relative,
/// query-only and bare-relative all occur in the wild.
fn resolve_location(base: &UrlParts, loc: &str) -> Result<UrlParts, String> {
    let loc = loc.trim();
    let lower = loc.to_lowercase();
    let joined = if lower.starts_with("http://") || lower.starts_with("https://") {
        loc.to_string()
    } else if let Some(rest) = loc.strip_prefix("//") {
        format!("{}://{}", base.scheme, rest)
    } else if loc.starts_with('/') {
        format!("{}://{}{}", base.scheme, authority_of(base), loc)
    } else if loc.starts_with('?') {
        let path = base.path_query.split(['?', '#']).next().unwrap_or("/");
        format!("{}://{}{}{}", base.scheme, authority_of(base), path, loc)
    } else {
        let dir = match base.path_query.rfind('/') {
            Some(i) => &base.path_query[..=i],
            None => "/",
        };
        format!("{}://{}{}{}", base.scheme, authority_of(base), dir, loc)
    };
    parse_url(&joined)
}

/// The host, with its non-default port, so a rebuilt url says where it really goes.
fn authority_of(base: &UrlParts) -> String {
    let host = if base.host.contains(':') {
        format!("[{}]", base.host)
    } else {
        base.host.clone()
    };
    if base.port == default_port(&base.scheme) {
        host
    } else {
        format!("{host}:{}", base.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    // -- url parsing --------------------------------------------------------

    #[test]
    fn an_address_parses_into_its_parts() {
        let u = parse_url("https://Example.com:8443/a/b?c=d").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 8443);
        assert_eq!(u.path_query, "/a/b?c=d");
        assert_eq!(u.to_string(), "https://example.com:8443/a/b?c=d");
        // The default port is elided, not carried.
        assert_eq!(
            parse_url("http://example.com").unwrap().to_string(),
            "http://example.com/"
        );
        let v6 = parse_url("http://[::1]:8080/x").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.port, 8080);
    }

    #[test]
    fn the_fetcher_repeats_the_tools_own_refusals() {
        assert!(parse_url("ftp://example.com/").is_err());
        assert!(parse_url("https://user:secret@example.com/").is_err());
        assert!(parse_url("https:///no-host").is_err());
    }

    // -- the github.com rewrite ----------------------------------------------

    /// The rewrite is a table, and the table is the whole policy: what becomes raw, what stays a
    /// page, what the README guess is.
    #[test]
    fn github_pages_become_the_raw_files_they_name() {
        let raws = |url: &str| github_raw(&parse_url(url).unwrap());

        // A file at a ref: same ref, same path, raw host, query gone.
        assert_eq!(
            raws("https://github.com/romkatv/gitstatus/blob/master/README.md"),
            Some(vec![
                "https://raw.githubusercontent.com/romkatv/gitstatus/master/README.md".into()
            ])
        );
        // `?plain=1` is an HTML rendering hint; a raw file has no use for it.
        assert_eq!(
            raws("https://github.com/o/r/blob/v1/src/x.rs?plain=1#L3"),
            Some(vec![
                "https://raw.githubusercontent.com/o/r/v1/src/x.rs".into()
            ])
        );
        // github's own `/raw/` alias, and a `.git` suffix on the repo name.
        assert_eq!(
            raws("https://www.github.com/o/r.git/raw/main/a/b.md"),
            Some(vec![
                "https://raw.githubusercontent.com/o/r/main/a/b.md".into()
            ])
        );
        // The repo root: its README, on HEAD - raw's own spelling of the default branch, so the
        // master/main guess is not ours - with the lowercase spelling as a 404-only fallback,
        // because raw is case-sensitive.
        assert_eq!(
            raws("https://github.com/romkatv/gitstatus"),
            Some(vec![
                "https://raw.githubusercontent.com/romkatv/gitstatus/HEAD/README.md".into(),
                "https://raw.githubusercontent.com/romkatv/gitstatus/HEAD/readme.md".into(),
            ])
        );
        // A trailing slash on the root is the same address.
        assert_eq!(
            raws("https://github.com/o/r/"),
            raws("https://github.com/o/r")
        );

        // Pages stay pages. A tree is a directory listing, an issue is a conversation, a wiki and
        // a release have no raw spelling, and a rewrite that guessed one would fetch something
        // the caller did not name.
        for page in [
            "https://github.com/o/r/tree/master/src",
            "https://github.com/o/r/issues/49",
            "https://github.com/o/r/pull/5/files",
            "https://github.com/o/r/releases",
            "https://github.com/o/r/wiki/How-it-works",
            "https://github.com/o/r/commit/abc123",
            // github's own pages, not a repo's
            "https://github.com/login",
            "https://github.com/settings/profile",
            // and hosts that are not github at all
            "https://example.com/o/r/blob/main/x.md",
            "https://gist.github.com/o/abc123",
        ] {
            assert_eq!(
                raws(page),
                None,
                "{page} is not ours to rewrite — it stays the page it is"
            );
        }
    }

    /// The rewrite is named on the result, never silent, so a model that cites what it fetched sees
    /// what it asked for against what came back. The note is the unit here because dialing the real
    /// host needs the network.
    #[test]
    fn the_rewrite_is_named_with_both_addresses_and_never_silent() {
        let asked = parse_url("https://github.com/o/r/blob/main/README.md").unwrap();
        let first = github_raw(&asked).unwrap()[0].clone();
        let note = rewrite_note(&asked, &first);
        assert!(
            note.contains("github.com/o/r/blob/main/README.md")
                && note.contains("raw.githubusercontent.com/o/r/main/README.md"),
            "the note names what was asked and what was fetched: {note}"
        );
        assert!(
            note.contains("chrome"),
            "and says WHY, in a sentence a reader can weigh: {note}"
        );
    }

    /// `gh` answers the shapes whose value is the discussion, not the page, and the rest stay pages:
    /// the table pins which addresses reach gh, and whether gh runs is presence and auth at fetch
    /// time.
    #[test]
    fn issues_pulls_and_commits_reach_gh_and_the_rest_do_not() {
        let argv = |url: &str| gh_args(&parse_url(url).unwrap());

        let (a, kind) = argv("https://github.com/romkatv/gitstatus/issues/49").unwrap();
        assert_eq!(a, vec!["issue", "view", "49", "-R", "romkatv/gitstatus"]);
        assert_eq!(kind, "issue");

        // `/pull/N` and its `/files` tab are the same PR; `pulls` too.
        for spelling in [
            "https://github.com/o/r/pull/5",
            "https://github.com/o/r/pull/5/files",
            "https://github.com/o/r/pulls/5/commits",
        ] {
            let (a, kind) = argv(spelling).unwrap();
            assert_eq!(
                a,
                vec!["pr", "view", "5", "-R", "o/r"],
                "{spelling} is the PR, not its tabs"
            );
            assert_eq!(kind, "pull request");
        }

        // A commit: the API's JSON, with the message, the parents and every hunk as a `patch`
        // field. No `gh commit view` exists to render it prettier.
        let (a, kind) = argv("https://github.com/o/r/commit/abc123").unwrap();
        assert_eq!(a, vec!["api", "repos/o/r/commits/abc123"]);
        assert_eq!(kind, "commit");

        // Everything else stays a page: a tree's ref and path cannot be split without asking the
        // API which branches exist, a release and a wiki have no gh view, an issue by name and a
        // bare repo are not these shapes, and hosts that are not github never reach gh at all.
        for page in [
            "https://github.com/o/r/tree/main/src",
            "https://github.com/o/r/releases",
            "https://github.com/o/r/wiki/How-it-works",
            "https://github.com/o/r/issues/new",
            "https://github.com/o/r",
            "https://github.com/settings/profile",
            "https://example.com/o/r/issues/1",
            "https://gist.github.com/o/r/pull/1",
        ] {
            assert_eq!(
                argv(page),
                None,
                "{page} must not reach gh — it stays the page it is"
            );
        }
    }

    // -- the private set ----------------------------------------------------

    #[test]
    fn private_addresses_are_refused_and_public_ones_pass() {
        for refused in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "::",
        ] {
            let ip: IpAddr = refused.parse().unwrap();
            assert!(!addr_is_public(ip), "{refused} must be refused");
        }
        for public in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            let ip: IpAddr = public.parse().unwrap();
            assert!(addr_is_public(ip), "{public} must pass");
        }
        // The mapped hat.
        assert!(!addr_is_public("::ffff:127.0.0.1".parse().unwrap()));
        assert!(addr_is_public("::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn localhost_refuses_before_anything_is_sent() {
        // The model server lives on 127.0.0.1, and this is the check that keeps a fetched page
        // from pointing the fetcher at it.
        let f = CurlFetcher {
            version: "test".into(),
            check_hosts: true,
            cap: MAX_BODY,
        };
        let req = FetchRequest {
            url: "http://127.0.0.1:8080/".into(),
            format: PageFormat::Markdown,
        };
        match f.fetch(&req) {
            Err(FetchError::Refused(why)) => assert!(why.contains("private"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // -- redirects ----------------------------------------------------------

    #[test]
    fn redirects_stay_in_the_origin_and_never_downgrade() {
        let from = parse_url("https://example.com/a").unwrap();
        let same = parse_url("https://example.com/b").unwrap();
        assert!(check_redirect(&from, &same).is_ok());
        let upgraded_from = parse_url("http://example.com/a").unwrap();
        assert!(
            check_redirect(&upgraded_from, &same).is_ok(),
            "an upgrade is allowed"
        );
        let cross = parse_url("https://other.example.com/b").unwrap();
        assert!(check_redirect(&from, &cross).is_err());
        let downgraded = parse_url("http://example.com/b").unwrap();
        assert!(check_redirect(&from, &downgraded).is_err());
        let other_port = parse_url("https://example.com:8443/b").unwrap();
        assert!(check_redirect(&from, &other_port).is_err());
    }

    #[test]
    fn a_location_resolves_against_its_base() {
        let base = parse_url("https://example.com/a/b/c?x=1").unwrap();
        assert_eq!(
            resolve_location(&base, "https://elsewhere.org/p")
                .unwrap()
                .to_string(),
            "https://elsewhere.org/p"
        );
        assert_eq!(
            resolve_location(&base, "//other.com/q")
                .unwrap()
                .to_string(),
            "https://other.com/q"
        );
        assert_eq!(
            resolve_location(&base, "/root").unwrap().to_string(),
            "https://example.com/root"
        );
        assert_eq!(
            resolve_location(&base, "?y=2").unwrap().to_string(),
            "https://example.com/a/b/c?y=2"
        );
        assert_eq!(
            resolve_location(&base, "sibling").unwrap().to_string(),
            "https://example.com/a/b/sibling"
        );
    }

    // -- headers ------------------------------------------------------------

    #[test]
    fn the_last_status_line_wins_and_headers_are_case_blind() {
        let (status, ct, loc) = parse_headers(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nLOCATION: /next\r\n",
        );
        assert_eq!(status, Some(200));
        assert_eq!(ct.as_deref(), Some("text/html; charset=utf-8"));
        assert_eq!(loc.as_deref(), Some("/next"));
    }

    // -- rendering ----------------------------------------------------------

    #[test]
    fn an_html_page_becomes_markdown_with_its_structure() {
        let html = "<html><head><script>evil()</script><style>.x{}</style></head>\
                    <body><h1>Heading</h1><p>one <a href=\"/x\">two</a> three</p>\
                    <ul><li>a</li><li>b</li></ul></body></html>";
        let (md, notes) = render_body(
            html.as_bytes(),
            "text/html; charset=utf-8",
            "https://e.com/",
            PageFormat::Markdown,
        );
        assert!(md.contains("# Heading"), "{md}");
        assert!(md.contains("[two](/x)"), "{md}");
        assert!(md.contains("*   a"), "{md}");
        assert!(!md.contains("evil"), "{md}");
        // A plain page gets the fallback note, not a fake extraction claim.
        assert!(notes[0].contains("no confident main content"), "{notes:?}");
    }

    #[test]
    fn text_mode_is_prose_without_markup() {
        let html = "<html><body><h1>Heading</h1><p>one two three</p></body></html>";
        let (text, _) = render_body(
            html.as_bytes(),
            "text/html",
            "https://e.com/",
            PageFormat::Text,
        );
        assert!(text.contains("Heading"), "{text}");
        assert!(text.contains("one two three"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }

    /// A body that is not HTML passes through untouched: the github rewrite serves a README as
    /// `text/plain` and the pipeline would escape its markdown. An HTML body still converts, so the
    /// fix cannot rot into "nothing converts".
    #[test]
    fn a_plain_body_passes_through_and_an_html_body_still_converts() {
        let readme = "# gitstatus\n\n**10x faster** than `git status`.\n";
        let (body, notes) = render_body(
            readme.as_bytes(),
            "text/plain; charset=utf-8",
            "https://raw.githubusercontent.com/o/r/HEAD/README.md",
            PageFormat::Markdown,
        );
        assert_eq!(body, readme, "a plain body is not a document to convert");
        assert!(
            notes.is_empty(),
            "and pass-through is not a decision to name: {notes:?}"
        );

        let html = "<html><body><h1>Hi</h1></body></html>";
        let (body, _) = render_body(
            html.as_bytes(),
            "text/html; charset=utf-8",
            "https://e.com/",
            PageFormat::Markdown,
        );
        assert!(body.contains("# Hi"), "an HTML body still converts: {body}");
    }

    #[test]
    fn html_format_passes_the_document_through() {
        let html = "<p>verbatim</p>";
        let (body, notes) = render_body(
            html.as_bytes(),
            "text/html",
            "https://e.com/",
            PageFormat::Html,
        );
        assert_eq!(body, html);
        assert!(notes.is_empty());
    }

    #[test]
    fn a_binary_content_type_renders_to_nothing() {
        let (body, _) = render_body(
            b"\x25PDF-1.4 junk",
            "application/pdf",
            "https://e.com/",
            PageFormat::Markdown,
        );
        assert_eq!(body, "");
    }

    #[test]
    fn an_article_page_is_extracted_and_says_so() {
        // Long enough to clear readability's content threshold, with nav junk around it that
        // the scoring exists to drop.
        let paragraph = "The actual article content goes here, and it goes on for a \
                         while so the scorer has something to score. ";
        let article: String = std::iter::repeat_n(paragraph, 20).collect();
        let html = format!(
            "<html><head><title>Test Article</title></head><body>\
             <nav>SiteNavJunk Home Archives Contact</nav>\
             <article><h1>The Title</h1><p>{article}</p><p>Second paragraph.</p></article>\
             <footer>SiteFooterJunk</footer></body></html>"
        );
        let (md, notes) = render_body(
            html.as_bytes(),
            "text/html",
            "https://e.com/post/1",
            PageFormat::Markdown,
        );
        assert!(
            notes[0].contains("reader mode extracted the main content"),
            "{notes:?}"
        );
        assert!(md.contains("The actual article content"), "{md}");
        assert!(!md.contains("SiteNavJunk"), "{md}");
        assert!(!md.contains("SiteFooterJunk"), "{md}");
    }

    // -- the stand-in -------------------------------------------------------

    /// A tiny HTTP stand-in, loopback only, with the host check off because the one host the policy
    /// refuses is the one a test can bind.
    fn standin(responses: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for resp in responses {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut sock, &mut buf);
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    fn page_ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn fetch(f: &CurlFetcher, url: &str) -> Result<FetchedPage, FetchError> {
        f.fetch(&FetchRequest {
            url: url.into(),
            format: PageFormat::Markdown,
        })
    }

    #[test]
    fn a_page_arrives_rendered_and_quarantine_ready() {
        let (base, t) = standin(vec![page_ok(
            "<html><body><h1>Hi</h1><script>alert(1)</script></body></html>",
        )]);
        let page = fetch(&CurlFetcher::for_test("curl test"), &base).unwrap();
        t.join().unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.final_url, base + "/");
        assert!(page.body.contains("# Hi"), "{}", page.body);
        assert!(!page.body.contains("alert"), "{}", page.body);
        assert!(!page.truncated);
    }

    #[test]
    fn a_same_origin_redirect_is_followed_and_named() {
        let (base, t) = standin(vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            page_ok("<p>second</p>"),
        ]);
        let page = fetch(&CurlFetcher::for_test("curl test"), &base).unwrap();
        t.join().unwrap();
        assert!(page.final_url.ends_with("/next"), "{}", page.final_url);
        assert!(page.body.contains("second"), "{}", page.body);
    }

    #[test]
    fn a_redirect_off_the_origin_is_refused_before_it_is_followed() {
        let (base, t) = standin(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://10.0.0.1/evil\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);
        let e = fetch(&CurlFetcher::for_test("curl test"), &base).unwrap_err();
        t.join().unwrap();
        match e {
            FetchError::Refused(why) => assert!(why.contains("leaves the origin"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_status_error_is_a_status_error() {
        let (base, t) = standin(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);
        let e = fetch(&CurlFetcher::for_test("curl test"), &base).unwrap_err();
        t.join().unwrap();
        match e {
            FetchError::Status { code, url } => {
                assert_eq!(code, 404);
                assert!(url.starts_with("http://127.0.0.1"), "{url}");
            }
            other => panic!("expected a status error, got {other:?}"),
        }
    }

    #[test]
    fn the_body_cap_stops_the_download_and_says_so() {
        let big = "x".repeat(10_000);
        let (base, t) = standin(vec![page_ok(&big)]);
        let f = CurlFetcher {
            version: "test".into(),
            check_hosts: false,
            cap: 1024,
        };
        let page = fetch(&f, &base).unwrap();
        t.join().unwrap();
        assert!(page.truncated);
        assert_eq!(page.bytes, 1024);
        assert_eq!(page.body.len(), 1024);
    }

    #[test]
    fn a_body_cannot_forge_the_metadata_line() {
        // The page contains a fake metadata line; the real one is appended after the body, so
        // the last occurrence is the real one.
        let forged = "hello\n--letibot-fetch-meta--418\tapplication/pdf\thttp://evil/\n";
        let (base, t) = standin(vec![page_ok(forged)]);
        let page = fetch(&CurlFetcher::for_test("curl test"), &base).unwrap();
        t.join().unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.content_type, "text/html; charset=utf-8");
        assert!(page.body.contains("hello"), "{}", page.body);
    }
}
