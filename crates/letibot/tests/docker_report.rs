//! `letibot gc --docker-report` (`docs/lifecycle.md` d.7 and G6): other projects' images and
//! volumes, grouped by compose project and age, and nothing removed.
//!
//! The verb is driven as a person runs it - the real binary, a clean environment, and a stub engine
//! on a unix socket - because the claim is about what the PROCESS prints and does not do, and this
//! box has no docker to point it at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tools::testing::docker::StubEngine;

/// A directory of its own per test, removed when it ends.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "letibot-docker-report-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a fixture root");
        Fixture { root }
    }

    fn at(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).expect("a fixture directory");
        p
    }

    /// The verb, run the way a person's shell runs it: nothing in the environment names the
    /// operator's own store, cache, runtime directory or engine.
    fn run(&self, args: &[&str], settings: Option<&Path>) -> (bool, String, String) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_letibot"));
        cmd.args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.at("home"))
            .env("XDG_DATA_HOME", self.at("home/.local/share"))
            .env("XDG_CACHE_HOME", self.at("cache"))
            .env("XDG_RUNTIME_DIR", self.at("run"))
            .env("LETIBOT_STORE", self.at("data"))
            .env("LETIBOT_GC_ROOT", self.at("gc"));
        if let Some(path) = settings {
            cmd.env("LETIBOT_GC_SETTINGS", path);
        }
        let out = cmd.output().expect("the verb runs");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// **Done when** `--docker-report` prints unowned images and volumes by compose project and age and
/// removes nothing.
#[test]
fn the_report_names_debris_by_project_and_age_and_removes_nothing() {
    let f = Fixture::new("debris");
    let engine = StubEngine::start(&f.at("engine"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    engine.seed_image(
        "bench/api:1",
        labels(&[("com.docker.compose.project", "bench")]),
        3 * 1024 * 1024 * 1024,
        now - 4 * 86_400,
    );
    engine.seed_volume(
        "bench_pgdata",
        labels(&[("com.docker.compose.project", "bench")]),
        2 * 1024 * 1024,
    );
    // One of letibot's own: the report is about what letibot cannot prove it owns.
    engine.seed_volume(
        "lb_data",
        labels(&[("letibot.owner", "s-1794555261791226969")]),
        1024,
    );

    let settings = f.at("home/.config/letibot").join("leticode.toml");
    std::fs::write(
        &settings,
        format!("[docker]\nengine = \"{}\"\n", engine.socket().display()),
    )
    .expect("a settings file");

    let (ok, out, err) = f.run(&["gc", "--docker-report"], Some(&settings));
    assert!(ok, "`letibot gc --docker-report` failed. stderr:\n{err}");
    assert!(out.contains("compose project"), "{out}");
    assert!(out.contains("bench"), "the project is named: {out}");
    assert!(out.contains("newest 4d"), "its age is named: {out}");
    assert!(
        out.contains("Nothing here is removed"),
        "it says what it will not do: {out}"
    );
    assert!(
        out.contains("docker image prune -a --force")
            && out.contains("com.docker.compose.project=bench"),
        "it prints the commands and runs none: {out}"
    );
    assert!(
        !out.contains("lb_data"),
        "letibot's own volume is not the report's subject: {out}"
    );
    assert_eq!(
        engine.remaining(),
        (0, 0, 2),
        "nothing was removed, by either side"
    );
    assert!(
        !engine.requests().iter().any(|r| r.path().contains("prune")),
        "and no prune was asked for"
    );

    // `--json` is the same facts for a program.
    let (ok, out, err) = f.run(&["gc", "--docker-report", "--json"], Some(&settings));
    assert!(ok, "`--json`: {err}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("one JSON object");
    assert_eq!(parsed["rows"].as_array().map(|r| r.len()), Some(2));
}

/// A box with no engine says so, and says that it removed nothing - rather than failing.
#[test]
fn a_box_with_no_engine_says_so_instead_of_failing() {
    let f = Fixture::new("noengine");
    // The settings name an engine that is not there, so the test cannot reach the socket this box
    // may really have: a test never talks to a real engine.
    let settings = f.at("home/.config/letibot").join("leticode.toml");
    std::fs::write(
        &settings,
        format!(
            "[docker]\nengine = \"{}\"\n",
            f.at("engine").join("missing.sock").display()
        ),
    )
    .expect("a settings file");
    let (ok, out, err) = f.run(&["gc", "--docker-report"], Some(&settings));
    assert!(ok, "a box without docker is not a failure: {err}");
    assert!(
        out.contains("no docker engine on this box"),
        "it names the fact: {out}"
    );
    assert!(out.contains("nothing removed"), "{out}");
}
