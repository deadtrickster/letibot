//! A stdio server's environment and lifetime. Its own binary: the test sets a variable on this
//! process, which no other test may observe.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use letibot_mcp_client::{Catalog, ServerSpec, Transport};
use letibot_tools::builtins::external::mcp::McpCatalog;
use serde_json::json;

fn fake_bin() -> String {
    if let Some(p) = option_env!("CARGO_BIN_EXE_letibot-mcp-fake") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(|d| d.parent()).unwrap();
    debug.join("letibot-mcp-fake").display().to_string()
}

/// Whether `pid` is a live process: present in `/proc` and not a zombie waiting for its reaper.
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

#[test]
fn stdio_spawn_does_not_leak_env_or_orphan() {
    // SAFETY: the only test in this binary, set before any server or thread starts.
    unsafe { std::env::set_var("LETIBOT_TEST_PROVIDER_KEY", "sk-secret") };
    let spec = ServerSpec {
        name: "fake".into(),
        transport: Transport::Stdio {
            command: fake_bin(),
            args: Vec::new(),
            env: BTreeMap::from([("GIVEN".to_string(), "yes".to_string())]),
            cwd: None,
        },
        timeout: Duration::from_secs(20),
    };
    let cat = Catalog::start(&[spec]);
    let env = |name: &str| {
        cat.call("fake", "env", &json!({"name": name}))
            .unwrap()
            .text
    };
    assert_eq!(
        env("LETIBOT_TEST_PROVIDER_KEY"),
        "",
        "the daemon's env leaked"
    );
    assert_eq!(env("GIVEN"), "yes", "the spec's env is handed over");
    assert!(
        !env("PATH").is_empty(),
        "PATH is kept so a bare command resolves"
    );

    let pid: u32 = cat
        .call("fake", "spawn", &json!({}))
        .unwrap()
        .text
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid), "the grandchild started");
    drop(cat);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "grandchild {pid} outlived the server's drop");
}
