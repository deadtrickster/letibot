//! A running catalogue moved to a new config: only what changed is restarted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use letibot_mcp_client::{Catalog, Did, Pool, ServerSpec, State, Transport};
use letibot_tools::builtins::external::mcp::McpCatalog;

fn fake_bin() -> String {
    if let Some(p) = option_env!("CARGO_BIN_EXE_letibot-mcp-fake") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(|d| d.parent()).unwrap();
    debug.join("letibot-mcp-fake").display().to_string()
}

fn spec(name: &str, command: &str, args: &[&str]) -> ServerSpec {
    ServerSpec {
        name: name.into(),
        transport: Transport::Stdio {
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout: Duration::from_secs(20),
    }
}

fn fake(name: &str, args: &[&str]) -> ServerSpec {
    spec(name, &fake_bin(), args)
}

fn pid(cat: &Catalog, name: &str) -> u32 {
    let s = cat.status().into_iter().find(|s| s.name == name).unwrap();
    assert_eq!(s.state, State::Running, "{s:?}");
    s.pid.unwrap()
}

fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn said(cat: &Catalog, name: &str) -> String {
    cat.call(name, "pid", &serde_json::json!({})).unwrap().text
}

#[test]
fn a_changed_server_restarts_an_added_one_starts_and_a_removed_one_stops() {
    let cat = Catalog::start(&[fake("a", &[]), fake("b", &[]), fake("c", &[])]);
    let (a, b, c) = (pid(&cat, "a"), pid(&cat, "b"), pid(&cat, "c"));
    assert_eq!(said(&cat, "a"), a.to_string());
    let changes = cat.reconcile(
        &[fake("a", &["--tag=2"]), fake("b", &[]), fake("d", &[])],
        Vec::new(),
    );
    let lines: Vec<String> = changes.iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "a restarted (config changed)",
            "c stopped (removed)",
            "d added"
        ]
    );
    assert_ne!(pid(&cat, "a"), a, "a is a new process");
    assert_eq!(pid(&cat, "b"), b, "b was left running");
    assert!(
        !alive(a) && !alive(c),
        "the old a and the removed c are gone"
    );
    assert!(alive(pid(&cat, "d")));
    assert!(cat.servers().iter().all(|s| s.name != "c"));
    assert!(
        cat.reconcile(
            &[fake("a", &["--tag=2"]), fake("b", &[]), fake("d", &[])],
            Vec::new()
        )
        .is_empty()
    );
}

#[test]
fn a_call_in_flight_finishes_on_the_process_it_started_on() {
    let cat = Arc::new(Catalog::start(&[fake("slow", &[])]));
    let old = pid(&cat, "slow");
    let caller = {
        let cat = cat.clone();
        std::thread::spawn(move || {
            cat.call("slow", "pid", &serde_json::json!({"ms": 1500}))
                .unwrap()
                .text
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    let changes = cat.reconcile(&[fake("slow", &["--tag=new"])], Vec::new());
    assert_eq!(changes[0].did, Did::Restarted("config changed"));
    assert_eq!(caller.join().unwrap(), old.to_string());
    assert_ne!(said(&cat, "slow"), old.to_string());
    assert!(!alive(old), "the old process ends with its last call");
}

#[test]
fn a_restart_by_name_is_a_new_process() {
    let cat = Catalog::start(&[fake("one", &[]), fake("two", &[])]);
    let (one, two) = (pid(&cat, "one"), pid(&cat, "two"));
    let c = cat.restart("one", "by hand").unwrap();
    assert_eq!(c.to_string(), "one restarted (by hand)");
    assert_ne!(pid(&cat, "one"), one);
    assert_eq!(pid(&cat, "two"), two);
    assert!(cat.restart("nobody", "by hand").is_none());
}

#[test]
fn a_rebuilt_binary_is_stale_until_restarted() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-stale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("srv");
    std::fs::copy(fake_bin(), &bin).unwrap();
    let cat = Catalog::start(&[spec("srv", bin.to_str().unwrap(), &[])]);
    assert!(cat.stale().is_empty());
    std::thread::sleep(Duration::from_millis(20));
    // A new file at the path, as an install does: the running process keeps the old inode.
    let next = dir.join("srv.new");
    std::fs::copy(fake_bin(), &next).unwrap();
    std::fs::rename(&next, &bin).unwrap();
    assert_eq!(cat.stale(), ["srv"]);
    cat.restart("srv", "binary updated").unwrap();
    assert!(cat.stale().is_empty());
}

#[test]
fn a_reload_retries_a_server_that_failed() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-retry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("late");
    let _ = std::fs::remove_file(&bin);
    let specs = [spec("late", bin.to_str().unwrap(), &[])];
    let cat = Catalog::start(&specs);
    assert!(
        matches!(cat.status()[0].state, State::Failed(_)),
        "{:?}",
        cat.status()
    );
    std::fs::copy(fake_bin(), &bin).unwrap();
    // A file just written can be busy for exec while another test's fork still holds it.
    let mut changes = cat.reconcile(&specs, Vec::new());
    for _ in 0..20 {
        if !changes[0]
            .failed
            .as_deref()
            .is_some_and(|w| w.contains("busy"))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        changes = cat.reconcile(&specs, Vec::new());
    }
    assert_eq!(changes[0].to_string(), "late restarted (retried)");
    assert_eq!(cat.status()[0].state, State::Running);
}

#[test]
fn two_catalogues_share_one_process_and_a_reload_of_one_leaves_the_other() {
    let pool = Pool::new();
    let (one, _) = pool.catalog(&[fake("shared", &["--tag=pool"])], Vec::new());
    let (two, _) = pool.catalog(&[fake("shared", &["--tag=pool"])], Vec::new());
    let p = pid(&one, "shared");
    assert_eq!(pid(&two, "shared"), p, "one process for one spec");
    one.reconcile(&[], Vec::new());
    assert!(alive(p), "the other catalogue still holds it");
    assert_eq!(said(&two, "shared"), p.to_string());
    two.reconcile(&[], Vec::new());
    assert!(!alive(p), "held by none, it is stopped");
}
