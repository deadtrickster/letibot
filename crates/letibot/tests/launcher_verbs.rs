//! `letibot-multi --verbs` is the one list of what `letibot <word>` answers, and the launcher reads
//! it: every word on it reaches the multicall, and a word the launcher has no arm for is refused by
//! name rather than sent as a prompt.

use std::path::{Path, PathBuf};

fn scratch(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-launcher-verbs-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// What the multicall itself says its verbs are: the list the launcher and this test both read.
fn the_real_list() -> Vec<String> {
    let out = letibot_testenv::command(env!("CARGO_BIN_EXE_letibot-multi"))
        .arg("--verbs")
        .output()
        .expect("the multicall runs");
    assert!(out.status.success(), "`--verbs` exited {:?}", out.status);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// A launcher fixture: a `letibot-multi` that answers `--verbs` from `list`, a `harnessd` that
/// records its argv, and the directories the launcher insists on.
struct Fixture {
    dir: PathBuf,
    calls: PathBuf,
}

impl Fixture {
    fn new(tag: &str, list: &[String]) -> Fixture {
        let dir = scratch(tag);
        let bin = dir.join("bin");
        for d in [&bin, &dir.join("home"), &dir.join("run"), &dir.join("ws")] {
            std::fs::create_dir_all(d).expect("a fixture directory");
        }
        std::fs::write(dir.join("verbs"), list.join("\n") + "\n").expect("the list");
        let multi = bin.join("letibot-multi");
        std::fs::write(
            &multi,
            format!(
                "#!/bin/sh\n\
                 if [ \"${{1:-}}\" = --verbs ]; then exec cat '{}'; fi\n\
                 if [ \"${{1:-}}\" = where ] && [ \"${{3:-}}\" = --json ]; then\n\
                 \x20 printf '%s\\n' '{{\"folder\":\"/x\",\"live\":false,\"project\":\"x\",\"protocol\":null,\"socket\":\"/x/x.sock\",\"this_build\":1}}'\n\
                 \x20 exit 0\n\
                 fi\n\
                 printf 'letibot-multi'; printf ' %s' \"$@\"; printf '\\n'\n",
                dir.join("verbs").display()
            ),
        )
        .expect("the multicall stub");
        let calls = dir.join("calls");
        std::fs::write(
            bin.join("harnessd"),
            format!(
                "#!/bin/sh\nprintf 'harnessd %s\\n' \"$*\" >> '{}'\nprintf 'harnessd'; printf ' %s' \"$@\"; printf '\\n'\n",
                calls.display()
            ),
        )
        .expect("the daemon stub");
        for f in ["letibot-multi", "harnessd"] {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(bin.join(f), std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        Fixture { dir, calls }
    }

    fn run(&self, args: &[&str]) -> String {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/letibot")
            .canonicalize()
            .expect("the launcher script is in this repository");
        let mut cmd = letibot_testenv::command("bash");
        cmd.arg(&script)
            .args(args)
            .current_dir(self.dir.join("ws"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.join("home"))
            .env("XDG_RUNTIME_DIR", self.dir.join("run"))
            .env("LETIBOT_BIN", self.dir.join("bin"))
            .env("LETIBOT_ENDPOINT", "")
            .env("LETIBOT_ROOM_SOCKET", self.dir.join("room/room.sock"));
        let out = cmd.output().expect("the launcher runs");
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// What the daemon stub was run with, if anything.
    fn daemon_calls(&self) -> String {
        std::fs::read_to_string(&self.calls).unwrap_or_default()
    }
}

#[test]
fn every_verb_the_multicall_names_is_routed_and_never_a_prompt() {
    let list = the_real_list();
    assert!(list.contains(&"hold".to_string()), "{list:?}");
    assert!(list.contains(&"where".to_string()), "{list:?}");
    let f = Fixture::new("verbs", &list);
    for word in &list {
        let out = f.run(&[word.as_str()]);
        assert!(
            !out.contains("--prompt"),
            "`{word}` was sent as a prompt: {out}"
        );
        assert!(
            !out.contains("no such role"),
            "`{word}` is a verb of the multicall and the launcher does not route it: {out}"
        );
    }
    // The words whose arm is a straight pass-through reach the multicall with their own word;
    // the rest are the launcher's own checks, which refuse a missing argument by name.
    for word in [
        "hold", "holds", "gc", "release", "stats", "boundary", "fb", "where",
    ] {
        let out = f.run(&[word, "s-1"]);
        assert!(
            out.contains(&format!("letibot-multi {word}")),
            "`{word}` did not reach the multicall: {out}"
        );
    }
    assert!(
        !f.daemon_calls().contains("--prompt"),
        "a verb became a turn: {}",
        f.daemon_calls()
    );
}

/// The drift this list exists for: a verb the multicall grew and the launcher has no arm for. It is
/// refused by name, because a prompt is billed and the model would be asked to run it.
#[test]
fn a_verb_the_launcher_has_no_arm_for_is_refused_not_sent_as_a_prompt() {
    let mut list = the_real_list();
    list.push("frobnicate".into());
    let f = Fixture::new("no-arm", &list);
    let out = f.run(&["frobnicate"]);
    assert!(
        out.contains("has no arm for it"),
        "the refusal does not name the gap: {out}"
    );
    assert!(!out.contains("--prompt"), "{out}");
    assert_eq!(f.daemon_calls(), "", "nothing was run: {out}");
}

/// A word that is not a verb stays what it was: a prompt to the folder's daemon.
#[test]
fn a_word_that_is_not_a_verb_is_still_a_prompt() {
    let list = the_real_list();
    let f = Fixture::new("prompt", &list);
    f.run(&["fix the failing test"]);
    assert!(
        f.daemon_calls().contains("--prompt"),
        "the one-shot is not a prompt any more: {}",
        f.daemon_calls()
    );
}
