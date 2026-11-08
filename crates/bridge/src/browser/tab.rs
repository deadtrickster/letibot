//! Driving a tab the operator allowed, or looking at one the session was asked about. A drive
//! grant is the extension's (a `chrome.debugger` attachment, whose bar Chrome keeps up) and it
//! tells this table about it. A look grant is this bridge's: an ask opens it on the tab it came
//! from and the end of the turn that took it closes it. A daemon's `screen tab` call arrives on
//! `<rundir>/browser.sock`, waits in a queue, and the extension's worker pulls it, runs it on that
//! tab and posts the result back.
//!
//! ```text
//!   screen tab ──unix socket──▶ Tabs ◀──POST /tab/next, /tab/result── extension worker ──CDP──▶ the tab
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// What a session may ask a tab to do. Nothing else reaches the extension.
pub const ACTS: &[&str] = &["shot", "read", "click", "type", "key", "navigate", "scroll"];

/// What a look grant allows: nothing that changes the page.
pub const LOOK_ACTS: &[&str] = &["shot", "read"];

/// The argument keys passed through to the extension; any other key is dropped here.
const ARG_KEYS: &[&str] = &["n", "selector", "x", "y", "text", "keys", "url", "dy"];

/// The sentence a session with no grant gets.
pub const NO_GRANT: &str =
    "no tab is allowed for this session - the operator allows one from the extension popup";

/// The sentence a look grant gets for any act that is not in [`LOOK_ACTS`].
pub const LOOK_ONLY: &str = "this tab is shared for looking only, because you were asked about \
     it; the operator allows driving from the popup";

/// A worker that has not polled for this long is gone (Chrome closed, the extension reloaded),
/// and its grants with it. It polls at least every [`POLL_WAIT`].
pub const WORKER_STALE: Duration = Duration::from_secs(60);

/// How long `/tab/next` holds a poll open before answering "nothing".
pub const POLL_WAIT: Duration = Duration::from_secs(20);

/// A command not taken by the worker inside this is answered as not taken.
pub const TAKE_WITHIN: Duration = Duration::from_secs(10);

/// A taken command with no result inside this is answered as lost. Navigation waits for the
/// load inside the extension for at most 15 s.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(40);

const LINE_MAX: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Opened by an ask, for `shot` and `read`, until the turn that took the question ends.
    Look,
    /// Allowed from the popup, for every act.
    Drive,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Look => "look",
            Kind::Drive => "drive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub tab: i64,
    pub url: String,
    pub title: String,
    pub kind: Kind,
}

impl Grant {
    pub fn to_json(&self) -> Value {
        json!({ "tab": self.tab, "url": self.url, "title": self.title, "kind": self.kind.as_str() })
    }
}

/// Where a command's outcome goes: the socket thread waiting on it.
type Reply = Sender<Result<Value, String>>;

struct Command {
    id: String,
    session: String,
    act: String,
    args: Value,
    reply: Reply,
}

#[derive(Default)]
struct Inner {
    /// Drive grants, as the extension last reported them.
    grants: BTreeMap<String, Grant>,
    /// Look grants by session, each with the ask that opened it.
    looks: BTreeMap<String, (Grant, String)>,
    queue: VecDeque<Command>,
    taken: BTreeMap<String, (String, Reply)>,
    seen: Option<Instant>,
    next: u64,
}

/// The grant table and the command queue, shared by the HTTP side and the socket side.
#[derive(Default)]
pub struct Tabs {
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// What a sync made of the extension's list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    pub kept: Vec<String>,
    /// Session ids refused, with why. The extension detaches those tabs.
    pub refused: Vec<(String, String)>,
}

impl Inner {
    /// The grant a session acts under: a drive grant supersedes a look grant.
    fn current(&self, session: &str) -> Option<&Grant> {
        self.grants
            .get(session)
            .or_else(|| self.looks.get(session).map(|(g, _)| g))
    }

    /// Fail the queued commands of sessions that hold no grant any more.
    fn prune(&mut self) {
        let (keep, gone): (VecDeque<Command>, VecDeque<Command>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|c| self.current(&c.session).is_some());
        self.queue = keep;
        for c in gone {
            let _ = c.reply.send(Err("the operator revoked this tab".into()));
        }
    }
}

impl Tabs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open a look grant for `session` on the tab an ask came from. A later ask replaces it.
    pub fn look(&self, session: &str, ask: &str, tab: i64, url: &str, title: &str) {
        let grant = Grant {
            tab,
            url: url.to_string(),
            title: title.to_string(),
            kind: Kind::Look,
        };
        self.lock()
            .looks
            .insert(session.to_string(), (grant, ask.to_string()));
        self.cv.notify_all();
    }

    /// Close the look grant `ask` opened; one a later ask opened stays.
    pub fn unlook(&self, session: &str, ask: &str) {
        let mut g = self.lock();
        if g.looks.get(session).is_some_and(|(_, a)| a == ask) {
            g.looks.remove(session);
            g.prune();
        }
    }

    /// Whether any grant is open, so the worker's poll is worth holding.
    pub fn any(&self) -> bool {
        let g = self.lock();
        !g.grants.is_empty() || !g.looks.is_empty()
    }

    /// Replace the table with the extension's own list, which is the truth. A session new to
    /// the table is checked with `live` first; one already in it is kept, so a daemon restart
    /// does not drop the operator's grant. Commands for a session that lost its grant fail.
    pub fn sync(
        &self,
        list: Vec<(String, Grant)>,
        live: impl Fn(&str) -> Result<(), String>,
    ) -> Synced {
        let known: Vec<String> = self.lock().grants.keys().cloned().collect();
        let mut out = Synced::default();
        let mut table = BTreeMap::new();
        for (session, mut grant) in list {
            grant.kind = Kind::Drive;
            if table.contains_key(&session) {
                out.refused.push((
                    session,
                    "one tab per session: this session has another tab".into(),
                ));
                continue;
            }
            if !known.contains(&session)
                && let Err(why) = live(&session)
            {
                out.refused.push((session, why));
                continue;
            }
            out.kept.push(session.clone());
            table.insert(session, grant);
        }
        let mut g = self.lock();
        g.seen = Some(Instant::now());
        g.grants = table;
        g.prune();
        drop(g);
        self.cv.notify_all();
        out
    }

    /// This session's grant, if the extension is still checking in.
    pub fn grant(&self, session: &str) -> Result<Grant, String> {
        let g = self.lock();
        let grant = g.current(session).cloned().ok_or(NO_GRANT)?;
        match g.seen {
            Some(at) if at.elapsed() < WORKER_STALE => Ok(grant),
            Some(at) => Err(format!(
                "the extension has not checked in for {}s - is Chrome open? The grant stands \
                 again when it does",
                at.elapsed().as_secs()
            )),
            None if grant.kind == Kind::Look => {
                Err("the extension has not checked in yet; try again in a moment".into())
            }
            None => Err(NO_GRANT.into()),
        }
    }

    /// The worker's long poll: the next command, or `None` after `wait`.
    pub fn next(&self, wait: Duration) -> Option<Value> {
        let deadline = Instant::now() + wait;
        let mut g = self.lock();
        loop {
            g.seen = Some(Instant::now());
            if let Some(c) = g.queue.pop_front() {
                // Checked again here: the drive grant may have gone since, leaving a look.
                let (tab, kind) = match g.current(&c.session) {
                    None => {
                        let _ = c.reply.send(Err("the operator revoked this tab".into()));
                        continue;
                    }
                    Some(t) if t.kind == Kind::Look && !LOOK_ACTS.contains(&c.act.as_str()) => {
                        let _ = c.reply.send(Err(LOOK_ONLY.into()));
                        continue;
                    }
                    Some(t) => (t.tab, t.kind),
                };
                let wire = json!({
                    "id": c.id, "session": c.session, "tab": tab, "kind": kind.as_str(),
                    "act": c.act, "args": c.args,
                });
                g.taken.insert(c.id.clone(), (c.session, c.reply));
                return Some(wire);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The worker's answer to a taken command. `false` for an id nobody waits on.
    pub fn finish(&self, id: &str, outcome: Result<Value, String>) -> bool {
        let mut g = self.lock();
        let Some((session, reply)) = g.taken.remove(id) else {
            return false;
        };
        let inner = &mut *g;
        let grant = match inner.grants.get_mut(&session) {
            Some(d) => Some(d),
            None => inner.looks.get_mut(&session).map(|(l, _)| l),
        };
        if let (Ok(v), Some(grant)) = (&outcome, grant) {
            if let Some(u) = v.get("url").and_then(Value::as_str) {
                grant.url = u.to_string();
            }
            if let Some(t) = v.get("title").and_then(Value::as_str) {
                grant.title = t.to_string();
            }
        }
        reply.send(outcome).is_ok()
    }

    /// Queue one command for `session`'s tab and wait for its result.
    pub fn submit(
        &self,
        session: &str,
        act: &str,
        args: &Value,
        take_within: Duration,
        answer_within: Duration,
    ) -> Result<Value, String> {
        check(act, args)?;
        if self.grant(session)?.kind == Kind::Look && !LOOK_ACTS.contains(&act) {
            return Err(LOOK_ONLY.into());
        }
        let (tx, rx): (_, Receiver<Result<Value, String>>) = channel();
        let id = {
            let mut g = self.lock();
            g.next += 1;
            let id = format!("c{}", g.next);
            g.queue.push_back(Command {
                id: id.clone(),
                session: session.to_string(),
                act: act.to_string(),
                args: pick(args),
                reply: tx,
            });
            id
        };
        self.cv.notify_all();
        if let Ok(r) = rx.recv_timeout(take_within) {
            return r;
        }
        {
            let mut g = self.lock();
            if let Some(at) = g.queue.iter().position(|c| c.id == id) {
                g.queue.remove(at);
                return Err(format!(
                    "the extension did not take the command within {}s - is Chrome open with \
                     the letibot extension?",
                    take_within.as_secs()
                ));
            }
        }
        match rx.recv_timeout(answer_within) {
            Ok(r) => r,
            Err(_) => {
                self.lock().taken.remove(&id);
                Err(format!(
                    "the extension took the command and sent no result within {}s",
                    answer_within.as_secs()
                ))
            }
        }
    }
}

/// The act and its arguments, checked before anything is queued.
pub fn check(act: &str, args: &Value) -> Result<(), String> {
    if !ACTS.contains(&act) {
        return Err(format!("`{act}` is not a tab act: {}", ACTS.join(", ")));
    }
    if act == "navigate" {
        let url = args.get("url").and_then(Value::as_str).unwrap_or("").trim();
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err("navigate takes an http:// or https:// `url`".into());
        }
    }
    Ok(())
}

fn pick(args: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ARG_KEYS {
        if let Some(v) = args.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// One stderr line per command, refused ones included. The url is the tab's after the act.
fn log(session: &str, url: &str, act: &str, outcome: &Result<Value, String>) {
    let said = match outcome {
        Ok(_) => "ok".to_string(),
        Err(why) => format!("error: {}", why.chars().take(200).collect::<String>()),
    };
    eprintln!("letibot browser: tab {session} {url} {act}: {said}");
}

/// Answer one socket request: `{session, act, args}` -> `{ok, grant, result}` or `{ok, error}`.
pub fn answer(tabs: &Tabs, req: &Value) -> Value {
    let session = req.get("session").and_then(Value::as_str).unwrap_or("");
    let act = req.get("act").and_then(Value::as_str).unwrap_or("");
    if session.is_empty() {
        return json!({ "ok": false, "error": "the request names no session" });
    }
    if act == "grants" {
        return match tabs.grant(session) {
            Ok(g) => json!({ "ok": true, "grant": g.to_json() }),
            Err(why) => json!({ "ok": false, "error": why }),
        };
    }
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
    let outcome = tabs.submit(session, act, &args, TAKE_WITHIN, ANSWER_WITHIN);
    let grant = tabs.grant(session).ok();
    let url = grant.as_ref().map(|g| g.url.as_str()).unwrap_or("-");
    log(session, url, act, &outcome);
    match outcome {
        Ok(result) => json!({
            "ok": true,
            "grant": grant.map(|g| g.to_json()),
            "result": result,
        }),
        Err(why) => json!({ "ok": false, "error": why }),
    }
}

/// Bind `<rundir>/browser.sock`, 0600. A socket file nobody answers on is a dead bridge's and
/// is replaced; one that answers belongs to a running bridge and is refused.
pub fn bind(path: &Path) -> Result<UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!(
                "{} is answered by another `letibot browser`",
                path.display()
            ));
        }
        let _ = std::fs::remove_file(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let l = UnixListener::bind(path).map_err(|e| format!("binding {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(l)
}

/// Accept daemons forever, one thread per connection, one JSON line in and one out per request.
pub fn serve_socket(listener: UnixListener, tabs: Arc<Tabs>) {
    for stream in listener.incoming().flatten() {
        let tabs = tabs.clone();
        let _ = std::thread::Builder::new()
            .name("browser-tab".into())
            .spawn(move || connection(stream, &tabs));
    }
}

fn connection(stream: UnixStream, tabs: &Tabs) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match (&mut reader).take(LINE_MAX).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            let _ = writeln!(
                out,
                "{}",
                json!({ "ok": false, "error": format!("a request is one line of at most {LINE_MAX} bytes") })
            );
            return;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => answer(tabs, &req),
            Err(e) => json!({ "ok": false, "error": format!("the request is not JSON: {e}") }),
        };
        if writeln!(out, "{reply}").is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(tab: i64) -> Grant {
        Grant {
            tab,
            url: "https://example.com/".into(),
            title: "t".into(),
            kind: Kind::Drive,
        }
    }

    #[test]
    fn a_session_without_a_grant_is_refused_and_nothing_is_queued() {
        let tabs = Tabs::default();
        let e = tabs
            .submit("s-a", "shot", &json!({}), TAKE_WITHIN, ANSWER_WITHIN)
            .unwrap_err();
        assert_eq!(e, NO_GRANT);
        assert!(tabs.next(Duration::ZERO).is_none());
    }

    #[test]
    fn only_the_fixed_acts_and_http_navigation_pass() {
        assert!(
            check("eval", &json!({}))
                .unwrap_err()
                .contains("not a tab act")
        );
        assert!(check("navigate", &json!({"url": "javascript:alert(1)"})).is_err());
        assert!(check("navigate", &json!({"url": "file:///etc/passwd"})).is_err());
        assert!(check("navigate", &json!({"url": "https://example.com"})).is_ok());
        let p = pick(&json!({"n": 3, "expression": "document.cookie"}));
        assert_eq!(p, json!({"n": 3}), "unknown keys never reach the extension");
    }

    #[test]
    fn a_command_goes_to_the_worker_and_its_result_comes_back() {
        let tabs = Arc::new(Tabs::default());
        let s = tabs.sync(vec![("s-a".into(), grant(7))], |_| Ok(()));
        assert_eq!(s.kept, vec!["s-a".to_string()]);
        let t = tabs.clone();
        let worker = std::thread::spawn(move || {
            let cmd = t.next(Duration::from_secs(5)).expect("a command");
            assert_eq!(cmd["tab"], 7);
            assert_eq!(cmd["act"], "click");
            assert_eq!(cmd["args"], json!({"n": 2}));
            let id = cmd["id"].as_str().unwrap().to_string();
            assert!(t.finish(&id, Ok(json!({"url": "https://example.com/next"}))));
        });
        let r = tabs
            .submit("s-a", "click", &json!({"n": 2}), TAKE_WITHIN, ANSWER_WITHIN)
            .expect("answered");
        worker.join().unwrap();
        assert_eq!(r["url"], "https://example.com/next");
        assert_eq!(tabs.grant("s-a").unwrap().url, "https://example.com/next");
        assert!(
            tabs.grant("s-b").is_err(),
            "another session sees no grant for this tab"
        );
    }

    #[test]
    fn a_revoke_fails_the_queued_command_and_an_untaken_one_times_out() {
        let tabs = Arc::new(Tabs::default());
        tabs.sync(vec![("s-a".into(), grant(1))], |_| Ok(()));
        let t = tabs.clone();
        let waiter = std::thread::spawn(move || {
            t.submit(
                "s-a",
                "shot",
                &json!({}),
                Duration::from_secs(5),
                ANSWER_WITHIN,
            )
        });
        std::thread::sleep(Duration::from_millis(200));
        tabs.sync(vec![], |_| Ok(()));
        assert!(waiter.join().unwrap().unwrap_err().contains("revoked"));

        tabs.sync(vec![("s-a".into(), grant(1))], |_| Ok(()));
        let e = tabs
            .submit(
                "s-a",
                "shot",
                &json!({}),
                Duration::from_millis(100),
                ANSWER_WITHIN,
            )
            .unwrap_err();
        assert!(e.contains("did not take"), "{e}");
        assert!(
            tabs.next(Duration::ZERO).is_none(),
            "the untaken one left the queue"
        );
    }

    #[test]
    fn a_look_grant_shows_and_reads_only_and_a_drive_grant_supersedes_it() {
        let tabs = Arc::new(Tabs::default());
        tabs.look("s-a", "ask-1", 9, "https://example.com/q", "Q");
        tabs.next(Duration::ZERO);
        let g = tabs.grant("s-a").unwrap();
        assert_eq!((g.tab, g.kind), (9, Kind::Look));
        assert_eq!(g.to_json()["kind"], "look");
        for act in ["click", "type", "key", "scroll"] {
            let e = tabs
                .submit("s-a", act, &json!({}), TAKE_WITHIN, ANSWER_WITHIN)
                .unwrap_err();
            assert_eq!(e, LOOK_ONLY, "{act}");
        }
        let e = tabs
            .submit(
                "s-a",
                "navigate",
                &json!({"url": "https://example.com/"}),
                TAKE_WITHIN,
                ANSWER_WITHIN,
            )
            .unwrap_err();
        assert_eq!(e, LOOK_ONLY);
        assert!(tabs.next(Duration::ZERO).is_none(), "nothing was queued");

        let t = tabs.clone();
        let worker = std::thread::spawn(move || {
            let cmd = t.next(Duration::from_secs(5)).expect("a command");
            assert_eq!(
                (cmd["tab"].as_i64(), cmd["kind"].as_str()),
                (Some(9), Some("look"))
            );
            assert!(t.finish(cmd["id"].as_str().unwrap(), Ok(json!({"png": ""}))));
        });
        tabs.submit("s-a", "read", &json!({}), TAKE_WITHIN, ANSWER_WITHIN)
            .expect("a look reads");
        worker.join().unwrap();

        tabs.sync(vec![("s-a".into(), grant(4))], |_| Ok(()));
        let g = tabs.grant("s-a").unwrap();
        assert_eq!((g.tab, g.kind), (4, Kind::Drive), "drive supersedes");
        tabs.sync(vec![], |_| Ok(()));
        assert_eq!(
            tabs.grant("s-a").unwrap().kind,
            Kind::Look,
            "a revoked drive leaves the look, never the reverse"
        );

        tabs.unlook("s-a", "ask-0");
        assert!(
            tabs.grant("s-a").is_ok(),
            "another ask's end keeps this one"
        );
        tabs.unlook("s-a", "ask-1");
        assert_eq!(tabs.grant("s-a").unwrap_err(), NO_GRANT);
        assert!(!tabs.any());
    }

    #[test]
    fn a_new_session_must_be_live_and_holds_one_tab() {
        let tabs = Tabs::default();
        let s = tabs.sync(
            vec![
                ("s-a".into(), grant(1)),
                ("s-a".into(), grant(2)),
                ("s-gone".into(), grant(3)),
            ],
            |id| {
                if id == "s-a" {
                    Ok(())
                } else {
                    Err("not live".into())
                }
            },
        );
        assert_eq!(s.kept, vec!["s-a".to_string()]);
        assert_eq!(s.refused.len(), 2, "{s:?}");
        assert_eq!(tabs.grant("s-a").unwrap().tab, 1);
        // Known already: kept even if the listing cannot see it this moment.
        let s = tabs.sync(vec![("s-a".into(), grant(1))], |_| Err("down".into()));
        assert_eq!(s.kept, vec!["s-a".to_string()]);
    }
}
