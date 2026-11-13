//! What docker holds for one owner, and how it goes (`docs/lifecycle.md` d.5 and d.7).
//!
//! Discovery is a label filter: `docker ps -a --filter label=letibot.owner=<id>` is the whole
//! proof, because the label is the only ownership docker offers and the proxy is the only way a
//! seat could put an object on the engine without one. Containers and networks are `compute` and go
//! at close; a named volume is `work` - it may hold the only copy of something a person wants - so
//! it waits for the work grace and goes at settle, and its size comes from `/system/df`.
//!
//! An image is where this build stops. The design's own rule is "never per owner": an image is a
//! shared cache, a pulled one carries no label at all, and deleting one can break a container
//! another owner still runs. So nothing here removes an image per owner; `--docker-report` names
//! them, and the opt-in `[gc.docker]` policy is the only thing that ever prunes one.

use letibot_tokencore::store::HoldingClass;

use super::config::Settings;
use super::http::{Request, Response, call};
use super::proxy::set_param;
use super::{API_VERSION, COMPOSE_PROJECT, LABEL_OWNER};

/// A container: compute, and gone at close.
pub const CONTAINER: &str = "container";
/// A network: compute, and gone at close - after its containers, which is the engine's order.
pub const NETWORK: &str = "network";
/// A volume: work. It survives the close and goes at settle.
pub const VOLUME: &str = "volume";

/// How long a single engine call may take. A reaper that hangs on a wedged engine is the leak this
/// whole package exists to stop, so every call is bounded and a timeout is `kept`.
pub const BOUND: std::time::Duration = std::time::Duration::from_secs(60);

/// An engine, as everything in this module uses one: one request in, one answer out.
///
/// Two doors: [`Socket`], for a reaper or a plan that has no hands and goes straight at the socket
/// the box configured, and a [`crate::lifecycle::Hands`] wrapper for the pass, so a test can drive
/// the whole reap without an engine at all.
pub trait Engine: Send + Sync {
    fn call(&self, req: &Request) -> Result<Response, String>;
    /// The socket it answers from, for a line that has to say where a fact came from.
    fn name(&self) -> String;
}

/// The engine on its socket.
pub struct Socket {
    pub path: std::path::PathBuf,
    pub bound: std::time::Duration,
}

impl Socket {
    pub fn at(path: &std::path::Path) -> Socket {
        Socket {
            path: path.to_path_buf(),
            bound: BOUND,
        }
    }
}

impl Engine for Socket {
    fn call(&self, req: &Request) -> Result<Response, String> {
        call(&self.path, req, self.bound)
    }

    fn name(&self) -> String {
        self.path.display().to_string()
    }
}

/// One object the engine says this owner holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub kind: &'static str,
    pub handle: String,
    pub class: HoldingClass,
    pub bytes: Option<u64>,
    pub why: String,
}

/// What one removal answered: `gone` only when the object is absent afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    pub kind: &'static str,
    pub handle: String,
    pub gone: bool,
    pub said: String,
    pub bytes: u64,
}

impl Ended {
    pub fn word(&self) -> &'static str {
        if self.gone { "gone" } else { "kept" }
    }
}

/// The label filter every call carries: `{"label":["letibot.owner=<id>"]}`.
fn owner_filters(owner: &str) -> String {
    serde_json::json!({ "label": [format!("{LABEL_OWNER}={owner}")] }).to_string()
}

fn get(engine: &dyn Engine, path: &str) -> Result<serde_json::Value, String> {
    let res = engine.call(&Request::get(path))?;
    if res.status != 200 {
        return Err(said(&res));
    }
    res.json_body()
        .ok_or_else(|| format!("the engine answered {path} with no JSON"))
}

fn said(res: &Response) -> String {
    res.json_body()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(|m| m.to_string())
        })
        .unwrap_or_else(|| format!("the engine answered {}", res.status))
}

fn labels(value: &serde_json::Value) -> &serde_json::Value {
    value
        .get("Labels")
        .or_else(|| value.pointer("/Config/Labels"))
        .unwrap_or(&serde_json::Value::Null)
}

fn is_owners(labels: &serde_json::Value, owner: &str) -> bool {
    labels.get(LABEL_OWNER).and_then(|o| o.as_str()) == Some(owner)
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// What the engine says one owner holds, from the label alone.
pub fn discovered(engine: &dyn Engine, owner: &str) -> Result<Vec<Discovered>, String> {
    let filters = set_param("all=1", "filters", &owner_filters(owner));
    let containers = get(
        engine,
        &format!("/v{API_VERSION}/containers/json?{filters}"),
    )?;
    let volumes = get(
        engine,
        &format!(
            "/v{API_VERSION}/volumes?{}",
            set_param("", "filters", &owner_filters(owner))
        ),
    )?;
    let networks = get(
        engine,
        &format!(
            "/v{API_VERSION}/networks?{}",
            set_param("", "filters", &owner_filters(owner))
        ),
    )?;
    let sizes = volume_sizes(engine).unwrap_or_default();
    let mut out = Vec::new();
    for c in containers.as_array().into_iter().flatten() {
        let id = c.get("Id").and_then(|i| i.as_str()).unwrap_or_default();
        if id.is_empty() || !is_owners(labels(c), owner) {
            continue;
        }
        let names: Vec<&str> = c
            .get("Names")
            .and_then(|n| n.as_array())
            .map(|n| n.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let state = c.get("State").and_then(|s| s.as_str()).unwrap_or("?");
        out.push(Discovered {
            kind: CONTAINER,
            handle: short_id(id),
            class: HoldingClass::Compute,
            bytes: c.get("SizeRw").and_then(|s| s.as_u64()),
            why: format!(
                "{} ({state})",
                names
                    .first()
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| short_id(id))
            ),
        });
    }
    for n in networks
        .get("Networks")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
    {
        let id = n.get("Id").and_then(|i| i.as_str()).unwrap_or_default();
        if id.is_empty() || !is_owners(labels(n), owner) {
            continue;
        }
        out.push(Discovered {
            kind: NETWORK,
            handle: short_id(id),
            class: HoldingClass::Compute,
            bytes: None,
            why: format!(
                "network {}",
                n.get("Name").and_then(|v| v.as_str()).unwrap_or_default()
            ),
        });
    }
    for v in volumes
        .get("Volumes")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let name = v.get("Name").and_then(|n| n.as_str()).unwrap_or_default();
        if name.is_empty() || !is_owners(labels(v), owner) {
            continue;
        }
        out.push(Discovered {
            kind: VOLUME,
            handle: name.to_string(),
            class: HoldingClass::Work,
            bytes: sizes.get(name).copied(),
            why: format!("volume {name}"),
        });
    }
    Ok(out)
}

/// Volume name to size, from `/system/df`, which is the only endpoint that reports it.
fn volume_sizes(engine: &dyn Engine) -> Result<std::collections::BTreeMap<String, u64>, String> {
    let df = get(engine, &format!("/v{API_VERSION}/system/df"))?;
    let mut out = std::collections::BTreeMap::new();
    for v in df
        .get("Volumes")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(size)) = (
            v.get("Name").and_then(|n| n.as_str()),
            v.pointer("/UsageData/Size").and_then(|s| s.as_u64()),
        ) else {
            continue;
        };
        out.insert(name.to_string(), size);
    }
    Ok(out)
}

/// Remove one object by its handle, and believe it only when the engine's own answer says so.
pub fn remove(engine: &dyn Engine, kind: &str, handle: &str) -> Ended {
    let path = match kind {
        CONTAINER => format!("/v{API_VERSION}/containers/{handle}?force=1&v=1"),
        NETWORK => format!("/v{API_VERSION}/networks/{handle}"),
        _ => format!("/v{API_VERSION}/volumes/{handle}?force=1"),
    };
    match engine.call(&Request::new("DELETE", &path)) {
        Ok(res) if res.status < 300 => Ended {
            kind: match kind {
                CONTAINER => CONTAINER,
                NETWORK => NETWORK,
                _ => VOLUME,
            },
            handle: handle.to_string(),
            gone: true,
            said: match kind {
                CONTAINER | NETWORK => "removed by its label, at close".to_string(),
                _ => "removed by its label, at settle".to_string(),
            },
            bytes: 0,
        },
        Ok(res) => Ended {
            kind: match kind {
                CONTAINER => CONTAINER,
                NETWORK => NETWORK,
                _ => VOLUME,
            },
            handle: handle.to_string(),
            gone: false,
            said: said(&res),
            bytes: 0,
        },
        Err(why) => Ended {
            kind: match kind {
                CONTAINER => CONTAINER,
                NETWORK => NETWORK,
                _ => VOLUME,
            },
            handle: handle.to_string(),
            gone: false,
            said: why,
            bytes: 0,
        },
    }
}

/// Containers, then networks: `docker rm -f` and `network rm` for this owner's label only, each
/// judged by looking afterwards. `docker rm -f` takes the container's anonymous volumes with it,
/// which is why a *named* one survives (d.5, and `--rm`'s own rule).
pub fn end_compute(engine: &dyn Engine, owner: &str) -> Vec<Ended> {
    let mut out = Vec::new();
    let listed = discovered(engine, owner);
    let list = match &listed {
        Ok(list) => list,
        Err(why) => {
            return vec![Ended {
                kind: CONTAINER,
                handle: String::new(),
                gone: false,
                said: format!("nothing of its docker compute could be listed: {why}"),
                bytes: 0,
            }];
        }
    };
    // Containers first: a network cannot go while a container is attached to it.
    for order in [CONTAINER, NETWORK] {
        for found in list.iter().filter(|f| f.kind == order) {
            let mut ended = remove(engine, found.kind, &found.handle);
            ended.bytes = found.bytes.unwrap_or(0);
            out.push(ended);
        }
    }
    out
}

/// Volumes by label: `work`, so this is only ever called for an owner the work grace has passed.
pub fn end_work(engine: &dyn Engine, owner: &str) -> Vec<Ended> {
    match discovered(engine, owner) {
        Ok(list) => list
            .iter()
            .filter(|f| f.kind == VOLUME)
            .map(|f| {
                let mut ended = remove(engine, VOLUME, &f.handle);
                ended.bytes = f.bytes.unwrap_or(0);
                ended
            })
            .collect(),
        Err(why) => vec![Ended {
            kind: VOLUME,
            handle: String::new(),
            gone: false,
            said: format!("its volumes could not be listed: {why}"),
            bytes: 0,
        }],
    }
}

/// One row of the report: an object letibot does not own, with what a person needs to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: &'static str,
    pub handle: String,
    /// `com.docker.compose.project`, or `(no compose project)`.
    pub project: String,
    pub bytes: Option<u64>,
    /// How long since it was made, when the engine says.
    pub age_ms: Option<i64>,
}

/// What `letibot gc --docker-report` prints: other people's images and volumes, grouped, sized and
/// aged - and the exact commands the operator could run. It removes nothing, ever.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
    pub now_ms: i64,
    /// The engine this was read from, so an empty report says which box had nothing.
    pub engine: String,
}

impl Report {
    pub fn images(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.kind == "image").collect()
    }

    pub fn volumes(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.kind == "volume").collect()
    }

    /// Project to (count, bytes) for one kind, biggest first: the grouping the design asks for.
    pub fn grouped(&self, kind: &str) -> Vec<(String, usize, u64)> {
        let mut map: std::collections::BTreeMap<String, (usize, u64)> =
            std::collections::BTreeMap::new();
        for r in self.rows.iter().filter(|r| r.kind == kind) {
            let e = map.entry(r.project.clone()).or_default();
            e.0 += 1;
            e.1 += r.bytes.unwrap_or(0);
        }
        let mut out: Vec<(String, usize, u64)> =
            map.into_iter().map(|(k, (n, b))| (k, n, b)).collect();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "docker on this box ({}): objects letibot does not own. Nothing here is removed.\n",
            self.engine
        ));
        if self.rows.is_empty() {
            out.push_str("  (the engine holds nothing unowned)\n");
            return out;
        }
        for (kind, what) in [("image", "images"), ("volume", "volumes")] {
            let grouped = self.grouped(kind);
            if grouped.is_empty() {
                continue;
            }
            let total: u64 = grouped.iter().map(|g| g.2).sum();
            out.push_str(&format!(
                "\n{what} — {} unowned, {}, by compose project:\n",
                self.rows.iter().filter(|r| r.kind == kind).count(),
                crate::lifecycle::human_bytes(total)
            ));
            for (project, n, bytes) in grouped {
                let age = self
                    .rows
                    .iter()
                    .filter(|r| r.kind == kind && r.project == project)
                    .filter_map(|r| r.age_ms)
                    .min();
                out.push_str(&format!(
                    "  {project:<40} {n:>3}  {:>10}{}\n",
                    crate::lifecycle::human_bytes(bytes),
                    match age {
                        Some(ms) => format!(
                            "  newest {} ago",
                            crate::lifecycle::human_millis(ms.max(0) as u64)
                        ),
                        None => String::new(),
                    }
                ));
            }
        }
        out.push_str("\nthe commands, which nobody here runs:\n");
        for (project, _, _) in self.grouped("image") {
            out.push_str(&format!(
                "  docker image prune -a --force --filter 'label={COMPOSE_PROJECT}={project}'\n"
            ));
        }
        for (project, _, _) in self.grouped("volume") {
            out.push_str(&format!(
                "  docker volume prune -a --force --filter 'label={COMPOSE_PROJECT}={project}'\n"
            ));
        }
        out.push_str(
            "to have the timer do it: `[gc.docker] prune_label` and `prune_images_unused_for` in \
             leticode.toml\n",
        );
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "engine": self.engine,
            "now_ms": self.now_ms,
            "rows": self.rows.iter().map(|r| serde_json::json!({
                "kind": r.kind,
                "handle": r.handle,
                "project": r.project,
                "bytes": r.bytes,
                "age_ms": r.age_ms,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Other people's images and volumes: everything the engine holds that carries no `letibot.owner`.
pub fn report(engine: &dyn Engine, now_ms: i64) -> Result<Report, String> {
    let df = get(engine, &format!("/v{API_VERSION}/system/df"))?;
    let mut out = Report {
        rows: Vec::new(),
        now_ms,
        engine: engine.name(),
    };
    for image in df
        .get("Images")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
    {
        if labels(image)
            .get(LABEL_OWNER)
            .and_then(|o| o.as_str())
            .is_some()
        {
            continue;
        }
        let created = image
            .get("Created")
            .and_then(|c| c.as_u64())
            .map(|s| (s as i64) * 1000);
        let handle = image
            .get("RepoTags")
            .and_then(|t| t.as_array())
            .and_then(|t| t.first())
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .unwrap_or_else(|| {
                short_id(image.get("Id").and_then(|i| i.as_str()).unwrap_or_default())
            });
        out.rows.push(Row {
            kind: "image",
            handle,
            project: project_of(labels(image)),
            bytes: image.get("Size").and_then(|s| s.as_u64()),
            age_ms: created.map(|c| now_ms - c),
        });
    }
    for volume in df
        .get("Volumes")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if labels(volume)
            .get(LABEL_OWNER)
            .and_then(|o| o.as_str())
            .is_some()
        {
            continue;
        }
        out.rows.push(Row {
            kind: "volume",
            handle: volume
                .get("Name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            project: project_of(labels(volume)),
            bytes: volume.pointer("/UsageData/Size").and_then(|s| s.as_u64()),
            age_ms: volume
                .get("CreatedAt")
                .and_then(|c| c.as_str())
                .and_then(rfc3339_ms)
                .map(|c| now_ms - c),
        });
    }
    Ok(out)
}

/// The opt-in prune: only objects matching a configured label, only older than the configured span.
/// `Ok(rows)` is what it removed, in the engine's own words; no `[gc.docker]` prunes nothing.
pub fn prune(engine: &dyn Engine, settings: &Settings) -> Result<Vec<String>, String> {
    if !settings.prunes() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    // The values go into the API's `filters` JSON as they are: `label=` is the CLI's spelling and
    // an engine reads it as a label named "label". A pattern ending in `*` is a key prefix, which
    // docker's own matcher handles.
    let labels: Vec<String> = settings
        .prune_label
        .iter()
        .map(|l| l.trim_end_matches('*').trim_end_matches('=').to_string())
        .collect();
    if labels.is_empty() {
        return Ok(out);
    }
    // `until` is an image filter; the docs say so, and a volume prune that carries one is a filter
    // docker ignores. A named volume needs `all` - `docker volume prune` alone takes anonymous ones.
    let mut images = serde_json::json!({ "label": labels.clone() });
    if let Some(age) = settings.prune_images_unused_for_ms {
        // Docker's `until` takes a duration: hours is the finest unit it documents for one.
        let hours = (age / 3_600_000).max(1);
        images["until"] = serde_json::json!([format!("{hours}h")]);
    }
    let volumes = serde_json::json!({ "label": labels, "all": ["true"] });
    for (endpoint, what, filters, field) in [
        ("images/prune", "images", images, "ImagesDeleted"),
        ("volumes/prune", "volumes", volumes, "VolumesDeleted"),
    ] {
        let query = set_param("", "filters", &filters.to_string());
        let req = Request::new("POST", &format!("/v{API_VERSION}/{endpoint}?{query}"));
        let res = engine.call(&req)?;
        if res.status >= 300 {
            return Err(format!("pruning {what}: {}", said(&res)));
        }
        let body = res.json_body().unwrap_or(serde_json::Value::Null);
        let count = body.get(field).and_then(|i| i.as_array()).map(|i| i.len());
        out.push(format!("pruned {} {what}", count.unwrap_or(0)));
    }
    Ok(out)
}

/// A compose project from a label set, or `(no compose project)`.
fn project_of(labels: &serde_json::Value) -> String {
    labels
        .get(COMPOSE_PROJECT)
        .and_then(|p| p.as_str())
        .map(|p| p.to_string())
        .unwrap_or_else(|| "(no compose project)".to_string())
}

/// `2026-11-01T04:12:33.123456789Z` as unix ms. The engine writes RFC3339 with nanoseconds and an
/// all-zero stamp for "unknown", which is not a date anybody wants in a report.
fn rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s.starts_with("0001-") {
        return None;
    }
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let clock = rest
        .trim_end_matches('Z')
        .splitn(2, |c: char| c == '+' || c == '-')
        .next()?;
    let mut c = clock.split(':');
    let hh: i64 = c.next()?.parse().ok()?;
    let mm: i64 = c.next()?.parse().ok()?;
    let ss: f64 = c.next()?.parse().ok()?;
    let days = days_from_civil(y, m, day);
    Some((days * 86_400 + hh * 3_600 + mm * 60) * 1_000 + (ss * 1_000.0) as i64)
}

/// Days since the epoch for a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
