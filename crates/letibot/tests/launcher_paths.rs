//! The launcher and the daemon read one providers.toml: the path `scripts/letibot` derives in
//! bash is the one `letibot_paths` gives every Rust reader, under the same environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn scratch() -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-launcher-paths-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// The path `letibot --status` names when no local model has a home.
fn launcher_providers(dir: &Path, vars: &[(&str, &OsString)]) -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .expect("the launcher script is in this repository");
    let (bin, home, run, ws) = (
        dir.join("bin"),
        dir.join("home"),
        dir.join("run"),
        dir.join("ws"),
    );
    for d in [&bin, &home, &run, &ws] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    letibot_tools::testing::write_stub(&bin.join("harnessd"), "#!/bin/sh\nexit 1\n");
    // The launcher asks the multicall for this folder's socket.
    let multi = bin.join("letibot-multi");
    std::fs::copy(env!("CARGO_BIN_EXE_letibot-multi"), &multi).expect("the binary is copied");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&multi, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script)
        .arg("--status")
        .current_dir(&ws)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &run)
        .env("LETIBOT_BIN", &bin)
        .env("LETIBOT_ROOM_SOCKET", dir.join("room/room.sock"));
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the launcher runs");
    let text = String::from_utf8_lossy(&out.stdout);
    let at = text
        .lines()
        .find_map(|l| l.split_once("block in ").map(|(_, rest)| rest))
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_else(|| {
            panic!(
                "no providers path in `letibot --status`:\n{text}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    PathBuf::from(at)
}

#[test]
fn launcher_and_daemon_resolve_same_providers_file() {
    let dir = scratch();
    let home = OsString::from(dir.join("home"));
    let xdg = OsString::from(dir.join("xdg"));
    let alt = OsString::from(dir.join("alt/providers.toml"));
    let empty = OsString::new();
    type Case<'a> = (&'a str, Vec<(&'a str, &'a OsString)>, Option<&'a OsString>);
    let cases: [Case; 3] = [
        (
            "XDG_CONFIG_HOME",
            vec![("XDG_CONFIG_HOME", &xdg)],
            Some(&xdg),
        ),
        ("LETIBOT_PROVIDERS", vec![("LETIBOT_PROVIDERS", &alt)], None),
        (
            "empty XDG_CONFIG_HOME",
            vec![("XDG_CONFIG_HOME", &empty)],
            Some(&empty),
        ),
    ];
    for (name, vars, xdg_config_home) in cases {
        let env = letibot_paths::Env {
            xdg_config_home: xdg_config_home.cloned(),
            xdg_data_home: None,
            home: Some(home.clone()),
        };
        let daemon = letibot_paths::providers_file_in(&env).expect("a config dir");
        assert_eq!(launcher_providers(&dir, &vars), daemon, "{name}");
    }
}
