//! The labelling proxy: one socket per owner, and every request through it checked (`docs/lifecycle.md`
//! d.7, placement 3).
//!
//! The engine has no way to label an object by default (a label is fixed for the life of an object
//! and neither `dockerd` nor the CLI has a default-label switch), so the only place a label can be
//! put on reliably is the API - which means the only path to the socket has to be ours. A `PATH`
//! shim is not enough: `/usr/bin/docker`, a compose plugin, an SDK and `curl --unix-socket` all
//! bypass one, and all four arrive here instead.
//!
//! What the proxy is not: a security boundary against the engine itself. An object it refuses to
//! touch can still be reached by a seat that was granted the real engine socket by hand, which is
//! why the grant letibot writes is this socket and not that one.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::http::{Request, Response};
use super::{API_VERSION, LABEL_OWNER, Policy, Route};

/// Where the proxy sends a request it decided to forward.
pub trait Upstream: Send + Sync {
    fn call(&self, req: &Request) -> Result<Response, String>;
}

impl<F> Upstream for F
where
    F: Fn(&Request) -> Result<Response, String> + Send + Sync,
{
    fn call(&self, req: &Request) -> Result<Response, String> {
        (self)(req)
    }
}

/// The engine, on its socket.
pub struct Socket {
    pub path: PathBuf,
    pub bound: std::time::Duration,
}

impl Upstream for Socket {
    fn call(&self, req: &Request) -> Result<Response, String> {
        super::http::call(&self.path, req, self.bound)
    }
}

/// One owner's proxy: its policy, its route, and the engine behind it.
pub struct Proxy {
    pub policy: Policy,
    pub route: Route,
    pub upstream: Arc<dyn Upstream>,
}

impl Proxy {
    pub fn new(policy: Policy, route: Route, upstream: Arc<dyn Upstream>) -> Proxy {
        Proxy {
            policy,
            route,
            upstream,
        }
    }

    /// The proxy for an engine socket on this box, as a granted seat gets it.
    pub fn granted(policy: Policy) -> Proxy {
        let upstream = match &policy.engine {
            Some(path) => Arc::new(Socket {
                path: path.clone(),
                bound: std::time::Duration::from_secs(120),
            }) as Arc<dyn Upstream>,
            // No engine: every forwarded request answers with that sentence rather than a connect
            // error nobody can act on.
            None => Arc::new(|_: &Request| {
                Err(
                    "this box has no docker engine socket: set `[docker] engine` in \
                     ~/.config/letibot/leticode.toml, or $DOCKER_HOST"
                        .to_string(),
                )
            }) as Arc<dyn Upstream>,
        };
        Proxy::new(policy, Route::Forward, upstream)
    }

    /// A socket that refuses everything in [`super::WITHHELD`]'s words: what a seat without the
    /// grant gets, so its `docker` says why rather than `no such file or directory`.
    pub fn withheld(policy: Policy) -> Proxy {
        let why = super::WITHHELD.to_string();
        Proxy::new(
            policy,
            Route::Refuse(why),
            Arc::new(|_: &Request| Err("withheld".to_string())),
        )
    }

    /// One request, decided. This is the whole proxy: [`serve`] only moves bytes to it.
    pub fn handle(&self, req: &Request) -> Response {
        if let Route::Refuse(why) = &self.route {
            return Response::error(403, why);
        }
        if upgraded(req) {
            return Response::error(
                400,
                "refused by letibot: an upgraded connection (`docker exec -it`, `docker attach`) \
                 is not proxied - after the 101 there is no HTTP left to read, so the proxy could \
                 not say whose object a write belonged to. Use `docker exec` without a tty.",
            );
        }
        let rewritten = match self.rewrite(req) {
            Ok(r) => r,
            Err(refusal) => return refusal,
        };
        if let Some(refusal) = self.own_check(&rewritten) {
            return refusal;
        }
        match self.upstream.call(&rewritten) {
            Ok(mut res) => {
                self.filter(&rewritten, &mut res);
                res
            }
            Err(why) => Response::error(
                502,
                &format!("letibot's docker proxy could not reach the engine: {why}"),
            ),
        }
    }

    /// The request as the engine must see it: labels on every create, a name this owner can be
    /// found by, a memory bound, a cgroup parent, and a prune that cannot reach past us.
    fn rewrite(&self, req: &Request) -> Result<Request, Response> {
        let (prefix, rest) = split_version(req.path());
        let mut out = req.clone();
        match (req.method.as_str(), rest) {
            ("POST", "/containers/create") => {
                let mut body = match req.json_body() {
                    Some(v) if v.is_object() => v,
                    _ => return Ok(out),
                };
                self.labelled_create(&mut body)?;
                out.set_body(&body);
                let name = param(req.query(), "name");
                if let Some(name) = name {
                    let want = self.policy.prefixed(&name);
                    out.target = format!(
                        "{prefix}/containers/create?{}",
                        set_param(req.query(), "name", &want)
                    );
                }
                Ok(out)
            }
            ("POST", "/networks/create") => {
                let mut body = match req.json_body() {
                    Some(v) if v.is_object() => v,
                    _ => return Ok(out),
                };
                merge_labels(&mut body, &self.policy.labels());
                if let Some(name) = body.get("Name").and_then(|n| n.as_str()) {
                    let want = self.policy.prefixed(name);
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("Name".into(), serde_json::Value::String(want));
                    }
                }
                out.set_body(&body);
                Ok(out)
            }
            ("POST", "/volumes/create") => {
                let mut body = match req.json_body() {
                    Some(v) if v.is_object() => v,
                    _ => return Ok(out),
                };
                // A volume's name is the client's (`db_data`, `bench_pgdata`): prefixing it would
                // rename what a compose file points at, and the label is what proves ownership.
                merge_labels(&mut body, &self.policy.labels());
                out.set_body(&body);
                Ok(out)
            }
            ("POST", "/build") => {
                // A build's labels travel in the query, not the body: `labels={"k":"v"}`.
                let mut labels: BTreeMap<String, String> = param(req.query(), "labels")
                    .and_then(|l| serde_json::from_str(&l).ok())
                    .unwrap_or_default();
                for (k, v) in self.policy.labels() {
                    labels.insert(k, v);
                }
                let encoded = serde_json::to_string(&labels).unwrap_or_default();
                out.target = format!(
                    "{prefix}/build?{}",
                    set_param(req.query(), "labels", &encoded)
                );
                Ok(out)
            }
            ("POST", "/containers/prune")
            | ("POST", "/networks/prune")
            | ("POST", "/volumes/prune")
            | ("POST", "/images/prune") => {
                // `docker system prune` is the command that eats other people's work. Through this
                // socket it can only eat this owner's, whatever filters the client asked for.
                let filters = with_label_filter(param(req.query(), "filters"), &self.policy.owner);
                out.target = format!(
                    "{prefix}{rest}?{}",
                    set_param(req.query(), "filters", &filters)
                );
                Ok(out)
            }
            _ => Ok(out),
        }
    }

    /// Labels, and the two refusals a container create needs, on one JSON body.
    fn labelled_create(&self, body: &mut serde_json::Value) -> Result<(), Response> {
        merge_labels(body, &self.policy.labels());
        let mut host = body
            .get("HostConfig")
            .and_then(|h| h.as_object())
            // A `HostConfig` the client left out is made on demand: the memory bound goes in
            // either way.
            .cloned()
            .unwrap_or_default();
        if host.get("Privileged").and_then(|p| p.as_bool()) == Some(true) {
            return Err(Response::error(
                403,
                "refused by letibot: a privileged container is the whole host, which is not a \
                 thing a seat can be granted. Drop `--privileged`: what a build or a test needs \
                 from it is usually a device, a mount or a capability, and those are narrow.",
            ));
        }
        let root = self.policy.root.display().to_string();
        if let Some(binds) = host.get("Binds").and_then(|b| b.as_array()) {
            for bind in binds.iter().filter_map(|b| b.as_str()) {
                if let Some(source) = bind_source(bind)
                    && !Under::of(&source).inside(&self.policy.root)
                {
                    return Err(Response::error(
                        403,
                        &format!(
                            "refused by letibot: the bind `{bind}` points at {source}, outside this \
                             owner's root ({root}). A seat's docker may mount its own furniture - \
                             its worktree, its target, its /tmp - and nothing else."
                        ),
                    ));
                }
            }
        }
        if let Some(mounts) = host.get("Mounts").and_then(|m| m.as_array()) {
            for mount in mounts {
                if mount.get("Type").and_then(|t| t.as_str()) != Some("bind") {
                    continue;
                }
                let Some(source) = mount.get("Source").and_then(|s| s.as_str()) else {
                    continue;
                };
                if !Under::of(source).inside(&self.policy.root) {
                    return Err(Response::error(
                        403,
                        &format!(
                            "refused by letibot: the mount source {source} is outside this owner's \
                             root ({root})"
                        ),
                    ));
                }
            }
        }
        let none = host.get("Memory").and_then(|m| m.as_u64()).unwrap_or(0) == 0;
        if none && let Some(memory) = self.policy.memory {
            host.insert("Memory".into(), serde_json::Value::from(memory));
        }
        if let Some(parent) = &self.policy.cgroup_parent
            && host
                .get("CgroupParent")
                .and_then(|c| c.as_str())
                .is_none_or(|c| c.is_empty())
        {
            host.insert(
                "CgroupParent".into(),
                serde_json::Value::String(parent.clone()),
            );
        }
        if let Some(obj) = body.as_object_mut() {
            obj.insert("HostConfig".into(), serde_json::Value::Object(host));
        }
        Ok(())
    }

    /// A request that names an object is only answered when the object is this owner's. Images are
    /// not checked: an image is a shared cache and the design never owner-reaps one.
    fn own_check(&self, req: &Request) -> Option<Response> {
        let (_, rest) = split_version(req.path());
        let object = Object::named(rest)?;
        let labels = match self.labels_of(&object) {
            Ok(labels) => labels,
            // Not there: the engine's own 404 is a better answer than ours, and the request that
            // follows gets it.
            Err(Why::Missing) => return None,
            Err(Why::Unreachable(why)) => {
                return Some(Response::error(
                    502,
                    &format!("letibot's docker proxy could not read the object's labels: {why}"),
                ));
            }
        };
        if labels.get(LABEL_OWNER).map(String::as_str) == Some(self.policy.owner.as_str()) {
            return None;
        }
        Some(Response::error(
            403,
            &format!(
                "refused by letibot: {} `{}` carries no `{LABEL_OWNER}={}`, so it is not this \
                 seat's. Only the owner's own objects can be listed, inspected or removed through \
                 this socket",
                object.word(),
                object.handle,
                self.policy.owner
            ),
        ))
    }

    fn labels_of(&self, object: &Object) -> Result<BTreeMap<String, String>, Why> {
        let path = match object.word() {
            "container" => format!("/v{API_VERSION}/containers/{}/json", object.handle),
            "network" => format!("/v{API_VERSION}/networks/{}", object.handle),
            _ => format!("/v{API_VERSION}/volumes/{}", object.handle),
        };
        let res = self
            .upstream
            .call(&Request::get(&path))
            .map_err(Why::Unreachable)?;
        if res.status == 404 {
            return Err(Why::Missing);
        }
        if res.status != 200 {
            return Err(Why::Unreachable(format!(
                "the engine answered {} for {path}",
                res.status
            )));
        }
        let body = res.json_body().unwrap_or(serde_json::Value::Null);
        let labels = match object.word() {
            "container" => body.pointer("/Config/Labels"),
            _ => body.get("Labels"),
        };
        Ok(labels
            .and_then(|l| l.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// An answer a seat may see: its own objects, never a neighbour's.
    fn filter(&self, req: &Request, res: &mut Response) {
        let (_, rest) = split_version(req.path());
        let mine = |labels: Option<&serde_json::Value>| -> bool {
            labels
                .and_then(|l| l.get(LABEL_OWNER))
                .and_then(|v| v.as_str())
                == Some(self.policy.owner.as_str())
        };
        let Some(mut body) = res.json_body() else {
            return;
        };
        let changed = match (req.method.as_str(), rest) {
            ("GET", "/containers/json") => keep_array(&mut body, |e| mine(e.get("Labels"))),
            ("GET", "/networks") => match body.get_mut("Networks") {
                Some(n) => keep_array(n, |e| mine(e.get("Labels"))),
                None => false,
            },
            ("GET", "/volumes") => match body.get_mut("Volumes") {
                Some(v) => keep_array(v, |e| mine(e.get("Labels"))),
                None => false,
            },
            _ => false,
        };
        if changed {
            res.body = serde_json::to_vec(&body).unwrap_or_default();
        }
    }
}

/// Why an object's labels could not be read. Two answers, because the caller does something
/// different with each: a missing object gets the engine's own 404, an unreachable one gets ours.
enum Why {
    Missing,
    Unreachable(String),
}

/// A container, network or volume named by a request path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    object: Kind,
    pub handle: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Container,
    Network,
    Volume,
}

impl Object {
    /// The object a version-stripped path names, if it names one.
    pub fn named(rest: &str) -> Option<Object> {
        let mut parts = rest.trim_start_matches('/').split('/');
        let kind = match parts.next()? {
            "containers" => Kind::Container,
            "networks" => Kind::Network,
            "volumes" => Kind::Volume,
            // `/containers/json` and `/containers/prune` are not objects.
            _ => return None,
        };
        let head = parts.next()?;
        if head.is_empty() || head == "json" || head == "prune" || head == "create" {
            return None;
        }
        Some(Object {
            object: kind,
            handle: head.to_string(),
        })
    }

    pub fn word(&self) -> &'static str {
        match self.object {
            Kind::Container => "container",
            Kind::Network => "network",
            Kind::Volume => "volume",
        }
    }
}

/// A serve loop on one socket, stopped when this value drops.
pub struct Serve {
    pub socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Serve {
    /// Bind [`Policy::socket`] and answer it with `proxy` until this value drops. The owner root is
    /// made first: the socket lives in it, and so does everything it may mount.
    pub fn start(proxy: Arc<Proxy>) -> std::io::Result<Serve> {
        let socket = proxy.policy.socket();
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A socket left by a daemon that died is not a socket anybody is answering.
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("docker-proxy".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let proxy = Arc::clone(&proxy);
                            // One thread per connection: a client that keeps a socket open must
                            // not hold the next `docker ps` behind it.
                            let _ = std::thread::Builder::new()
                                .name("docker-proxy-conn".into())
                                .spawn(move || {
                                    let _ = stream
                                        .set_read_timeout(Some(std::time::Duration::from_secs(60)));
                                    let mut reader = BufReader::new(&stream);
                                    let Some(req) = Request::read(&mut reader).ok().flatten()
                                    else {
                                        return;
                                    };
                                    let res = proxy.handle(&req);
                                    let mut write = &stream;
                                    let _ = res.write_to(&mut write);
                                });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
                    }
                }
            })?;
        Ok(Serve {
            socket,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// `/v1.44/containers/json` as `("/v1.44", "/containers/json")`. A client that sends no version
/// keeps sending none, and the proxy's own calls name [`API_VERSION`].
fn split_version(path: &str) -> (&str, &str) {
    if let Some(rest) = path.strip_prefix("/v")
        && let Some(i) = rest.find('/')
    {
        let version = &rest[..i];
        if !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit() || c == '.')
            && version.contains('.')
        {
            return (&path[..i + 2], &rest[i..]);
        }
    }
    ("", path)
}

/// A query parameter's value, undecoded: docker's own parameters are JSON or plain names.
pub(crate) fn param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if k == name {
            return Some(percent_decode(v));
        }
    }
    None
}

pub(crate) fn set_param(query: &str, name: &str, value: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut done = false;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        if pair.split_once('=').map(|(k, _)| k) == Some(name) {
            parts.push(format!("{name}={}", percent_encode(value)));
            done = true;
        } else {
            parts.push(pair.to_string());
        }
    }
    if !done {
        parts.push(format!("{name}={}", percent_encode(value)));
    }
    parts.join("&")
}

/// `filters` with this owner's label added. Docker's format is a JSON object of string arrays.
fn with_label_filter(filters: Option<String>, owner: &str) -> String {
    let mut map: BTreeMap<String, Vec<String>> = filters
        .and_then(|f| serde_json::from_str(&f).ok())
        .unwrap_or_default();
    let want = format!("{LABEL_OWNER}={owner}");
    let entry = map.entry("label".into()).or_default();
    if !entry.iter().any(|l| l == &want) {
        entry.push(want);
    }
    serde_json::to_string(&map).unwrap_or_default()
}

/// `Labels` on a body, with ours winning: a client that spelled `letibot.owner` itself cannot
/// choose somebody else's owner.
fn merge_labels(body: &mut serde_json::Value, labels: &BTreeMap<String, String>) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    let entry = obj
        .entry("Labels")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(map) = entry.as_object_mut() {
        for (k, v) in labels {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
    }
}

/// A bind's source, as `"src:dst[:mode]"`. A source without a `/` is a named volume, which is the
/// engine's own storage and never a path on this box.
fn bind_source(bind: &str) -> Option<String> {
    let source = bind.split(':').next()?;
    source.starts_with('/').then(|| source.to_string())
}

/// One path, canonicalised as far as it exists, compared against a root.
struct Under {
    path: PathBuf,
}

impl Under {
    fn of(path: &str) -> Under {
        // `canonicalize` fails on a path that does not exist yet, which is the normal case for a
        // bind's source - so the parent is canonicalised and the last component kept as written,
        // and a `..` in the front is resolved by the canonicalise of the part that exists.
        let p = PathBuf::from(path);
        let resolved = match p.canonicalize() {
            Ok(real) => real,
            Err(_) => match (p.parent(), p.file_name()) {
                (Some(parent), Some(name)) => parent
                    .canonicalize()
                    .map(|real| real.join(name))
                    .unwrap_or(p.clone()),
                _ => p.clone(),
            },
        };
        Under { path: resolved }
    }

    fn inside(&self, root: &std::path::Path) -> bool {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.path.starts_with(&root)
    }
}

/// Whether a request asks for the connection to be upgraded: after a 101 there is no HTTP to read.
fn upgraded(req: &Request) -> bool {
    req.header("Upgrade").is_some()
        || req
            .header("Connection")
            .is_some_and(|c| c.to_ascii_lowercase().contains("upgrade"))
}

/// Keep the elements of `value` that `keep` accepts, when `value` is an array.
fn keep_array(value: &mut serde_json::Value, keep: impl Fn(&serde_json::Value) -> bool) -> bool {
    let Some(items) = value.as_array_mut() else {
        return false;
    };
    let before = items.len();
    items.retain(|e| keep(e));
    items.len() != before
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Everything a query value may not carry raw. A build's `labels` is a JSON object, and an object
/// full of `{`/`"` in a URL is decoded by nobody.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
