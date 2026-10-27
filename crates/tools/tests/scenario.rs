//! **`scenario` against a fake ssh** — a service that comes up, a client that uses its port, and a
//! teardown that always happens.
//!
//! Every service here is a real process (`python3 -m http.server`), on a real port of this box,
//! started through the fake ssh's job directory — so the readiness check, the substitution of
//! `${server.port}` into the client, and the group kill at the end are all the production ones.
//! Nothing here contacts another machine.

#[path = "support/fake_remote.rs"]
mod fake_remote;

use std::sync::Arc;
use std::time::Duration;

use letibot_tools::builtins::scenario::Scenario;
use letibot_tools::exec::ProcessHost;
use letibot_tools::remote::{Hosts, RemoteProcesses, Ssh};
use letibot_transcript::ToolOutcome;
use serde_json::json;

use fake_remote::{Rig, free_port, listening, until};

/// A scenario wired to one rig.
fn tool(rig: &Rig) -> Scenario {
    Scenario::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
}

/// **This box as the session's own backend** — a `local` leg's process host, and the one a `run`
/// with no `host` uses. The processes are the fake's (their commands run on this box through the
/// fake ssh), so what this exercises is exactly the part that differs: `local`'s readiness check
/// asks *this* box's kernel instead of a host's.
fn here(rig: &Rig) -> Option<Arc<dyn ProcessHost>> {
    let hosts = Hosts::load_from(rig.hosts_file());
    let host = hosts.get("fake1")?.clone();
    let ssh = Ssh::connect(host, Some(rig.ssh_program()), rig.paths()).ok()?;
    Some(Arc::new(RemoteProcesses::new(
        ssh,
        "local".to_string(),
        rig.remote_root().join("local").display().to_string(),
    )))
}

fn rows(payload: &str) -> Vec<&str> {
    payload
        .lines()
        .filter(|l| l.starts_with("  "))
        .map(|l| l.trim_start())
        .collect()
}

/// **A server role becomes ready by port, the client uses `${server.port}`, and everything is torn
/// down afterwards.** The whole fixture, in one test, because each of the three is worthless
/// without the other two.
#[test]
fn a_server_is_ready_by_port_and_the_client_uses_it_and_is_torn_down() {
    let rig = Rig::new("by-port", &["fake1"]);
    let port = free_port();
    let answer = tool(&rig).answer(
        &json!({
            "roles": {
                "web": {
                    "host": "fake1",
                    "command": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "ready": {"port": port}
                }
            },
            "run": {
                "host": "fake1",
                "command": format!(
                    "python3 -c \"import urllib.request; \
                     print('status=' + str(urllib.request.urlopen('http://127.0.0.1:{port}/').status))\""
                )
            },
            "deadline_s": 60
        }),
        here(&rig),
        "scenario-by-port",
    );
    let text = answer.payload.clone();
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{text}");
    assert_eq!(
        rows(&text).len(),
        2,
        "one row per role, one for the run:\n{text}"
    );
    assert!(
        text.contains("web on fake1 · stopped · ")
            && text.contains("ready after ")
            && text.contains("the scenario ended"),
        "the service's row says it came up and was torn down:\n{text}"
    );
    assert!(
        text.contains("run on fake1 · exited 0"),
        "the client ran, so `${{web.port}}` was substituted:\n{text}"
    );
    assert!(
        text.contains("status=200"),
        "the client really reached the server:\n{text}"
    );
    // **The teardown is a fact about the machine**, not about the row: the port is free again.
    assert!(
        until(Duration::from_secs(10), || !listening(port)),
        "the server is still listening on {port} after the scenario ended"
    );
}

/// **A role that never becomes ready fails the scenario and tears down the ones that did start.**
#[test]
fn a_role_that_never_answers_ready_fails_and_the_started_ones_are_ended() {
    let rig = Rig::new("never-ready", &["fake1"]);
    let port = free_port();
    let started = std::time::Instant::now();
    let answer = tool(&rig).answer(
        &json!({
            "roles": {
                "web": {
                    "host": "fake1",
                    "command": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "ready": {"port": port}
                },
                "slow": {
                    "host": "fake1",
                    "command": "sleep 60",
                    "ready": {"port": free_port()},
                    "ready_deadline_s": 1
                }
            },
            "deadline_s": 60
        }),
        here(&rig),
        "scenario-never-ready",
    );
    let took = started.elapsed();
    let text = answer.payload.clone();
    assert!(
        matches!(answer.outcome, ToolOutcome::Failed { .. }),
        "a role that never came up is a failed scenario:\n{text}"
    );
    assert!(text.contains("did not finish"), "{text}");
    assert!(
        text.contains("slow on fake1 · stopped") && text.contains("did not answer within"),
        "the row says what it was waiting for:\n{text}"
    );
    assert!(
        text.contains("web on fake1 · stopped"),
        "the role that DID come up was torn down:\n{text}"
    );
    assert!(
        took < Duration::from_secs(30),
        "a 1-second readiness deadline returned in {took:?}"
    );
    assert!(
        until(Duration::from_secs(10), || !listening(port)),
        "the web role was left listening on {port}"
    );
}

/// **A cycle in `after` is refused before a single process exists** — the refusal a scenario that
/// would hang has to give, and the ssh log is the evidence that nothing was started.
#[test]
fn a_cycle_is_refused_before_anything_starts() {
    let rig = Rig::new("cycle", &["fake1"]);
    let answer = tool(&rig).answer(
        &json!({
            "roles": {
                "a": {"host": "fake1", "command": "sleep 1", "after": ["b"]},
                "b": {"host": "fake1", "command": "sleep 1", "after": ["a"]}
            }
        }),
        here(&rig),
        "scenario-cycle",
    );
    let text = answer.payload.clone();
    assert!(
        matches!(answer.outcome, ToolOutcome::Failed { .. }),
        "{text}"
    );
    assert!(text.contains("cycle"), "{text}");
    assert!(
        rig.ssh_calls().is_empty(),
        "nothing was started, so ssh was never called: {:?}",
        rig.ssh_calls()
    );
}

/// **`after` starts a role only once the one it names is READY**, and `${role.host}` and
/// `${role.port}` both resolve in a later role's command — the two things the dependency edge is
/// for.
#[test]
fn after_orders_the_starts_and_a_roles_facts_resolve() {
    let rig = Rig::new("after", &["fake1"]);
    let port = free_port();
    let client = format!(
        "echo peer=${{web.host}}; python3 -c \"import urllib.request; \
         print('ok=' + str(urllib.request.urlopen('http://127.0.0.1:{port}/').status == 200))\""
    );
    let answer = tool(&rig).answer(
        &json!({
            "roles": {
                // Declared first, started SECOND: the order is the graph's, not the object's.
                "client": {
                    "host": "fake1",
                    "command": client,
                    "after": ["web"]
                },
                "web": {
                    "host": "fake1",
                    "command": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "ready": {"port": port}
                }
            },
            "deadline_s": 60
        }),
        here(&rig),
        "scenario-after",
    );
    let text = answer.payload.clone();
    let lines = rows(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].starts_with("web on fake1"),
        "`web` was started first because `client` is after it:\n{text}"
    );
    assert!(
        lines[1].starts_with("client on fake1"),
        "and `client` second:\n{text}"
    );
    // **Both facts resolved**: the client's own line shows the peer's name, and it reached the
    // server. A `${…}` left unsubstituted is a bash `bad substitution` and an `exited 1` row.
    assert!(
        lines[1].contains("peer=fake1")
            && lines[1].contains("ok=True")
            && lines[1].contains("exited 0"),
        "`${{web.host}}` and `${{web.port}}` were substituted:\n{text}"
    );
    assert!(!listening(port), "the service was torn down");
}

/// **Every role and the run are supervised records** — a `process` row with its host, so a session
/// close or a daemon restart has the same list. Measured here because it is the contract between
/// the tool and the daemon's supervisor.
#[test]
fn every_role_is_written_down_before_it_is_ready() {
    use letibot_tools::fleet::{ProcessBook, ProcessNote};

    #[derive(Default)]
    struct Book {
        started: std::sync::Mutex<Vec<ProcessNote>>,
        ended: std::sync::Mutex<usize>,
    }
    impl ProcessBook for Book {
        fn started(&self, p: &ProcessNote) {
            self.started.lock().expect("book").push(p.clone());
        }
        fn ended(&self, _h: &str, _s: letibot_tokencore::store::ProcState, _e: Option<i32>) {
            *self.ended.lock().expect("book") += 1;
        }
    }
    let rig = Rig::new("records", &["fake1"]);
    let port = free_port();
    let book = Arc::new(Book::default());
    let answer = Scenario::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
        .with_book(book.clone())
        .answer(
            &json!({
                "roles": {
                    "web": {
                        "host": "fake1",
                        "command": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                        "ready": {"port": port}
                    }
                },
                "run": "true",
                "deadline_s": 60
            }),
            here(&rig),
            "scenario-records",
        );
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{}", answer.payload);
    let started = book.started.lock().expect("book").clone();
    assert_eq!(
        started.len(),
        2,
        "a record for the role and one for the run: {started:?}"
    );
    let web = started
        .iter()
        .find(|p| p.command.contains("http.server"))
        .unwrap_or_else(|| panic!("the role's record is there: {started:?}"));
    assert_eq!(web.host.as_deref(), Some("fake1"));
    assert!(web.identity.starts_with("cmd:"));
    assert!(
        web.log.as_deref().is_some_and(|l| l.ends_with(".out")),
        "the record names the file the supervisor reads: {web:?}"
    );
    // The run's own record has no host, because it ran as `local`.
    assert!(
        started.iter().any(|p| p.host.is_none()),
        "the run is a record too, and a local one: {started:?}"
    );
    assert_eq!(*book.ended.lock().expect("book"), 2);
}

/// **The other two readiness checks** — a line in the role's own output, and a command that exits 0
/// — because the port is only one of the three spellings a fixture has, and the other two read
/// different things: the job's output file, and a probe job of its own.
#[test]
fn a_log_line_and_a_checking_command_both_answer_as_readiness() {
    let rig = Rig::new("checks", &["fake1"]);
    let answer = tool(&rig).answer(
        &json!({
            "roles": {
                "logger": {
                    "host": "fake1",
                    "command": "echo starting; sleep 0.2; echo 'listening on 9999'; sleep 60",
                    "ready": {"log": "listening on \\d+"}
                },
                "checked": {
                    "host": "fake1",
                    "command": "sleep 60",
                    "ready": {"check": "true"},
                    "ready_deadline_s": 10
                }
            },
            "run": "true",
            "deadline_s": 60
        }),
        here(&rig),
        "scenario-checks",
    );
    let text = answer.payload.clone();
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{text}");
    assert!(
        text.contains("logger on fake1 · stopped · ") && text.contains("ready after "),
        "the log line made it ready:\n{text}"
    );
    assert!(
        text.contains("checked on fake1 · stopped · ") && text.contains("ready after "),
        "the checking command made it ready:\n{text}"
    );
    assert!(text.contains("run (local) · exited 0"), "{text}");
}

/// **The tool is one gated call whose arguments name every host and every command** — the card's
/// whole content, asserted on the schema because that is what the gate is handed.
#[test]
fn the_tool_is_one_gated_call_that_names_its_hosts() {
    let schema = letibot_tools::runtime::Tool::schema(&Scenario::new());
    assert_eq!(schema.name, "scenario");
    assert_eq!(schema.access, letibot_tools::schema::Access::Exec);
    let props = &schema.parameters["properties"];
    assert!(props.get("roles").is_some());
    assert!(props.get("run").is_some());
    assert!(schema.description.contains("tear"));
}
