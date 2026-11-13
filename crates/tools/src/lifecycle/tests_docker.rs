//! G6's done-whens as the PASS sees them: a close takes the owner's containers and networks by
//! label and leaves a stranger's alone, a named volume survives the close and is in the plan with
//! its size, and it goes at settle. The engine is a stub on a unix socket (`testing::docker`):
//! this box has no docker, and a test may never reach a real engine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use letibot_tokencore::store::{OwnerKind, OwnerRow, OwnerState, Store};

use crate::testing::docker::StubEngine;

use super::*;

/// A directory of its own per test, removed when it ends.
struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let p = std::env::temp_dir().join(format!(
            "letibot-dockergc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Fixture(p)
    }

    fn at(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn places(&self, engine: Option<&Path>) -> Places {
        Places {
            gc_root: self.at("gc"),
            box_id: "lab2x1".into(),
            settings: Settings::default(),
            data_dir: self.at("data"),
            rundir: self.at("run"),
            firecode_runs: None,
            firecode_cache: None,
            proc_root: self.at("proc"),
            cgroup_fs: self.at("cg"),
            docker_engine: engine.map(Path::to_path_buf),
        }
    }

    fn store(&self) -> Store {
        Store::open_in_memory().expect("an in-memory store")
    }

    fn owner(&self, id: &str, state: OwnerState) -> OwnerRow {
        let repo = self.at("repo");
        OwnerRow {
            state,
            closed_ms: (state != OwnerState::Live).then(|| now_ms() - 1_000),
            ..OwnerRow::live(
                id,
                OwnerKind::Package,
                Some(repo.display().to_string()),
                now_ms() - 5_000,
            )
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The hands the pass reaches the engine through: the stub's socket, exactly as `BoxHands` reaches
/// the box's own.
struct StubHands {
    engine: Option<PathBuf>,
}

impl Hands for StubHands {
    fn firecode(&self, _args: &[String]) -> Result<String, String> {
        Err("this fixture has no firecode".into())
    }

    fn lock_held(&self, _target: &Path) -> Result<bool, String> {
        Ok(false)
    }

    fn host_command(&self, _host: &str, _script: &str) -> Result<String, String> {
        Err("this fixture has no hosts".into())
    }

    fn host_read(&self, _host: &str, _path: &str) -> Result<Vec<u8>, String> {
        Err("this fixture has no hosts".into())
    }

    fn docker(&self, req: &crate::docker::Request) -> Result<crate::docker::Response, String> {
        let Some(engine) = &self.engine else {
            return Err("no docker engine in this fixture".into());
        };
        crate::docker::call(engine, req, Duration::from_secs(10))
    }
}

fn ops(engine: Option<&Path>, docker: crate::docker::Settings) -> Ops {
    Ops {
        by: "test".into(),
        firecode: None,
        grace: Duration::from_millis(1),
        after_kill: Duration::from_millis(1),
        daemon_grace: Duration::from_millis(1),
        hands: Arc::new(StubHands {
            engine: engine.map(Path::to_path_buf),
        }),
        docker,
    }
}

fn ours(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// G6-1 as the pass runs it: closing the owner removes its containers and networks by label, and a
/// container made outside the proxy is untouched.
#[test]
fn a_close_takes_the_owners_containers_and_networks_and_leaves_a_stranger() {
    let fx = Fixture::new("close");
    let stub = StubEngine::start(fx.at("engine").as_path());
    let places = fx.places(Some(stub.socket()));
    let store = fx.store();
    let owner = fx.owner("s-1794555261791226969", OwnerState::Closing);
    let labels = ours(&[
        (crate::docker::LABEL_OWNER, "s-1794555261791226969"),
        (crate::docker::LABEL_BOX, "lab2x1"),
    ]);
    stub.seed_container("lb-17945552-web", labels.clone());
    stub.seed_network("lb-17945552_default", labels);
    stub.seed_container("bench-db", ours(&[("com.docker.compose.project", "bench")]));
    store.put_owner(&owner).expect("the owner row");
    super::register(&store, &places, &owner).expect("its root and its holdings");

    let pass = super::pass(
        &store,
        &places,
        &ops(Some(stub.socket()), crate::docker::Settings::default()),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");

    assert_eq!(
        stub.container_names(),
        vec!["bench-db".to_string()],
        "the owner's container went and the stranger stayed"
    );
    assert_eq!(stub.remaining().1, 0, "its network went");
    let said: Vec<String> = pass.acts.iter().map(|a| a.line()).collect();
    assert!(
        said.iter()
            .any(|l| l.contains("container") && l.contains("at close")),
        "the journal names the container's end: {said:?}"
    );
    assert!(
        said.iter().any(|l| l.contains("network")),
        "and the network's: {said:?}"
    );
    let closed = store.owner(&owner.id).expect("read").expect("there");
    assert!(
        closed.state == OwnerState::Closed,
        "with no compute left the owner is closed, not closing: {}",
        closed.state.word()
    );
}

/// G6-2: a named volume survives the close, is in the plan with its size, and goes at settle.
#[test]
fn a_named_volume_survives_the_close_and_goes_at_settle() {
    let fx = Fixture::new("volume");
    let stub = StubEngine::start(fx.at("engine").as_path());
    let places = fx.places(Some(stub.socket()));
    let store = fx.store();
    let owner = fx.owner("s-1794555261791226969", OwnerState::Closed);
    stub.seed_volume(
        "bench_data",
        ours(&[
            (crate::docker::LABEL_OWNER, "s-1794555261791226969"),
            (crate::docker::LABEL_BOX, "lab2x1"),
        ]),
        6 * 1024 * 1024,
    );
    store.put_owner(&owner).expect("the owner row");
    super::register(&store, &places, &owner).expect("its root and its holdings");

    // The plan: one line, with the size the engine reported and a verdict that keeps it for now.
    let plan = super::plan(&store, &places, Some(&owner.id), now_ms()).expect("a plan");
    let view = plan
        .owners
        .iter()
        .find(|o| o.id == owner.id)
        .expect("its block");
    let volume = view
        .holds
        .iter()
        .find(|h| h.kind == "volume")
        .expect("the volume is discovered");
    assert_eq!(volume.handle, "bench_data");
    assert_eq!(
        volume.bytes,
        Some(6 * 1024 * 1024),
        "the plan prints a size"
    );
    assert_eq!(
        volume.verdict.word(),
        "reap at",
        "a closed owner's volume waits for the work grace: {}",
        volume.verdict.said()
    );
    assert!(
        volume.verdict.said().contains("not proved landed"),
        "and it says which grace: {}",
        volume.verdict.said()
    );

    // A pass now keeps it.
    let pass = super::pass(
        &store,
        &places,
        &ops(Some(stub.socket()), crate::docker::Settings::default()),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");
    assert_eq!(stub.remaining().2, 1, "the volume is still there");
    assert!(
        pass.kept.iter().any(|k| k.contains("volume bench_data")),
        "and the pass says why: {:?}",
        pass.kept
    );

    // Settled: it goes, by its label.
    store
        .set_owner_state(&owner.id, OwnerState::Settled, "landed", now_ms())
        .expect("settled");
    super::pass(
        &store,
        &places,
        &ops(Some(stub.socket()), crate::docker::Settings::default()),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");
    assert_eq!(stub.remaining().2, 0, "at settle the volume goes");
    let row = store
        .holdings_of(&owner.id)
        .expect("holdings")
        .into_iter()
        .find(|h| h.kind == "volume")
        .expect("its row");
    assert_eq!(row.state, letibot_tokencore::store::HoldingState::Gone);
}

/// A box with no engine socket holds nothing of docker's and says nothing about it: not being able
/// to tell is never a reason to reap (`docs/lifecycle.md` d.4 rule 8).
#[test]
fn a_box_with_no_engine_holds_nothing_of_dockers() {
    let fx = Fixture::new("noengine");
    let places = fx.places(None);
    let store = fx.store();
    let owner = fx.owner("s-1794555261791226969", OwnerState::Closed);
    store.put_owner(&owner).expect("the owner row");
    super::register(&store, &places, &owner).expect("its root");
    let plan = super::plan(&store, &places, Some(&owner.id), now_ms()).expect("a plan");
    let view = plan
        .owners
        .iter()
        .find(|o| o.id == owner.id)
        .expect("its block");
    assert!(
        view.holds.iter().all(|h| h.kind != "volume"),
        "no engine, no docker holdings: {:?}",
        view.holds
            .iter()
            .map(|h| h.kind.clone())
            .collect::<Vec<_>>()
    );
    let pass = super::pass(
        &store,
        &places,
        &ops(None, crate::docker::Settings::default()),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");
    assert!(pass.acts.iter().all(|a| a.kind != "container"));
}

/// The opt-in `[gc.docker]` prune: off unless it names labels, and then only what they match.
#[test]
fn the_opt_in_prune_is_the_only_thing_a_pass_prunes() {
    let fx = Fixture::new("prune");
    let stub = StubEngine::start(fx.at("engine").as_path());
    let places = fx.places(Some(stub.socket()));
    let store = fx.store();
    let owner = fx.owner("s-1794555261791226969", OwnerState::Closed);
    store.put_owner(&owner).expect("the owner row");
    stub.seed_image(
        "bench/api:1",
        ours(&[("com.docker.compose.project", "bench")]),
        1024,
        1_000,
    );
    stub.seed_image(
        "ours:built",
        ours(&[(crate::docker::LABEL_OWNER, "s-1794555261791226969")]),
        1024,
        1_000,
    );

    // Off by default: a pass with no `[gc.docker]` prunes nothing.
    super::pass(
        &store,
        &places,
        &ops(Some(stub.socket()), crate::docker::Settings::default()),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");
    assert!(
        !stub.requests().iter().any(|r| r.path().contains("prune")),
        "no policy, no prune"
    );

    let policy = crate::docker::Settings {
        prune_label: vec!["com.docker.compose.project=bench".into()],
        prune_images_unused_for_ms: Some(30 * 24 * 60 * 60 * 1000),
        ..crate::docker::Settings::default()
    };
    let pass = super::pass(
        &store,
        &places,
        &ops(Some(stub.socket()), policy),
        Some(&owner.id),
        now_ms(),
    )
    .expect("a pass");
    assert!(
        pass.acts
            .iter()
            .any(|a| a.kind == "docker" && a.owner == "box"),
        "the prune is an act of the box, journaled: {:?}",
        pass.acts.iter().map(|a| a.line()).collect::<Vec<_>>()
    );
    // `ours:built` carries our label, which the configured one does not match, so it survives.
    assert!(
        stub.requests()
            .iter()
            .any(|r| r.path().contains("images/prune")),
        "the image prune ran"
    );
    let events = store.reap_events_of("box").expect("the box journal");
    assert!(
        events.iter().any(|e| e.said.contains("[gc.docker]")),
        "and it is readable with `letibot gc --log`: {events:?}"
    );
}
