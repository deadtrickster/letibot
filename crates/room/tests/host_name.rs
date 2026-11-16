//! The room's name for this box, from both sides that derive it: `harnessd` in Rust and
//! `scripts/letibot` in bash. A launcher that named another socket than the coordinator binds
//! starts a second coordinator, and a name that moves with the network does the same every move.

use std::path::Path;

/// The launcher's own two functions, run in bash: no copy of them in the test.
fn launcher_key() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/letibot");
    let text = std::fs::read_to_string(&script).expect("the launcher is in this repository");
    let function = |name: &str| -> String {
        let start = text
            .find(&format!("\n{name}() {{"))
            .unwrap_or_else(|| panic!("`{name}` is not defined in the launcher"));
        let body = &text[start + 1..];
        let end = body.find("\n}\n").expect("the function ends") + 3;
        body[..end].to_string()
    };
    let program = format!(
        "{}{}room_host_key \"$(room_host_name)\"",
        function("room_host_name"),
        function("room_host_key")
    );
    let out = std::process::Command::new("bash")
        .args(["-c", &program])
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_launcher_and_the_coordinator_name_the_same_room() {
    assert_eq!(launcher_key(), letibot_room::lease::host_key());
}

/// A Mac's `uname -n` is whatever DHCP handed it last; the room follows the local host name the
/// operator set, which the network does not change.
#[cfg(target_os = "macos")]
#[test]
fn a_macs_room_is_named_by_its_local_host_name_not_by_dhcp() {
    let local = std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .expect("scutil runs");
    let local = String::from_utf8_lossy(&local.stdout).trim().to_string();
    assert!(!local.is_empty(), "this Mac has a local host name");
    assert_eq!(letibot_room::lease::host_name(), local);
}
