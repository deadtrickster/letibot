//! A stub Docker engine on a unix socket: the fixture every docker test runs against, because the
//! guest has no engine and a test may never reach a real one.
//!
//! It records every request it is sent and answers in the Engine API's own shapes - the fields the
//! proxy and the reaper read (`Id`, `Labels`, `Names`, `State`, `Volumes`, `UsageData`, `Images`,
//! `ImagesDeleted`) - so a test asserts on what went over the wire and on what the engine would
//! have done with it, which is as far as a box without docker can honestly go.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::docker::{Request, Response};

/// One container the stub engine holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub state: String,
    pub size_rw: u64,
}

/// One network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

/// One volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub size: u64,
    pub created: String,
}

/// One image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub tag: String,
    pub labels: BTreeMap<String, String>,
    pub size: u64,
    pub created: i64,
}

/// What the stub engine holds, and everything it was asked.
#[derive(Debug, Default)]
pub struct State {
    pub requests: Vec<Request>,
    pub containers: Vec<Container>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
    pub images: Vec<Image>,
    /// The answer a create gets: the id the stub hands back.
    pub next_id: u64,
}

/// A stub engine, serving from a socket under a fixture directory.
pub struct StubEngine {
    socket: PathBuf,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StubEngine {
    /// Bind `<dir>/engine.sock` and answer it until this value drops.
    pub fn start(dir: &Path) -> StubEngine {
        std::fs::create_dir_all(dir).expect("fixture dir");
        let socket = dir.join("engine.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind the stub");
        listener.set_nonblocking(true).expect("nonblocking");
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let shared = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
                        let mut reader = BufReader::new(&stream);
                        let Some(req) = Request::read(&mut reader).ok().flatten() else {
                            continue;
                        };
                        let res = answer(&shared, &req);
                        let mut write = &stream;
                        let _ = res.write_to(&mut write);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                }
            }
        });
        StubEngine {
            socket,
            state,
            stop,
            thread: Some(thread),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Every request the engine was sent, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.state.lock().expect("stub state").requests.clone()
    }

    /// The last request whose path contains `needle`, for an assertion about one call.
    pub fn last(&self, needle: &str) -> Option<Request> {
        self.requests()
            .into_iter()
            .rev()
            .find(|r| r.path().contains(needle))
    }

    /// A container the seats did NOT make: nothing about it carries a letibot label.
    pub fn seed_container(&self, name: &str, labels: BTreeMap<String, String>) -> String {
        let mut state = self.state.lock().expect("stub state");
        state.next_id += 1;
        let id = new_id(state.next_id);
        state.containers.push(Container {
            id: id.clone(),
            name: name.to_string(),
            labels,
            state: "running".into(),
            size_rw: 4096,
        });
        id
    }

    pub fn seed_network(&self, name: &str, labels: BTreeMap<String, String>) -> String {
        let mut state = self.state.lock().expect("stub state");
        state.next_id += 1;
        let id = new_id(state.next_id);
        state.networks.push(Network {
            id: id.clone(),
            name: name.to_string(),
            labels,
        });
        id
    }

    pub fn seed_volume(&self, name: &str, labels: BTreeMap<String, String>, size: u64) -> String {
        let mut state = self.state.lock().expect("stub state");
        state.volumes.push(Volume {
            name: name.to_string(),
            labels,
            size,
            created: "2026-11-01T04:12:33.123456789Z".into(),
        });
        name.to_string()
    }

    pub fn seed_image(&self, tag: &str, labels: BTreeMap<String, String>, size: u64, created: i64) {
        let mut state = self.state.lock().expect("stub state");
        state.next_id += 1;
        let id = format!("sha256:{}", new_id(state.next_id));
        state.images.push(Image {
            id,
            tag: tag.to_string(),
            labels,
            size,
            created,
        });
    }

    /// What the engine still holds, by kind: the answer a reap test asserts on.
    pub fn remaining(&self) -> (usize, usize, usize) {
        let state = self.state.lock().expect("stub state");
        (
            state.containers.len(),
            state.networks.len(),
            state.volumes.len(),
        )
    }

    pub fn container_names(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("stub state")
            .containers
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }
}

impl Drop for StubEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn answer(state: &Arc<Mutex<State>>, req: &Request) -> Response {
    let path = req.path().to_string();
    let filters = filters_of(req);
    let mut state = state.lock().expect("stub state");
    state.requests.push(req.clone());
    let rest = strip_version(&path).to_string();
    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    match (req.method.as_str(), parts.as_slice()) {
        ("GET", ["_ping"]) => Response::new(200, b"OK".to_vec()),
        ("POST", ["containers", "create"]) => {
            state.next_id += 1;
            let id = new_id(state.next_id);
            let body = req.json_body().unwrap_or(serde_json::Value::Null);
            state.containers.push(Container {
                id: id.clone(),
                name: name_param(req).unwrap_or_else(|| id.clone()),
                labels: labels_of_json(body.get("Labels")),
                state: "created".into(),
                size_rw: 0,
            });
            Response::ok(serde_json::json!({ "Id": id, "Warnings": [] }))
        }
        ("GET", ["containers", "json"]) => {
            let list: Vec<serde_json::Value> = state
                .containers
                .iter()
                .filter(|c| matches(&c.labels, &filters))
                .map(|c| {
                    serde_json::json!({
                        "Id": c.id,
                        "Names": [format!("/{}", c.name)],
                        "State": c.state,
                        "Labels": c.labels,
                        "SizeRw": c.size_rw,
                    })
                })
                .collect();
            Response::ok(serde_json::Value::Array(list))
        }
        ("GET", ["containers", id, "json"]) => {
            match state.containers.iter().find(|c| by_id(&c.id, id)) {
                Some(c) => Response::ok(serde_json::json!({
                    "Id": c.id,
                    "Name": format!("/{}", c.name),
                    "Config": { "Labels": c.labels },
                })),
                None => missing("container", id),
            }
        }
        ("DELETE", ["containers", id]) => {
            let before = state.containers.len();
            state.containers.retain(|c| !by_id(&c.id, id));
            if state.containers.len() == before {
                missing("container", id)
            } else {
                Response::new(204, Vec::new())
            }
        }
        ("POST", ["containers", "prune"]) => {
            let before = state.containers.len();
            state.containers.retain(|c| !matches(&c.labels, &filters));
            Response::ok(serde_json::json!({
                "ContainersDeleted": vec![String::new(); before - state.containers.len()],
            }))
        }
        ("POST", ["networks", "create"]) => {
            state.next_id += 1;
            let id = new_id(state.next_id);
            let body = req.json_body().unwrap_or(serde_json::Value::Null);
            state.networks.push(Network {
                id: id.clone(),
                name: body
                    .get("Name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string(),
                labels: labels_of_json(body.get("Labels")),
            });
            Response::ok(serde_json::json!({ "Id": id, "Warning": "" }))
        }
        ("GET", ["networks"]) => {
            let list: Vec<serde_json::Value> = state
                .networks
                .iter()
                .filter(|n| matches(&n.labels, &filters))
                .map(|n| {
                    serde_json::json!({
                        "Id": n.id, "Name": n.name, "Labels": n.labels,
                    })
                })
                .collect();
            Response::ok(serde_json::json!({ "Networks": list }))
        }
        ("GET", ["networks", id]) => match state.networks.iter().find(|n| by_id(&n.id, id)) {
            Some(n) => Response::ok(serde_json::json!({
                "Id": n.id, "Name": n.name, "Labels": n.labels,
            })),
            None => missing("network", id),
        },
        ("DELETE", ["networks", id]) => {
            let before = state.networks.len();
            state.networks.retain(|n| !by_id(&n.id, id));
            if state.networks.len() == before {
                missing("network", id)
            } else {
                Response::new(204, Vec::new())
            }
        }
        ("POST", ["volumes", "create"]) => {
            let body = req.json_body().unwrap_or(serde_json::Value::Null);
            let name = body
                .get("Name")
                .and_then(|n| n.as_str())
                .unwrap_or("anonymous")
                .to_string();
            state.volumes.push(Volume {
                name: name.clone(),
                labels: labels_of_json(body.get("Labels")),
                size: 0,
                created: "2026-11-01T04:12:33.123456789Z".into(),
            });
            Response::ok(serde_json::json!({ "Name": name, "Driver": "local" }))
        }
        ("GET", ["volumes"]) => {
            let list: Vec<serde_json::Value> = state
                .volumes
                .iter()
                .filter(|v| matches(&v.labels, &filters))
                .map(|v| {
                    serde_json::json!({
                        "Name": v.name,
                        "Labels": v.labels,
                        "CreatedAt": v.created,
                        "UsageData": { "Size": v.size, "RefCount": 0 },
                    })
                })
                .collect();
            Response::ok(serde_json::json!({ "Volumes": list, "Warnings": [] }))
        }
        ("GET", ["volumes", name]) => match state.volumes.iter().find(|v| v.name == *name) {
            Some(v) => Response::ok(serde_json::json!({
                "Name": v.name, "Labels": v.labels,
            })),
            None => missing("volume", name),
        },
        ("DELETE", ["volumes", name]) => {
            let before = state.volumes.len();
            state.volumes.retain(|v| v.name != *name);
            if state.volumes.len() == before {
                missing("volume", name)
            } else {
                Response::new(204, Vec::new())
            }
        }
        ("POST", ["volumes", "prune"]) => {
            let before = state.volumes.len();
            state.volumes.retain(|v| !matches(&v.labels, &filters));
            Response::ok(serde_json::json!({
                "VolumesDeleted": vec![String::new(); before - state.volumes.len()],
                "SpaceReclaimed": 0,
            }))
        }
        ("POST", ["images", "prune"]) => {
            let before = state.images.len();
            state.images.retain(|i| !matches(&i.labels, &filters));
            Response::ok(serde_json::json!({
                "ImagesDeleted": vec![serde_json::Value::Null; before - state.images.len()],
                "SpaceReclaimed": 0,
            }))
        }
        ("GET", ["images", "json"]) => {
            let list: Vec<serde_json::Value> = state
                .images
                .iter()
                .map(|i| {
                    serde_json::json!({
                        "Id": i.id,
                        "RepoTags": [i.tag],
                        "Labels": i.labels,
                        "Size": i.size,
                        "Created": i.created,
                    })
                })
                .collect();
            Response::ok(serde_json::Value::Array(list))
        }
        ("GET", ["system", "df"]) => Response::ok(serde_json::json!({
            "LayersSize": 0,
            "Images": state.images.iter().map(|i| serde_json::json!({
                "Id": i.id,
                "RepoTags": [i.tag],
                "Labels": i.labels,
                "Size": i.size,
                "Created": i.created,
            })).collect::<Vec<_>>(),
            "Containers": [],
            "Volumes": state.volumes.iter().map(|v| serde_json::json!({
                "Name": v.name,
                "Labels": v.labels,
                "CreatedAt": v.created,
                "UsageData": { "Size": v.size, "RefCount": 0 },
            })).collect::<Vec<_>>(),
        })),
        ("POST", ["build"]) => Response::ok(serde_json::json!({ "stream": "ok" })),
        _ => Response::error(
            404,
            &format!("the stub engine has no route for {} {}", req.method, path),
        ),
    }
}

/// A 64-character id whose FIRST characters are what distinguish it: the reaper's handles are
/// twelve characters (`docker ps` shows that many) and a left-padded counter would make every id
/// start with the same twelve zeros.
fn new_id(n: u64) -> String {
    format!(
        "{:016x}{}",
        n.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        "0".repeat(48)
    )
}

/// Docker accepts an object's id or any unambiguous prefix of it, which is why the reaper's handles
/// are twelve characters (`docker ps` shows that many). The stub matches the same way.
fn by_id(full: &str, asked: &str) -> bool {
    full == asked || full.starts_with(asked)
}

fn missing(kind: &str, id: &str) -> Response {
    Response::error(404, &format!("No such {kind}: {id}"))
}

fn strip_version(path: &str) -> &str {
    if let Some(rest) = path.strip_prefix("/v")
        && let Some(i) = rest.find('/')
        && rest[..i].contains('.')
    {
        return &rest[i..];
    }
    path
}

fn name_param(req: &Request) -> Option<String> {
    let query = req.query();
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "name").then(|| v.to_string())
    })
}

fn labels_of_json(value: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    value
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The filter values a request asks for, flattened: `label=letibot.owner=x` becomes
/// `("label", "letibot.owner=x")`.
fn filters_of(req: &Request) -> Vec<(String, String)> {
    let mut raw = String::new();
    for pair in req.query().split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == "filters"
        {
            raw = percent_decode(v);
            break;
        }
    }
    if raw.is_empty() {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in json {
        let values: Vec<String> = match value {
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            serde_json::Value::Object(o) => o.keys().filter(|_| key == "label").cloned().collect(),
            serde_json::Value::String(s) => vec![s],
            _ => Vec::new(),
        };
        for v in values {
            out.push((key.clone(), v));
        }
    }
    out
}

/// Whether a label set satisfies the filters a request carried.
fn matches(labels: &BTreeMap<String, String>, filters: &[(String, String)]) -> bool {
    filters.iter().all(|(key, want)| match key.as_str() {
        "label" => {
            let (k, v) = match want.split_once('=') {
                Some(kv) => kv,
                // A bare key: "has this label".
                None => return labels.contains_key(want),
            };
            let have = labels.get(k).map(String::as_str).unwrap_or_default();
            if let Some(prefix) = v.strip_suffix('*') {
                have.starts_with(prefix)
            } else {
                have == v
            }
        }
        // Every other filter is not this stub's business: a test that needs one asserts on the
        // request itself, which every call records.
        _ => true,
    })
}

fn percent_decode(s: &str) -> String {
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
