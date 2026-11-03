//! The sweep's failure sentence, and the retry it must not claim.
//!
//! `Ssh::no_answer` was written for the calls that ARE asked again on a fresh connection. The
//! sweep runs through `Ssh::raw`, which asks exactly once, so a host that stopped answering was
//! told *"the call was asked once more with a fresh connection"* about a call that was asked once.
//!
//! A binary of its own: nothing here places a session, and a fake ssh that never answers is the
//! whole fixture.

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::remote::{Host, Mise, Paths, Ssh};

/// A fake ssh that runs nothing and never answers, and writes down every invocation it was
/// handed: the ledger is what proves how many times the call was asked.
struct Silent {
    dir: PathBuf,
    program: PathBuf,
    log: PathBuf,
}

impl Silent {
    fn new(tag: &str) -> Silent {
        let dir = std::env::temp_dir().join(format!(
            "letibot-sweep-sentence-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let program = bin.join("ssh");
        let log = dir.join("ssh.log");
        // `[ $# -eq 0 ] && exit 0` first: `write_stub` proves the file by exec'ing it with no
        // arguments, and that run has to be harmless.
        letibot_tools::testing::write_stub(
            &program,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 printf 'ARGV %s\\n' \"$*\" >> '{log}'\n\
                 sleep 30\n",
                log = log.display()
            ),
        );
        Silent { dir, program, log }
    }

    /// Every invocation, one line each: the argv ssh was handed.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("ARGV "))
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The ssh material a connection is refused without, and neither is the operator's.
fn paths_in(dir: &Path) -> Paths {
    let paths = Paths::in_home(dir.join("letibot-home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key("quiet"), b"a dedicated key").expect("the key");
    std::fs::write(
        paths.known_hosts(),
        "quiet.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host key");
    paths
}

/// The sweep is one call. A host that stops answering gets a sentence about what was done about
/// it, and what was done is nothing: `no_answer`'s "asked once more with a fresh connection" is
/// the retry rule of a call that HAS one.
#[test]
fn a_sweep_that_never_answered_does_not_claim_a_retry() {
    let fake = Silent::new("one-shot");
    let paths = paths_in(&fake.dir);
    let ssh = Ssh::connect(
        Host {
            name: "quiet".into(),
            hostname: "quiet.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        },
        Some(fake.program.clone()),
        paths,
    )
    .expect("the connection is arranged")
    .with_bound(Duration::from_secs(1))
    .with_mark("s-01abcdef");

    let said = ssh
        .end_session_processes()
        .expect("a sweep that never answered says so");
    assert!(
        !said.contains("asked once more"),
        "the sweep is asked exactly once, and the sentence claims a retry: {said}"
    );
    assert_eq!(
        fake.calls().len(),
        1,
        "and the ledger is the proof: one call, so nothing was asked again: {:?}",
        fake.calls()
    );
}
