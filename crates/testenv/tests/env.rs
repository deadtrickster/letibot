//! The helpers, run: a spawned child sees no door, and the guard puts back what it changed.

use letibot_testenv::{EnvGuard, command, isolate};

fn child_env(cmd: &mut std::process::Command) -> String {
    let out = cmd.output().expect("env runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_command_carries_no_door_but_keeps_the_apparatus_and_what_the_test_sets() {
    let mut g = EnvGuard::lock();
    g.set("LETIBOT_SOCKET", "/operator/live.sock")
        .set("LETIBOT_SESSION", "s-live")
        .set("LETIBOT_VOCAB_GGUF", "/models/v.gguf");
    let said = child_env(&mut command("env"));
    assert!(!said.contains("/operator/live.sock"), "{said}");
    assert!(!said.contains("LETIBOT_SESSION="), "{said}");
    assert!(said.contains("LETIBOT_VOCAB_GGUF=/models/v.gguf"), "{said}");
    let said = child_env(command("env").env("LETIBOT_SOCKET", "/test/own.sock"));
    assert!(said.contains("LETIBOT_SOCKET=/test/own.sock"), "{said}");
}

#[test]
fn the_guard_puts_back_what_it_changed() {
    {
        let _g = EnvGuard::lock();
        // SAFETY: under the guard.
        unsafe {
            std::env::remove_var("TESTENV_A");
            std::env::set_var("TESTENV_B", "before");
        }
    }
    {
        let mut g = EnvGuard::lock();
        g.set("TESTENV_A", "x").set("TESTENV_B", "y");
        g.set("TESTENV_B", "z").remove("TESTENV_B");
    }
    let _g = EnvGuard::lock();
    assert_eq!(std::env::var_os("TESTENV_A"), None);
    assert_eq!(std::env::var("TESTENV_B").ok().as_deref(), Some("before"));
}

#[test]
fn isolate_takes_the_doors_off_and_moves_the_xdg_dirs() {
    {
        let _g = EnvGuard::lock();
        // SAFETY: under the guard. Set outside it, so nothing puts it back after `isolate`.
        unsafe { std::env::set_var("LETIBOT_PROVIDERS", "/operator/providers.toml") };
    }
    let dir = isolate();
    let _g = EnvGuard::lock();
    assert_eq!(std::env::var_os("LETIBOT_PROVIDERS"), None);
    let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from);
    assert_eq!(config, Some(dir.join("config")));
}
