//! **Resource leases on a shared host, against a fake ssh and a fake docker** (RH9).
//!
//! The rig is `support/fake_remote.rs`: a script standing in for `ssh` that runs the command
//! locally against a temporary "remote home", so `~/letibot-remote` is expanded on the far side.
//! This file adds one more fake — a `docker` in the rig's own `bin`, first on the far side's
//! `PATH` — because a teardown's whole job is *what did it remove*, and the answer has to come from
//! the program that would have removed it. **No test here contacts another machine, and none of
//! them can reach a real docker**: the fake is what `command -v docker` finds.
//!
//! # What is measured, and where each number comes from
//!
//! * **The ephemeral range** is this box's own `/proc/sys/net/ipv4/ip_local_port_range` — the fake
//!   ssh runs the census script for real, so the range is a measurement and not a constant. When a
//!   test needs a *deterministic* block it says what the range is instead
//!   ([`Blocks::with_ephemeral`]), because a kernel's range cannot be moved from a test.
//! * **The listening ports** are this box's real ones, so the *port somebody already holds* case is
//!   made true by binding one, not by pretending.
//! * **The store** is a real `sessions.db` on disk, so *a restart keeps the leases* is a second
//!   connection to the same file and not a second `HashMap`.

#[path = "support/fake_remote.rs"]
mod fake_remote;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_tokencore::store::{Ask, Lease, Store};
use letibot_tools::lease::{Blocks, Leases};
use letibot_tools::remote::Paths;
use letibot_transcript::ToolOutcome;
use serde_json::json;

use fake_remote::Rig;

/// **The store, as the lease engine needs it** — the daemon's `StoreLeases` in miniature, over one
/// shared connection. What it proves is the seam: a tool cannot open a store and does not try.
struct Book {
    store: Mutex<Store>,
}

impl Book {
    fn new(path: &Path) -> Arc<Book> {
        Arc::new(Book {
            store: Mutex::new(Store::open(path).expect("the test's store")),
        })
    }

    fn held(&self) -> Vec<Lease> {
        self.store
            .lock()
            .expect("the store")
            .held_leases()
            .expect("held")
    }
}

impl Leases for Book {
    fn take(&self, ask: &Ask<'_>) -> Result<Lease, String> {
        self.store
            .lock()
            .expect("the store")
            .allocate_lease(ask)
            .map_err(|e| e.to_string())
    }

    fn release(&self, id: i64, by: &str) -> Result<(), String> {
        self.store
            .lock()
            .expect("the store")
            .release_lease(id, by, Store::lease_now_ms())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// A store on disk, in a directory of the test's own.
fn store_at(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-lease-store-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("sessions.db");
    // The schema has to exist before anything opens it a second time.
    Store::open(&path).expect("a store");
    path
}

/// The engine a session would use, wired to one rig and one store.
fn blocks(rig: &Rig, store: &Path, session: &str) -> Blocks {
    Blocks::new(session)
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
        .with_leases(Arc::new(StoreLeasesAt::new(store, session)))
}

/// A per-session `Leases` over one store file — the daemon's `StoreLeases` exactly, and here
/// because `letibot-tools`' tests may not depend on `harnessd`.
struct StoreLeasesAt {
    path: PathBuf,
    store: Mutex<Option<Store>>,
}

impl StoreLeasesAt {
    fn new(path: &Path, _session: &str) -> StoreLeasesAt {
        StoreLeasesAt {
            path: path.to_path_buf(),
            store: Mutex::new(None),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.store.lock().expect("the lease connection");
        if g.is_none() {
            *g = Some(Store::open(&self.path).map_err(|e| e.to_string())?);
        }
        f(g.as_ref().expect("just opened"))
    }
}

impl Leases for StoreLeasesAt {
    fn take(&self, ask: &Ask<'_>) -> Result<Lease, String> {
        self.with(|s| s.allocate_lease(ask).map_err(|e| e.to_string()))
    }

    fn release(&self, id: i64, by: &str) -> Result<(), String> {
        self.with(|s| {
            s.release_lease(id, by, Store::lease_now_ms())
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

/// **The fake docker**, and the two lists it answers from.
///
/// `docker ps -a --format '{{.Names}}'` prints the container list; `docker compose ls --format
/// '{{.Name}}'` prints the project list; `docker rm -f …` and `docker compose -p … down` remove
/// from them and append to a log the test reads. Written with
/// [`letibot_tools::testing::write_stub`] and not by hand, because a script that is written and
/// then exec'd races every other fork in this binary (the `ETXTBSY` rake).
fn fake_docker(rig: &Rig) -> PathBuf {
    let containers = rig.bin().join("containers");
    let projects = rig.bin().join("projects");
    let script = format!(
        "#!/bin/bash\n\
         printf 'DOCKER %s\\n' \"$*\" >> {log}\n\
         case \"$1\" in\n\
         \x20 ps) cat {containers} 2>/dev/null; exit 0;;\n\
         \x20 compose)\n\
         \x20   if [ \"$2\" = ls ]; then cat {projects} 2>/dev/null; exit 0; fi\n\
         \x20   if [ \"$2\" = -p ]; then exit 0; fi\n\
         \x20   exit 0;;\n\
         \x20 rm)\n\
         \x20   shift 2\n\
         \x20   for n; do grep -vxF -- \"$n\" {containers} > {containers}.new 2>/dev/null; \
         mv {containers}.new {containers} 2>/dev/null; done\n\
         \x20   exit 0;;\n\
         esac\n\
         exit 0\n",
        log = rig.bin().join("docker.log").display(),
        containers = containers.display(),
        projects = projects.display(),
    );
    let path = rig.bin().join("docker");
    let program = rig.bin().join("letibot-test-docker.sh");
    std::fs::write(&program, script).expect("the docker stub's text");
    letibt_write_stub(&path, &program);
    path
}

/// `write_stub` takes the body, so the body is read from the file just written and handed over —
/// one call, so the text exists once and the `ETXTBSY` probe is the tree's own.
fn letibt_write_stub(path: &Path, program: &Path) {
    let body = std::fs::read_to_string(program).expect("the stub's text");
    letibot_tools::testing::write_stub(path, &body);
    let _ = std::fs::remove_file(program);
}

/// Plant what is "running" on the fake host.
fn plant(rig: &Rig, containers: &[&str], projects: &[&str]) {
    std::fs::write(rig.bin().join("containers"), containers.join("\n") + "\n").expect("containers");
    std::fs::write(rig.bin().join("projects"), projects.join("\n") + "\n").expect("projects");
}

/// What the fake docker was asked to do, in order.
fn docker_calls(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(rig.bin().join("docker.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// **The host's own ephemeral range**, read here as the census reads it there — so an assertion
/// that a block is *outside* it is a comparison of two real numbers.
fn ephemeral_of_this_box() -> (u16, u16) {
    if let Ok(text) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        let mut it = text.split_whitespace();
        if let (Some(lo), Some(hi)) = (it.next(), it.next())
            && let (Ok(lo), Ok(hi)) = (lo.parse::<u16>(), hi.parse::<u16>())
        {
            return (lo, hi);
        }
    }
    // A box whose kernel keeps the range in `sysctl` — macOS. The census script asks the same two
    // questions, so a test that could not read it here could not have run the census either.
    let out = std::process::Command::new("sysctl")
        .args(["-n", "net.inet.ip.portrange.first"])
        .output()
        .expect("sysctl");
    let lo: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("lo");
    let out = std::process::Command::new("sysctl")
        .args(["-n", "net.inet.ip.portrange.last"])
        .output()
        .expect("sysctl");
    let hi: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("hi");
    (lo, hi)
}

/// **Two sessions asking one host get blocks that do not overlap, and neither is inside the
/// host's own ephemeral range.** The collision the operator's Stroppy session hit eighteen times.
#[test]
fn two_sessions_get_disjoint_blocks_outside_the_ephemeral_range() {
    let rig = Rig::new("lease-two", &["fake1"]);
    let store = store_at("two");
    let (lo, hi) = ephemeral_of_this_box();

    let a = blocks(&rig, &store, "s-alpha")
        .take("fake1", 4)
        .expect("alpha's block");
    let b = blocks(&rig, &store, "s-beta")
        .take("fake1", 4)
        .expect("beta's block");

    assert!(
        a.port_first > hi && a.port_last > hi,
        "alpha's block {}-{} is above the host's range {lo}-{hi}",
        a.port_first,
        a.port_last
    );
    assert!(
        b.port_first > hi,
        "beta's block {}-{} is above the host's range {lo}-{hi}",
        b.port_first,
        b.port_last
    );
    assert_eq!(a.ports(), 4, "a block is the size that was asked for");
    assert!(
        b.port_first > a.port_last || b.port_last < a.port_first,
        "the two blocks do not overlap: {}-{} and {}-{}",
        a.port_first,
        a.port_last,
        b.port_first,
        b.port_last
    );
    // **The prefix is the session's, and it carries the block's number** so a teardown of one
    // cannot take the other's containers with it.
    assert!(
        a.prefix.starts_with("letibot-fake1-s-alpha-"),
        "{}",
        a.prefix
    );
    assert!(
        b.prefix.starts_with("letibot-fake1-s-beta-"),
        "{}",
        b.prefix
    );
    assert_ne!(a.prefix, b.prefix);
    // And every fact a command needs is in the environment it is handed.
    let env: std::collections::BTreeMap<String, String> = a.env().into_iter().collect();
    assert_eq!(env["LETIBOT_LEASE_PORT_FIRST"], a.port_first.to_string());
    assert_eq!(env["LETIBOT_LEASE_PREFIX"], a.prefix);
}

/// **A port somebody else is already listening on is skipped** — the census half, measured by
/// really binding one. The range is stated so the block is deterministic: the kernel's own range
/// cannot be moved from a test, and `pick` starts at `hi + 1`.
#[test]
fn a_port_someone_is_listening_on_is_skipped() {
    let rig = Rig::new("lease-busy", &["fake1"]);
    let store = store_at("busy");
    // A port held by this test, on this box, which is the fake host's own kernel.
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = held.local_addr().expect("the address").port();
    assert!(
        port > 1024,
        "an ephemeral port is above the unprivileged floor"
    );

    let block = blocks(&rig, &store, "s-busy")
        .with_ephemeral((1024, port - 1))
        .take("fake1", 1)
        .expect("a block");
    assert!(
        block.port_first != port,
        "the block starts past the listening port: {} against {port}",
        block.port_first
    );
    assert!(block.port_first > port, "{} > {port}", block.port_first);
    drop(held);
}

/// **A restart keeps the leases.** A lease is a row, so an engine built a moment later over the
/// same store — a different connection, no memory in common — still sees the block taken.
#[test]
fn a_lease_survives_a_new_engine_over_the_same_store() {
    let rig = Rig::new("lease-restart", &["fake1"]);
    let store = store_at("restart");
    let first = blocks(&rig, &store, "s-first")
        .take("fake1", 2)
        .expect("the first block");
    // The daemon comes back: a new engine, a new connection, and nothing shared but the file.
    let again = blocks(&rig, &store, "s-second")
        .take("fake1", 2)
        .expect("the second block");
    assert!(
        again.port_first > first.port_last,
        "the restarted engine allocated past the block the store already held: {}-{} against \
         {}-{}",
        again.port_first,
        again.port_last,
        first.port_first,
        first.port_last
    );
    let held = Book::new(&store).held();
    assert_eq!(held.len(), 2, "both blocks are held: {held:?}");
}

/// **Releasing frees the block for the next caller, and removes only what the block created.**
///
/// Three containers are planted on the fake host: one under this lease's prefix, one under
/// another session's, and one the operator's. Only the first is removed.
#[test]
fn a_release_frees_the_block_and_removes_only_its_own_prefix() {
    let rig = Rig::new("lease-tidy", &["fake1"]);
    let store = store_at("tidy");
    let engine = blocks(&rig, &store, "s-tidy");
    let lease = engine.take("fake1", 2).expect("a block");
    fake_docker(&rig);
    plant(
        &rig,
        &[
            &format!("{}-db", lease.prefix),
            &format!("{}-cache", lease.prefix),
            "letibot-fake1-s-other-1-db",
            "operator-s-own-postgres",
        ],
        &[&lease.prefix, "somebody-elses-project"],
    );

    let said = engine.release(&lease, "the test ended");
    let removed: Vec<String> = docker_calls(&rig)
        .into_iter()
        .filter(|l| l.starts_with("DOCKER rm"))
        .collect();
    assert_eq!(
        removed.len(),
        1,
        "one `docker rm`, and it names its own: {removed:?}"
    );
    let said_rm = &removed[0];
    assert!(
        said_rm.contains(&format!("{}-db", lease.prefix)),
        "{said_rm}"
    );
    assert!(
        said_rm.contains(&format!("{}-cache", lease.prefix)),
        "{said_rm}"
    );
    assert!(
        !said_rm.contains("s-other"),
        "another session's container is not this lease's to remove: {said_rm}"
    );
    assert!(
        !said_rm.contains("operator-s-own"),
        "the operator's own container is not this lease's to remove: {said_rm}"
    );
    let downs: Vec<String> = docker_calls(&rig)
        .into_iter()
        .filter(|l| l.starts_with("DOCKER compose -p"))
        .collect();
    assert_eq!(downs.len(), 1, "{downs:?}");
    assert!(downs[0].contains(&lease.prefix), "{downs:?}");
    assert!(
        !downs[0].contains("somebody-elses-project"),
        "a compose project that is not ours is not brought down: {downs:?}"
    );
    // The row, the answer, and the block being free again.
    assert!(
        said.iter().any(|l| l.contains("removed the container")),
        "{said:?}"
    );
    assert!(Book::new(&store).held().is_empty(), "the block went back");
    let next = engine.take("fake1", 2).expect("the same block again");
    assert_eq!(
        (next.port_first, next.port_last),
        (lease.port_first, lease.port_last),
        "a released block is the first one the next caller is given"
    );
}

/// **A name on the never-touch list is left alone, and the refusal names the pattern.**
///
/// The list is the second wall behind the prefix: the prefix says *what is mine*, the operator's
/// list says *what is not*, and the list wins. It is the operator's own framing —
/// *pgfuzz on lubuntu1, flowy-dogfood-pg on lubuntu3, the etcd fuzz on lab2x1*.
#[test]
fn a_never_touch_name_survives_a_teardown_that_would_have_had_it() {
    let rig = Rig::new("lease-never", &["fake1"]);
    let store = store_at("never");
    // The operator's list, on the host this test uses.
    let mut text = std::fs::read_to_string(rig.hosts_file()).expect("hosts.toml");
    text = text.replace(
        "[host.fake1]",
        "[host.fake1]\nnever_touch = [\"pgfuzz\"]\nreserve = 4\nflags = [\"heavy\", \"tmpfs\"]",
    );
    std::fs::write(rig.hosts_file(), text).expect("hosts.toml");

    let engine = blocks(&rig, &store, "s-never");
    let lease = engine.take("fake1", 2).expect("a block");
    fake_docker(&rig);
    // The session's own container matches the prefix — and the operator's workload happens to sit
    // under it too, because a prefix is a pattern and the list is what bounds it.
    plant(
        &rig,
        &[
            &format!("{}-db", lease.prefix),
            &format!("{}-pgfuzz-1", lease.prefix),
        ],
        &[],
    );

    let said = engine.release(&lease, "the test ended");
    let removed: Vec<String> = docker_calls(&rig)
        .into_iter()
        .filter(|l| l.starts_with("DOCKER rm"))
        .collect();
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert!(
        removed[0].contains(&format!("{}-db", lease.prefix)),
        "{removed:?}"
    );
    assert!(
        !removed[0].contains("pgfuzz"),
        "the never-touch container was NOT removed: {removed:?}"
    );
    let refusal = said
        .iter()
        .find(|l| l.contains("never_touch"))
        .unwrap_or_else(|| panic!("the refusal names the pattern: {said:?}"));
    assert!(refusal.contains("pgfuzz"), "{refusal}");
    assert!(refusal.contains("fake1"), "{refusal}");
    assert!(refusal.contains("NOT removed"), "{refusal}");
    // The container is still there, which is the fact that matters.
    let left = std::fs::read_to_string(rig.bin().join("containers")).expect("containers");
    assert!(left.contains("pgfuzz"), "{left}");
    assert!(!left.contains(&format!("{}-db", lease.prefix)), "{left}");
}

/// **A host that has nothing of ours left is a sentence, not a refusal** — and the block still goes
/// back. A container that went with its process is the ordinary case.
///
/// The fake docker is planted so this test cannot reach a real one on a box that has it: the
/// far side's `PATH` puts the rig's `bin` first, and what is there answers.
#[test]
fn a_host_with_nothing_of_ours_left_is_a_sentence() {
    let rig = Rig::new("lease-nothing", &["fake1"]);
    let store = store_at("nothing");
    let engine = blocks(&rig, &store, "s-plain");
    let lease = engine.take("fake1", 1).expect("a block");
    fake_docker(&rig);
    plant(
        &rig,
        &["somebody-elses-postgres"],
        &["somebody-elses-project"],
    );
    let said = engine.teardown("fake1", &lease.prefix).expect("a teardown");
    assert!(said.iter().any(|l| l.contains("nothing of")), "{said:?}");
    assert!(
        !docker_calls(&rig)
            .iter()
            .any(|l| l.starts_with("DOCKER rm") || l.starts_with("DOCKER compose -p")),
        "nothing was removed: {:?}",
        docker_calls(&rig)
    );
    engine.release(&lease, "the test ended");
    assert!(Book::new(&store).held().is_empty());
}

/// **A container another session made under ITS prefix is never touched, whatever the order.**
///
/// The one shape a prefix check can get wrong: two sessions whose prefixes share a head. This is
/// `starts_with`'s job, and a `contains` here would be a teardown that removed a stranger's work.
#[test]
fn a_prefix_that_is_a_prefix_of_another_is_still_not_it() {
    let rig = Rig::new("lease-prefix", &["fake1"]);
    let store = store_at("prefix");
    let engine = blocks(&rig, &store, "s-ab");
    let lease = engine.take("fake1", 1).expect("a block");
    fake_docker(&rig);
    plant(
        &rig,
        &[&format!("{}-db", lease.prefix), "letibot-fake1-s-ab-9-db"],
        &[],
    );
    engine.release(&lease, "the test ended");
    let rm: BTreeSet<String> = docker_calls(&rig)
        .into_iter()
        .filter(|l| l.starts_with("DOCKER rm"))
        .collect();
    assert_eq!(rm.len(), 1, "{rm:?}");
    let line = rm.iter().next().expect("one removal");
    assert!(line.contains(&format!("{}-db", lease.prefix)), "{line}");
    assert!(
        !line.contains("letibot-fake1-s-ab-9-db"),
        "a longer prefix that merely starts with ours is somebody else's: {line}"
    );
}

/// **A scenario's role takes its port from the session's lease, and the client is pointed at it**
/// (RH9). The whole point of `ready: {"port": "lease"}`: nobody typed a port, and the number the
/// client got is one the block owns.
#[test]
fn a_scenarios_port_comes_out_of_the_lease() {
    use letibot_tools::builtins::scenario::Scenario;
    use letibot_tools::exec::ProcessHost;
    use letibot_tools::remote::{Hosts, RemoteProcesses, Ssh};

    let rig = Rig::new("lease-scenario", &["fake1"]);
    let store = store_at("scenario");
    let engine = blocks(&rig, &store, "s-scn");
    // **The block is stated rather than discovered**, and stated as the box's OWN range so the
    // claim below is the real one: whatever port the client is handed is above the range this
    // kernel hands out to outgoing connections.
    let (lo, hi) = ephemeral_of_this_box();
    let engine = engine.with_ephemeral((hi, hi));

    // `local` is a leg on this box through the same fake ssh — the `run` host, which is where a
    // client usually wants to be.
    let hosts = Hosts::load_from(rig.hosts_file());
    let host = hosts.get("fake1").expect("fake1").clone();
    let ssh = Ssh::connect(host, Some(rig.ssh_program()), rig.paths()).expect("the connection");
    let local: Option<Arc<dyn ProcessHost>> = Some(Arc::new(RemoteProcesses::new(
        ssh,
        "local".to_string(),
        rig.remote_root().join("local").display().to_string(),
    )));

    let answer = Scenario::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
        .with_leases(Arc::new(engine))
        .answer(
            &json!({
                "roles": {
                    "web": {
                        "host": "fake1",
                        "command": "python3 -m http.server ${web.port} --bind 127.0.0.1",
                        "ready": {"port": "lease"},
                        "ready_deadline_s": 20
                    }
                },
                "run": {"command": "echo the-client-would-use ${web.port}"}
            }),
            local,
            "scenario-lease",
        );
    let text = answer.payload.clone();
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{text}");

    // The client's own line carries a port, and it is inside the block: above the box's ephemeral
    // range's floor and inside what was stated.
    let said = text
        .lines()
        .find(|l| l.contains("the-client-would-use"))
        .unwrap_or_else(|| panic!("the run's output is in the answer:\n{text}"));
    let port: u16 = said
        .rsplit("the-client-would-use ")
        .next()
        .and_then(|t| {
            t.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|d| d.parse().ok())
        })
        .unwrap_or_else(|| panic!("a port after the words:\n{text}"));
    assert!(
        port > hi,
        "the port came from the block, above the box's range {lo}-{hi}: {port}\n{text}"
    );
    // Every block this call took went back, and the answer says so.
    assert!(text.contains("lease: "), "{text}");
    assert!(
        Book::new(&store).held().is_empty(),
        "the block went back with the scenario"
    );
}

/// The hosts file the rig writes is readable through the engine's own reader, with the three keys
/// this package adds — the seam a scenario's `never_touch`/`reserve` come through.
#[test]
fn the_rigs_hosts_file_carries_the_three_keys() {
    let rig = Rig::new("lease-keys", &["fake1"]);
    let mut text = std::fs::read_to_string(rig.hosts_file()).expect("hosts.toml");
    text = text.replace(
        "[host.fake1]",
        "[host.fake1]\nreserve = 4\nflags = [\"heavy\", \"tmpfs\"]\nnever_touch = [\"pgfuzz\", \"etcd\"]",
    );
    std::fs::write(rig.hosts_file(), text).expect("hosts.toml");
    let hosts = letibot_tools::remote::Hosts::load_from(rig.hosts_file());
    let host = hosts.get("fake1").expect("fake1");
    assert_eq!(host.reserve, 4);
    assert_eq!(host.flags, vec!["heavy", "tmpfs"]);
    assert_eq!(host.never_touch, vec!["pgfuzz", "etcd"]);
    assert!(host.has_flag("HEAVY"));
    assert_eq!(host.never_touch_hit("pgfuzz -t 3600"), Some("pgfuzz"));
    assert_eq!(host.never_touch_hit("etcd-fuzz-0"), Some("etcd"));
    assert_eq!(host.never_touch_hit("cargo test"), None);
    // A server 32 cores busy with 4 of them reserved has 24 for anyone else — and an oversubscribed
    // box reports a negative rather than a zero, because the reason is the useful half.
    assert_eq!(host.spare_cores(32, "4.00 2.00 1.00"), Some(24.0));
    assert_eq!(host.spare_cores(8, "12.00 9.00 5.00"), Some(-8.0));
    assert_eq!(host.spare_cores(0, "1.00"), None);
    assert_eq!(host.spare_cores(8, ""), None);
    let _ = Paths::from_env();
}
