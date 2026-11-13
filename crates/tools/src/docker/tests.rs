//! The G6 bar, one test per "Done when" item of `docs/lifecycle.md`'s G6 section - against a stub
//! engine on a unix socket, because this box has no docker and a test may not reach a real one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::docker::config::span_ms;
use crate::docker::proxy::{Object, Proxy, Serve};
use crate::docker::prune;
use crate::docker::{
    API_VERSION, LABEL_BOX, LABEL_OWNER, Policy, Request, Settings, end_compute, end_work, report,
};
use crate::testing::docker::StubEngine;

const OWNER: &str = "s-1794555261791226969";

/// A fixture directory that goes when the test ends.
struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("lb-docker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        Fixture(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn policy(fx: &Fixture, engine: &Path) -> Policy {
    let root = fx.path().join("owner");
    std::fs::create_dir_all(&root).expect("owner root");
    Policy {
        owner: OWNER.to_string(),
        box_id: "lab2x1".into(),
        root,
        engine: Some(engine.to_path_buf()),
        memory: Some(4 * 1024 * 1024 * 1024),
        cgroup_parent: Some("letibot.slice/docker.scope".into()),
    }
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The stub engine as this module's reaper reaches it: `Socket`, the same door a plan uses.
fn engine(stub: &StubEngine) -> crate::docker::Socket {
    crate::docker::Socket::at(stub.socket())
}

/// A granted proxy, forwarding to `stub`, and the socket it serves. The caller keeps the fixture.
fn granted(fx: &Fixture, stub: &StubEngine) -> Arc<Proxy> {
    Arc::new(Proxy::granted(policy(fx, stub.socket())))
}

// ---------------------------------------------------------------- the done-when list

/// G6-1: `docker run -d`, `docker compose up -d` and a raw socket request each produce objects
/// carrying `letibot.owner=<id>`.
#[test]
fn a_docker_run_is_labelled_and_named_for_its_owner() {
    let fx = Fixture::new("run");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);

    let mut req = Request::json(
        "POST",
        &format!("/v{API_VERSION}/containers/create?name=web"),
        &serde_json::json!({ "Image": "alpine", "Labels": {} }),
    );
    // A bind inside the owner root is the seat's own furniture and is allowed.
    req.set_body(&serde_json::json!({
        "Image": "alpine",
        "Labels": {},
        "HostConfig": { "Binds": [format!("{}:/x", policy_for_root(&fx).display())] },
    }));
    let res = proxy.handle(&req);
    assert!(res.status < 300, "the create went through: {res:?}");

    let seen = stub
        .last("containers/create")
        .expect("the create reached the engine");
    let body = seen.json_body().expect("a JSON body");
    assert_eq!(body["Labels"][LABEL_OWNER], OWNER);
    assert_eq!(body["Labels"][LABEL_BOX], "lab2x1");
    assert_eq!(
        body["HostConfig"]["Memory"],
        4 * 1024 * 1024 * 1024u64,
        "a create that names no limit is given one"
    );
    assert_eq!(
        body["HostConfig"]["CgroupParent"],
        "letibot.slice/docker.scope"
    );
    assert!(
        seen.query().contains("name=lb-17945552-web"),
        "the name is prefixed once: {}",
        seen.query()
    );
}

/// The owner root the fixture's policy has, for a bind that must be allowed.
fn policy_for_root(fx: &Fixture) -> PathBuf {
    fx.path().join("owner")
}

/// G6-1, compose: a create that already carries compose's project label keeps it and gains ours.
#[test]
fn a_compose_create_keeps_its_project_label_and_gains_ours() {
    let fx = Fixture::new("compose");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);

    let body = serde_json::json!({
        "Image": "postgres",
        "Labels": { "com.docker.compose.project": "lb-17945552", "com.docker.compose.service": "db" },
    });
    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/containers/create?name=lb-17945552-db-1"),
        &body,
    ));
    assert!(res.status < 300, "{res:?}");
    let seen = stub.last("containers/create").expect("create");
    let body = seen.json_body().expect("json");
    assert_eq!(
        body["Labels"]["com.docker.compose.project"], "lb-17945552",
        "compose's own label is not ours to rewrite"
    );
    assert_eq!(body["Labels"][LABEL_OWNER], OWNER);
    assert!(
        seen.query().contains("name=lb-17945552-db-1")
            && !seen.query().contains("lb-17945552-lb-17945552"),
        "a name already carrying the prefix is not prefixed twice: {}",
        seen.query()
    );

    // A network and a volume the same compose run creates.
    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/networks/create"),
        &serde_json::json!({ "Name": "shop_default", "Labels": {} }),
    ));
    assert!(res.status < 300, "{res:?}");
    let net = stub.last("networks/create").expect("network create");
    assert_eq!(net.json_body().expect("json")["Labels"][LABEL_OWNER], OWNER);

    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/volumes/create"),
        &serde_json::json!({ "Name": "shop_data", "Labels": {} }),
    ));
    assert!(res.status < 300, "{res:?}");
    let vol = stub.last("volumes/create").expect("volume create");
    assert_eq!(vol.json_body().expect("json")["Labels"][LABEL_OWNER], OWNER);
}

/// G6-1, a raw `curl --unix-socket "$DOCKER_HOST"`: the same rules over the socket itself, which is
/// the path a PATH shim could never have covered.
#[test]
fn a_raw_request_on_the_socket_is_labelled_like_any_other() {
    let fx = Fixture::new("raw");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);
    let serve = Serve::start(Arc::clone(&proxy)).expect("the socket is served");
    let body = serde_json::json!({ "Name": "curl_net", "Labels": {} });
    let req = Request::json("POST", &format!("/v{API_VERSION}/networks/create"), &body);
    let res = crate::docker::call(&serve.socket, &req, std::time::Duration::from_secs(10))
        .expect("the socket answered");
    assert_eq!(res.status, 200, "{res:?}");
    let seen = stub
        .last("networks/create")
        .expect("the network reached the engine");
    let body = seen.json_body().expect("json");
    assert_eq!(body["Labels"][LABEL_OWNER], OWNER);
    assert_eq!(body["Name"], "lb-17945552-curl_net");
}

/// G6-1: closing the owner removes its containers and networks and leaves a container made outside
/// the proxy alone.
#[test]
fn closing_the_owner_removes_its_containers_and_networks_and_leaves_strangers() {
    let fx = Fixture::new("close");
    let stub = StubEngine::start(fx.path());
    let mine = stub.seed_container("ours", labels(&[(LABEL_OWNER, OWNER)]));
    let outsider =
        stub.seed_container("theirs", labels(&[("com.docker.compose.project", "bench")]));
    let my_net = stub.seed_network("ours", labels(&[(LABEL_OWNER, OWNER)]));
    let their_net = stub.seed_network("theirs", BTreeMap::new());
    stub.seed_volume("ours_data", labels(&[(LABEL_OWNER, OWNER)]), 4096);
    stub.seed_volume("theirs_data", BTreeMap::new(), 8192);

    let ended = end_compute(&engine(&stub), OWNER);
    println!(
        "REQUESTS {:?}",
        stub.requests()
            .iter()
            .map(|r| (r.method.clone(), r.target.clone()))
            .collect::<Vec<_>>()
    );
    println!("ENDED {ended:?} remaining {:?}", stub.remaining());
    assert!(
        ended.iter().all(|e| e.gone),
        "every act of the close worked: {ended:?}"
    );
    assert_eq!(
        stub.remaining(),
        (1, 1, 2),
        "one container and one network left"
    );
    let names = stub.container_names();
    assert_eq!(names, vec!["theirs".to_string()]);
    assert!(!names.iter().any(|n| n == "ours"));
    let kept: Vec<String> = stub.requests().iter().map(|r| r.target.clone()).collect();
    assert!(
        !kept
            .iter()
            .any(|t| t.contains(&outsider) && t.contains("DELETE")),
        "the stranger's id was never sent a DELETE"
    );
    assert!(
        !kept.iter().any(|t| t.contains(&their_net)),
        "the stranger's network was never touched"
    );
    // The engine's own record is what the test reads: the owner's two objects were asked for by
    // handle, and the stranger's were never named in a request at all.
    let prefix = |id: &String| id.chars().take(12).collect::<String>();
    assert!(
        kept.iter().any(|t| t.contains(&prefix(&mine))),
        "the owner's container was deleted by its handle: {kept:?}"
    );
    assert!(
        kept.iter().any(|t| t.contains(&prefix(&my_net))),
        "and its network: {kept:?}"
    );
}

/// G6-2: `docker ps` does not list a container made outside the proxy, and `docker rm` of it is
/// refused - by the label, which is the only proof docker has.
#[test]
fn ps_hides_a_stranger_and_rm_of_one_is_refused() {
    let fx = Fixture::new("ps");
    let stub = StubEngine::start(fx.path());
    let mine = stub.seed_container("ours", labels(&[(LABEL_OWNER, OWNER)]));
    let outsider =
        stub.seed_container("theirs", labels(&[("com.docker.compose.project", "bench")]));
    let proxy = granted(&fx, &stub);

    let res = proxy.handle(&Request::get(&format!(
        "/v{API_VERSION}/containers/json?all=1"
    )));
    assert_eq!(res.status, 200);
    let listed = res.json_body().expect("json");
    let ids: Vec<String> = listed
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|c| c.get("Id").and_then(|i| i.as_str()).map(|s| s.to_string()))
        .collect();
    assert_eq!(
        ids,
        vec![mine.clone()],
        "only the owner's own container is listed"
    );

    let res = proxy.handle(&Request::new(
        "DELETE",
        &format!("/v{API_VERSION}/containers/{outsider}?force=1"),
    ));
    assert_eq!(res.status, 403, "{res:?}");
    let said = res.json_body().expect("json")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(said.contains("carries no `letibot.owner="), "{said}");
    assert_eq!(
        stub.remaining().0,
        2,
        "the stranger is still there: the engine was never asked to remove it"
    );

    // The owner's own container is its own to remove.
    let res = proxy.handle(&Request::new(
        "DELETE",
        &format!("/v{API_VERSION}/containers/{mine}?force=1"),
    ));
    assert_eq!(res.status, 204, "{res:?}");
}

/// G6: a volume survives the close, is in the plan with its size, and goes at settle - which the
/// pass decides with `judge`, so this test proves the engine side of both halves.
#[test]
fn a_named_volume_survives_close_and_goes_at_settle() {
    let fx = Fixture::new("volume");
    let stub = StubEngine::start(fx.path());
    stub.seed_volume(
        "ours_data",
        labels(&[(LABEL_OWNER, OWNER), (LABEL_BOX, "lab2x1")]),
        6 * 1024 * 1024,
    );
    stub.seed_volume("theirs_data", BTreeMap::new(), 1024);

    // The close leaves it: `end_compute` is containers and networks only.
    let ended = end_compute(&engine(&stub), OWNER);
    assert!(ended.iter().all(|e| e.gone));
    assert_eq!(stub.remaining(), (0, 0, 2), "both volumes are still there");

    // Discovery names it with its size, which is what the plan prints.
    let found = crate::docker::discovered(&engine(&stub), OWNER).expect("the engine answers");
    let volume = found
        .iter()
        .find(|f| f.kind == "volume")
        .expect("the volume is discovered");
    assert_eq!(volume.handle, "ours_data");
    assert_eq!(volume.bytes, Some(6 * 1024 * 1024));
    assert_eq!(
        volume.class,
        letibot_tokencore::store::HoldingClass::Work,
        "a volume may hold the only copy of something: it is work, not a cache"
    );

    // And at settle it goes, by its label.
    let ended = end_work(&engine(&stub), OWNER);
    assert!(ended.iter().all(|e| e.gone), "{ended:?}");
    assert_eq!(stub.remaining(), (0, 0, 1));
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|r| r.method == "DELETE" && r.path().contains("volumes/"))
            .count(),
        1,
        "one DELETE, and only for the owner's volume"
    );
}

/// G6-3: in a seat without the grant, any client reads the sentence that names the grant and the
/// VM route - `docker ps` included, because the socket answers it rather than the seat being empty.
#[test]
fn a_seat_without_the_grant_reads_the_sentence_naming_the_grant_and_the_vm() {
    let fx = Fixture::new("withheld");
    let policy = Policy {
        owner: OWNER.to_string(),
        box_id: "lab2x1".into(),
        root: fx.path().join("owner"),
        engine: None,
        memory: None,
        cgroup_parent: None,
    };
    let serve = Serve::start(Arc::new(Proxy::withheld(policy.clone()))).expect("served");
    let res = crate::docker::call(
        &serve.socket,
        &Request::get(&format!("/v{API_VERSION}/containers/json")),
        std::time::Duration::from_secs(10),
    )
    .expect("the socket answered");
    assert_eq!(res.status, 403);
    let said = res.json_body().expect("json")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(said.contains("not granted"), "{said}");
    assert!(said.contains("docker = true"), "it names the grant: {said}");
    assert!(said.contains("vm: true"), "and the VM route: {said}");

    // The env a seat is given points at that socket either way, so its `docker` says this rather
    // than "no such file or directory".
    let env: BTreeMap<String, String> = policy.seat_env(false).into_iter().collect();
    assert_eq!(
        env.get("DOCKER_HOST").map(String::as_str),
        Some(format!("unix://{}", policy.socket().display()).as_str())
    );
    assert!(
        !env.contains_key("COMPOSE_PROJECT_NAME"),
        "nothing is granted"
    );
    let granted: BTreeMap<String, String> = policy.seat_env(true).into_iter().collect();
    assert_eq!(
        granted.get("COMPOSE_PROJECT_NAME").map(String::as_str),
        Some("lb-17945552")
    );
}

/// G6: a `--docker-report` names other people's images and volumes by compose project and age and
/// removes nothing; the opt-in policy prunes only what its label matches.
#[test]
fn the_docker_report_names_debris_and_removes_nothing() {
    let fx = Fixture::new("report");
    let stub = StubEngine::start(fx.path());
    let now = 1_760_000_000_000i64;
    stub.seed_image(
        "bench/api:1",
        labels(&[("com.docker.compose.project", "bench")]),
        3 * 1024 * 1024 * 1024,
        now / 1000 - 4 * 86_400,
    );
    stub.seed_image(
        "scratch:latest",
        BTreeMap::new(),
        1024 * 1024 * 1024,
        now / 1000 - 20 * 86_400,
    );
    stub.seed_image(
        "ours:built",
        labels(&[(LABEL_OWNER, OWNER)]),
        512 * 1024 * 1024,
        now / 1000,
    );
    stub.seed_volume(
        "bench_pgdata",
        labels(&[("com.docker.compose.project", "bench")]),
        2048,
    );
    stub.seed_volume("ours_data", labels(&[(LABEL_OWNER, OWNER)]), 4096);

    let out = report(&engine(&stub), now).expect("the engine answers");
    let rendered = out.render();
    assert!(rendered.contains("bench"), "{rendered}");
    assert!(rendered.contains("(no compose project)"), "{rendered}");
    assert!(
        !rendered.contains("ours:built") && !rendered.contains("ours_data"),
        "letibot's own objects are not the report's subject: {rendered}"
    );
    assert!(rendered.contains("newest 4d0h ago"), "{rendered}");
    assert!(
        rendered.contains(
            "docker image prune -a --force --filter 'label=com.docker.compose.project=bench'"
        ),
        "it prints the command and runs nothing: {rendered}"
    );
    // Nothing was removed, and no prune endpoint was called.
    assert_eq!(stub.remaining(), (0, 0, 2));
    assert!(
        !stub.requests().iter().any(|r| r.path().contains("prune")),
        "the report never prunes"
    );

    // Off by default: no `[gc.docker]`, no prune, whatever the engine holds.
    let off = Settings::default();
    assert!(
        prune(&engine(&stub), &off)
            .expect("nothing to do")
            .is_empty()
    );
    assert_eq!(stub.remaining(), (0, 0, 2));

    // And with the policy set, only objects matching the configured label go.
    let on = Settings {
        prune_label: vec!["com.docker.compose.project=bench".into()],
        prune_images_unused_for_ms: Some(span_ms("30d").expect("a span")),
        ..Settings::default()
    };
    let said = prune(&engine(&stub), &on).expect("the opt-in prune ran");
    assert_eq!(said.len(), 2, "{said:?}");
    let prune_reqs: Vec<String> = stub
        .requests()
        .iter()
        .filter(|r| r.path().contains("prune"))
        .map(|r| r.target.clone())
        .collect();
    assert_eq!(prune_reqs.len(), 2, "one image prune and one volume prune");
    assert!(
        prune_reqs
            .iter()
            .all(|t| t.contains("com.docker.compose.project%3Dbench")
                || t.contains("com.docker.compose.project=bench")),
        "both prunes carry the configured label: {prune_reqs:?}"
    );
    assert!(
        prune_reqs[0].contains("until"),
        "the image prune carries the age bound: {}",
        prune_reqs[0]
    );
    assert!(
        !prune_reqs[1].contains("until"),
        "and the volume prune does not - docker ignores `until` for volumes: {}",
        prune_reqs[1]
    );
    // `ours:built` has our label and is not matched by the configured one, so it survives.
    assert_eq!(stub.remaining().2, 1, "only the stranger's volume went");
}

/// The join the daemon needs: a policy made from an owner's places, and the env a seat is given so
/// its `docker` reaches that policy's socket and nothing else.
#[test]
fn a_policy_is_made_from_the_owner_and_the_boxes_settings() {
    let fx = Fixture::new("policy");
    let places = crate::lifecycle::Places {
        gc_root: fx.path().join("gc"),
        box_id: "lab2x1".into(),
        settings: crate::lifecycle::Settings::default(),
        data_dir: fx.path().join("data"),
        rundir: fx.path().join("run"),
        firecode_runs: None,
        firecode_cache: None,
        proc_root: fx.path().join("proc"),
        cgroup_fs: fx.path().join("cg"),
        docker_engine: None,
    };
    let policy = Policy::of(OWNER, &places);
    assert_eq!(policy.box_id, "lab2x1");
    assert_eq!(policy.root, fx.path().join("gc").join("lab2x1").join(OWNER));
    assert_eq!(policy.socket(), policy.root.join("docker.sock"));
    assert_eq!(policy.project(), "lb-17945552");
    let env: BTreeMap<String, String> = policy.seat_env(true).into_iter().collect();
    assert_eq!(
        env.get("DOCKER_HOST").map(String::as_str),
        Some(format!("unix://{}", policy.socket().display()).as_str())
    );
    assert_eq!(env.get("LETIBOT_OWNER").map(String::as_str), Some(OWNER));
    assert_eq!(
        env.get("COMPOSE_PROJECT_NAME").map(String::as_str),
        Some("lb-17945552")
    );
}

// ---------------------------------------------------------------- the two refusals a create has

#[test]
fn a_privileged_container_is_refused_and_an_outside_bind_is_refused() {
    let fx = Fixture::new("refusals");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);

    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/containers/create"),
        &serde_json::json!({
            "Image": "alpine",
            "HostConfig": { "Privileged": true },
        }),
    ));
    assert_eq!(res.status, 403);
    let said = res.json_body().expect("json")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(said.contains("privileged"), "{said}");
    assert!(
        stub.last("containers/create").is_none(),
        "a refused create never reaches the engine"
    );

    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/containers/create"),
        &serde_json::json!({
            "Image": "alpine",
            "HostConfig": { "Binds": ["/etc:/host-etc"] },
        }),
    ));
    assert_eq!(res.status, 403);
    let said = res.json_body().expect("json")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(said.contains("outside this owner's root"), "{said}");

    // A named volume is not a path on this box, so it is not a bind this proxy refuses.
    let res = proxy.handle(&Request::json(
        "POST",
        &format!("/v{API_VERSION}/containers/create"),
        &serde_json::json!({
            "Image": "alpine",
            "HostConfig": { "Binds": ["ours_data:/var/lib/pg"] },
        }),
    ));
    assert!(res.status < 300, "{res:?}");
}

// ---------------------------------------------------------------- the rest of the rules

#[test]
fn a_prune_through_the_proxy_can_only_reach_this_owner() {
    let fx = Fixture::new("prune");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);

    let res = proxy.handle(&Request::new(
        "POST",
        &format!("/v{API_VERSION}/volumes/prune"),
    ));
    assert!(res.status < 300, "{res:?}");
    let seen = stub
        .last("volumes/prune")
        .expect("the prune reached the engine");
    let filters = crate::docker::proxy::param(seen.query(), "filters").expect("filters were added");
    assert!(
        filters.contains(&format!("{LABEL_OWNER}={OWNER}")),
        "a prune can only reach the owner's own objects: {filters}"
    );
}

#[test]
fn a_build_gets_its_labels_in_the_query() {
    let fx = Fixture::new("build");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);
    let req = Request::new(
        "POST",
        &format!("/v{API_VERSION}/build?t=b:1&labels=%7B%22stage%22%3A%22ci%22%7D"),
    );
    let res = proxy.handle(&req);
    assert!(res.status < 300, "{res:?}");
    let seen = stub.last("build").expect("the build reached the engine");
    let labels: BTreeMap<String, String> =
        serde_json::from_str(&crate::docker::proxy::percent_decode(
            &crate::docker::proxy::param(seen.query(), "labels").expect("labels"),
        ))
        .expect("a JSON object");
    assert_eq!(
        labels.get("stage").map(String::as_str),
        Some("ci"),
        "the client's own label is kept"
    );
    assert_eq!(labels.get(LABEL_OWNER).map(String::as_str), Some(OWNER));
}

#[test]
fn an_upgraded_connection_is_refused_by_name() {
    let fx = Fixture::new("upgrade");
    let stub = StubEngine::start(fx.path());
    let proxy = granted(&fx, &stub);
    let mut req = Request::get(&format!("/v{API_VERSION}/containers/abc/attach"));
    req.set_header("Connection", "Upgrade");
    req.set_header("Upgrade", "tcp");
    let res = proxy.handle(&req);
    assert_eq!(res.status, 400);
    let said = res.json_body().expect("json")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        said.contains("exec -it"),
        "the refusal says what to do instead: {said}"
    );
}

#[test]
fn an_object_this_owner_cannot_see_is_its_own_fault_not_a_stranger_s() {
    // The object a path names is read from the path, not guessed: `/containers/json` and
    // `/containers/prune` are not objects, and a volume is named by its name, not its id.
    assert!(Object::named("/containers/json").is_none());
    assert!(Object::named("/containers/prune").is_none());
    assert!(Object::named("/containers/create").is_none());
    let c = Object::named("/containers/abc123/start").expect("a container");
    assert_eq!((c.word(), c.handle.as_str()), ("container", "abc123"));
    let v = Object::named("/volumes/ours_data").expect("a volume");
    assert_eq!((v.word(), v.handle.as_str()), ("volume", "ours_data"));
}

#[test]
fn the_api_version_the_proxy_targets_is_written_down() {
    // A fact the operator asked for in the brief, and a test instead of a comment: the proxy's own
    // calls name one version, and `--version` on the engine is not where it comes from.
    assert_eq!(API_VERSION, "1.44");
    let fx = Fixture::new("version");
    let stub = StubEngine::start(fx.path());
    let proxy = crate::docker::Proxy::granted(policy(&fx, stub.socket()));
    // A client with no version prefix is forwarded as it came.
    let res = proxy.handle(&Request::get("/_ping"));
    assert_eq!(res.status, 200);
    assert_eq!(
        stub.last("_ping").expect("reached").path(),
        "/_ping",
        "the client's own spelling of the path is kept"
    );
}
