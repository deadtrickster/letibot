//! `letibot browser`: the local bridge the Chrome extension (`extensions/chrome`) talks to.
//!
//! ```text
//!   extension worker ──HTTP 127.0.0.1:8790, bearer token──▶ this ──unix socket──▶ the session's daemon
//! ```
//!
//! Three doors are checked on every request, in order: the `Host` header names loopback (a
//! rebound DNS name is refused), the `Origin` is the extension's, and the bearer token matches
//! `browser.token`. Only the operator's live top-level sessions can be asked.
//!
//! The same doors front `/tab/*`, through which the extension reports the tabs the operator
//! allowed and runs the commands sessions send on `<rundir>/browser.sock` (`tab`). An ask also
//! opens a look grant on its own tab for as long as the turn that took it runs.

pub mod ask;
pub mod http;
pub mod sessions;
pub mod tab;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

/// The port when `[browser] port` says nothing.
pub const DEFAULT_PORT: u16 = 8790;

/// The id the `key` in `extensions/chrome/manifest.json` gives the unpacked extension.
pub const EXTENSION_ID: &str = "kbaoealgdkldcgaojgdehmdaaekkmffd";

/// The identity the bridge's head attaches under, so the daemon's log says who prompted.
pub const IDENTITY: &str = "letibot-browser";

/// The sizes an ask may carry. Each is a resource the tree already bounds, and a field over its
/// limit is refused with both numbers, never cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub question: u64,
    pub selection: u64,
    /// The daemon's own ceiling for inlining an attached text file.
    pub markdown: u64,
    pub agents_md: u64,
    /// The largest file a search opens (`grep::FILE_CEILING`): a screenshot is far below it.
    pub image: u64,
    /// Everything above as JSON, the picture as base64, and room for the keys.
    pub body: u64,
}

pub const LIMITS: Limits = Limits {
    question: 16 * 1024,
    selection: 64 * 1024,
    markdown: 256 * 1024,
    agents_md: 16 * 1024,
    image: letibot_tools::builtins::grep::FILE_CEILING,
    body: letibot_tools::builtins::grep::FILE_CEILING / 3 * 4
        + 16 * 1024
        + 2 * 64 * 1024
        + 256 * 1024
        + 16 * 1024
        + 64 * 1024,
};

impl Limits {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "question": self.question,
            "selection": self.selection,
            "markdown": self.markdown,
            "agents_md": self.agents_md,
            "image": self.image,
            "body": self.body,
        })
    }
}

/// Everything one bridge answers with.
pub struct Server {
    pub token: String,
    /// `chrome-extension://<id>`: the one origin answered.
    pub origin: String,
    pub port: u16,
    pub store: PathBuf,
    pub rundir: PathBuf,
    /// Where pictures wait for the daemon to read them.
    pub inbox: PathBuf,
    pub limits: Limits,
    /// How long a stream follows a turn before it says so and stops.
    pub patience: Duration,
    /// The allowed tabs and the commands waiting for them.
    pub tabs: Arc<tab::Tabs>,
}

/// Bind the listener, refusing anything that is not a loopback address.
pub fn bind(addr: SocketAddr) -> Result<TcpListener, String> {
    if !addr.ip().is_loopback() {
        return Err(format!(
            "{addr} is not a loopback address: the browser bridge listens on 127.0.0.1 only, \
             because the token is the only thing between the network and an operator prompt"
        ));
    }
    TcpListener::bind(addr).map_err(|e| format!("binding {addr}: {e}"))
}

/// The token, created on first use: 32 random bytes as hex, mode 0600. `true` when this call
/// made it, which is the one time it is printed.
pub fn token(path: &Path) -> Result<(String, bool), String> {
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Ok(text) = std::fs::read_to_string(path) {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} is readable by others (mode {:o}); `chmod 600` it or delete it for a new one",
                path.display(),
                meta.permissions().mode() & 0o777
            ));
        }
        let t = text.trim().to_string();
        if t.len() < 32 {
            return Err(format!(
                "{} holds no usable token; delete it for a new one",
                path.display()
            ));
        }
        return Ok((t, false));
    }
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let t: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{t}").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((t, true))
}

/// Byte equality whose time does not depend on where the first difference is.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Accept forever, one thread per connection. The listing is cached for a second: the card and
/// the popup ask together, and each listing attaches to every running daemon.
pub fn serve(listener: TcpListener, server: Arc<Server>) {
    let cache: Arc<Mutex<Option<(Instant, Vec<sessions::Row>)>>> = Arc::default();
    for stream in listener.incoming().flatten() {
        let (server, cache) = (server.clone(), cache.clone());
        let _ = std::thread::Builder::new()
            .name("browser-conn".into())
            .spawn(move || handle(stream, &server, &cache));
    }
}

fn listing(
    server: &Server,
    cache: &Mutex<Option<(Instant, Vec<sessions::Row>)>>,
) -> Result<Vec<sessions::Row>, String> {
    let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, rows)) = g.as_ref()
        && at.elapsed() < Duration::from_secs(1)
    {
        return Ok(rows.clone());
    }
    let rows = sessions::list(&server.store, &server.rundir, crate::fleet::ATTACH_TIMEOUT)?;
    *g = Some((Instant::now(), rows.clone()));
    Ok(rows)
}

/// The door checks, then the route. A refusal never carries the allow-origin header unless the
/// origin was the extension's, so a page cannot read why it was refused.
fn handle(
    mut stream: TcpStream,
    server: &Server,
    cache: &Mutex<Option<(Instant, Vec<sessions::Row>)>>,
) {
    let req = match http::read_request(&mut stream, server.limits.body as usize) {
        Ok(r) => r,
        Err(r) => return http::error(&mut stream, r.status, None, &r.why),
    };
    let host = req.header("host").unwrap_or_default();
    let port = server.port;
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return http::error(
            &mut stream,
            403,
            None,
            &format!("Host `{host}` is not this bridge (127.0.0.1:{port})"),
        );
    }
    // Chrome sends no Origin on an extension's GET to a host it has permission for. A page
    // cannot set the bearer header without a CORS request, which always names its Origin.
    let origin = req.header("origin").unwrap_or_default();
    if !origin.is_empty() && origin != server.origin {
        return http::error(
            &mut stream,
            403,
            None,
            &format!(
                "Origin `{origin}` is not the letibot extension ({}); set `[browser] \
                 extension_id` if you loaded it under another id",
                server.origin
            ),
        );
    }
    let allow = Some(server.origin.as_str());
    if req.method == "OPTIONS" {
        return http::preflight(&mut stream, &server.origin);
    }
    let bearer = req
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !same(bearer.trim().as_bytes(), server.token.as_bytes()) {
        return http::error(
            &mut stream,
            401,
            allow,
            if bearer.is_empty() {
                "no token: paste the one `letibot browser` printed into the extension's options"
            } else {
                "wrong token: paste the one in ~/.config/letibot/browser.token into the extension's options"
            },
        );
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/sessions") => match listing(server, cache) {
            Ok(rows) => http::json(
                &mut stream,
                200,
                allow,
                &json!({
                    "sessions": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
                    "limits": server.limits.to_json(),
                }),
            ),
            Err(why) => http::error(&mut stream, 500, allow, &why),
        },
        ("POST", "/ask") => ask_route(&mut stream, server, cache, &req.body),
        ("POST", "/tab/sync") => tab_sync(&mut stream, server, cache, &req.body, false),
        ("POST", "/tab/next") => tab_sync(&mut stream, server, cache, &req.body, true),
        ("POST", "/tab/result") => tab_result(&mut stream, server, &req.body),
        (_, "/sessions" | "/ask" | "/tab/sync" | "/tab/next" | "/tab/result") => {
            http::error(&mut stream, 405, allow, "wrong method")
        }
        (_, path) => http::error(&mut stream, 404, allow, &format!("no route {path}")),
    }
}

/// The extension's grant list replaces the table; `/tab/next` then holds the poll open for one
/// command. The answer says which grants were refused, so the extension detaches those tabs.
fn tab_sync(
    stream: &mut TcpStream,
    server: &Server,
    cache: &Mutex<Option<(Instant, Vec<sessions::Row>)>>,
    body: &[u8],
    poll: bool,
) {
    let allow = Some(server.origin.as_str());
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return http::error(stream, 400, allow, &format!("not JSON: {e}")),
    };
    let mut list = Vec::new();
    for g in v["grants"].as_array().into_iter().flatten() {
        let (Some(session), Some(tab)) = (g["session"].as_str(), g["tab"].as_i64()) else {
            return http::error(stream, 400, allow, "a grant is {session, tab, url, title}");
        };
        let text = |k: &str| g[k].as_str().unwrap_or_default().to_string();
        list.push((
            session.to_string(),
            tab::Grant {
                tab,
                url: text("url"),
                title: text("title"),
                kind: tab::Kind::Drive,
            },
        ));
    }
    let synced = server.tabs.sync(list, |id| match listing(server, cache) {
        Ok(rows) if rows.iter().any(|r| r.id == id) => Ok(()),
        Ok(_) => Err(format!("`{id}` is not a live top-level session of yours")),
        Err(why) => Err(why),
    });
    let command = if poll && server.tabs.any() {
        server.tabs.next(tab::POLL_WAIT)
    } else {
        None
    };
    http::json(
        stream,
        200,
        allow,
        &json!({
            "kept": synced.kept,
            "refused": synced.refused.iter()
                .map(|(s, why)| json!({ "session": s, "why": why }))
                .collect::<Vec<_>>(),
            "command": command,
        }),
    );
}

fn tab_result(stream: &mut TcpStream, server: &Server, body: &[u8]) {
    let allow = Some(server.origin.as_str());
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return http::error(stream, 400, allow, &format!("not JSON: {e}")),
    };
    let Some(id) = v["id"].as_str() else {
        return http::error(stream, 400, allow, "a result is {id, ok, result | error}");
    };
    let outcome = if v["ok"].as_bool() == Some(true) {
        Ok(v["result"].clone())
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("the extension said no")
            .to_string())
    };
    let waited = server.tabs.finish(id, outcome);
    http::json(stream, 200, allow, &json!({ "taken": waited }));
}

/// An id the page cannot guess. It never reaches the session as text: the stream recognises the
/// ask's own row by the page fence's mark, which is derived from it.
fn ask_id() -> Result<String, String> {
    let mut raw = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut raw))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(format!(
        "b-{}",
        raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

fn ask_route(
    stream: &mut TcpStream,
    server: &Server,
    cache: &Mutex<Option<(Instant, Vec<sessions::Row>)>>,
    body: &[u8],
) {
    let allow = Some(server.origin.as_str());
    let body: ask::AskBody = match serde_json::from_slice(body) {
        Ok(b) => b,
        Err(e) => return http::error(stream, 400, allow, &format!("the ask is not JSON: {e}")),
    };
    if let Err(why) = ask::check(&body, &server.limits) {
        let status = if why.contains("at most") { 413 } else { 400 };
        return http::error(stream, status, allow, &why);
    }
    let image = match ask::decode_image(&body.image, &server.limits) {
        Ok(i) => i,
        Err(why) => {
            let status = if why.contains("at most") { 413 } else { 400 };
            return http::error(stream, status, allow, &why);
        }
    };
    // Re-listed, not trusted from the card: a session that ended or is not top-level is refused.
    let rows = match listing(server, cache) {
        Ok(r) => r,
        Err(why) => return http::error(stream, 500, allow, &why),
    };
    let Some(row) = rows.into_iter().find(|r| r.id == body.session) else {
        return http::error(
            stream,
            404,
            allow,
            &format!(
                "`{}` is not a live top-level session of yours; reopen the picker",
                body.session
            ),
        );
    };
    let id = match ask_id() {
        Ok(id) => id,
        Err(why) => return http::error(stream, 500, allow, &why),
    };
    let attachments = match image {
        Some((mime, bytes)) => match ask::store_image(&server.inbox, &id, &mime, &bytes) {
            Ok(a) => vec![a],
            Err(why) => return http::error(stream, 500, allow, &why),
        },
        None => Vec::new(),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let head = match crate::head::Head::attach_prompting(
        &row.daemon(&server.store),
        &row.id,
        IDENTITY,
        tx,
    ) {
        Ok(h) => h,
        Err(why) => return http::error(stream, 409, allow, &why),
    };
    let running = head.snapshot().turn == crate::head::Turn::Running;
    // Asked about a page, the session may look at it until the turn that took the question ends.
    let _look = body
        .tab
        .map(|t| Look::open(&server.tabs, &row.id, &id, t, &body));
    let text = ask::prompt(&id, &body, &server.limits);
    if let Err(why) = head.prompt_attached(&text, attachments) {
        return http::error(stream, 409, allow, &why);
    }
    if http::sse_head(stream, &server.origin).is_err() {
        return;
    }
    let _ = http::event(
        stream,
        "accepted",
        &json!({ "ask": id, "session": row.id, "name": row.name, "project": row.project }),
    );
    if running {
        let _ = http::event(
            stream,
            "status",
            &json!({ "text": "a turn is running; the question waits for the session to take it" }),
        );
    }
    let closed = || head.snapshot().closed;
    let marker = ask::marker(&id);
    let end = ask::stream(
        stream,
        &rx,
        &marker,
        closed,
        Instant::now() + server.patience,
    );
    let _ = match end {
        ask::End::Done(how) => http::event(stream, "done", &json!({ "how": how })),
        ask::End::Failed(why) => http::event(stream, "error", &json!({ "error": why })),
        ask::End::Timeout => http::event(
            stream,
            "error",
            &json!({ "error": format!(
                "no turn end within {}s; the session still has the question - read it in letibot",
                server.patience.as_secs()
            ) }),
        ),
        ask::End::ClientGone => Ok(()),
    };
}

/// A look grant held for one ask; dropped on every way out of the route.
struct Look<'a> {
    tabs: &'a tab::Tabs,
    session: String,
    ask: String,
}

impl<'a> Look<'a> {
    fn open(tabs: &'a tab::Tabs, session: &str, ask: &str, tab: i64, body: &ask::AskBody) -> Self {
        tabs.look(session, ask, tab, &body.url, &body.title);
        Look {
            tabs,
            session: session.to_string(),
            ask: ask.to_string(),
        }
    }
}

impl Drop for Look<'_> {
    fn drop(&mut self) {
        self.tabs.unlook(&self.session, &self.ask);
    }
}

/// `letibot browser`'s body once the CLI has read the config: bind, print, serve.
pub fn run(port: u16, extension_id: &str, token_path: &Path) -> Result<(), String> {
    let (token, created) = token(token_path)?;
    let inbox = ask::default_inbox().ok_or("no $HOME: the picture inbox has no default")?;
    ask::sweep(&inbox);
    let store = match std::env::var_os("LETIBOT_STORE") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("no $HOME")?)
            .join(".local/share/letibot/sessions.db"),
    };
    let listener = bind(SocketAddr::from(([127, 0, 0, 1], port)))?;
    let server = Arc::new(Server {
        token: token.clone(),
        origin: format!("chrome-extension://{extension_id}"),
        port,
        store,
        rundir: crate::daemon::rundir(),
        inbox,
        limits: LIMITS,
        patience: crate::tools::DEFAULT_TIMEOUT,
        tabs: Arc::default(),
    });
    let sock = letibot_tools::builtins::screen::browser_socket();
    let unix = tab::bind(&sock)?;
    let tabs = server.tabs.clone();
    std::thread::Builder::new()
        .name("browser-sock".into())
        .spawn(move || tab::serve_socket(unix, tabs))
        .map_err(|e| format!("the socket thread: {e}"))?;
    eprintln!(
        "letibot browser: listening on 127.0.0.1:{port} for {}, sessions on {}",
        server.origin,
        sock.display()
    );
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/chrome");
    let ext = ext.canonicalize().unwrap_or(ext);
    let status = if created {
        format!(
            "a new token, shown this once:\n\n    {token}\n\n  kept in {}, mode 0600",
            token_path.display()
        )
    } else {
        format!("you have a token already, in {}", token_path.display())
    };
    eprintln!(
        "letibot browser: {status}\n\n\
         Setup, once:\n\
         \x20 1. chrome://extensions, turn on Developer mode\n\
         \x20 2. Load unpacked: {}\n\
         \x20 3. the extension's options: paste the token, Save. The popup picks the session.\n\
         More: docs/browser.md\n",
        ext.display()
    );
    serve(listener, server);
    Ok(())
}
