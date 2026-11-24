//! A bridge whose executable was replaced by an install says so when asked to start or attach,
//! instead of driving newer daemons with an old build. This test replaces its own executable.

use std::path::PathBuf;

use letibot_bridge::tools::Bridge;

/// The running executable moved aside and a new file put at its path, as an install does.
/// Put back on drop, so a failed assertion leaves the build as it was.
struct Replaced {
    exe: PathBuf,
    aside: PathBuf,
}

impl Replaced {
    fn now() -> Replaced {
        let exe = std::env::current_exe().expect("this test's executable");
        let aside = exe.with_extension("running");
        std::fs::rename(&exe, &aside).expect("move the running file aside");
        std::fs::write(&exe, b"a newer build").expect("a new file at the path");
        Replaced { exe, aside }
    }
}

impl Drop for Replaced {
    fn drop(&mut self) {
        let _ = std::fs::rename(&self.aside, &self.exe);
    }
}

#[test]
fn a_stale_bridge_binary_says_so_at_attach() {
    let scratch = letibot_sessionlog::testing::scratch_root()
        .join(format!("lb-stale-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("bin")).expect("scratch");
    // SAFETY: one test in this binary, set before any thread reads the environment. No daemon
    // binary and no store of the operator's can be reached from here.
    unsafe {
        std::env::set_var("LETIBOT_BIN", scratch.join("bin"));
        std::env::set_var("LETIBOT_STORE", scratch.join("store"));
        std::env::set_var("XDG_DATA_HOME", scratch.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", scratch.join("run"));
        std::env::remove_var("LETIBOT_SOCKET");
    }
    let bridge = Bridge::new();
    let _replaced = Replaced::now();
    let folder = scratch.display().to_string();

    let attach = bridge
        .attach(&folder, "s-0123456789abcdef", true)
        .expect_err("a stale bridge does not attach");
    assert!(
        attach.contains("older than the build installed") && attach.contains("Restart"),
        "the refusal says the build is stale and to restart: {attach}"
    );
    let start = bridge
        .start(
            &folder,
            "a brief",
            None,
            "writes-allowed",
            None,
            Some(false),
            None,
        )
        .expect_err("a stale bridge does not start a session");
    assert!(start.contains("older than the build installed"), "{start}");
    let _ = std::fs::remove_dir_all(&scratch);
}
