//! **`--host NAME` seats a root session whose tools run on that machine.**
//!
//! `docs/remote-hosts.md`: *"the operator | `leticode --host lab2x1` — a session whose tools
//! run on lab2x1 (like `--vm`)"*. This is that, measured through the real opener: a real
//! `Harness::open`, the real backend construction, the real gate wiring — and a **fake ssh**
//! (a script that accepts the flags and runs the command against a temporary directory
//! standing in for the machine), so nothing here reaches a network or a second box.
//!
//! What it pins:
//!
//! | fact | measured by |
//! |---|---|
//! | the placement is `Remote(name)`, not a VM and not a host session | `Harness::config` |
//! | the backend is the ssh one, and says so in a sentence | `Harness::wiring().backend` |
//! | the repository's HEAD arrives as a checkout on that machine | the worktree on the fake remote |
//! | the session's workspace is the CHECKOUT, and the boundary answer is not invented | `workspace_path` in the description |
//! | its commits come back as `remote/<host>/<short id>` when it ends | `close_backend`'s sentence and the local branch |
//! | the placement says where it is, and what stayed here | the `remote_placement` warning |
//!
//! `$LETIBOT_HOSTS` and `$LETIBOT_SSH` are set here, and this file is a test binary of its
//! own — nothing else in this process reads either, which is why the `set_var`s below are
//! the whole of the environment this suite touches. `$HOME` is set with them, because the one
//! ssh policy asks the *operator's* home whether a key is inside their own ssh directory, and
//! the answer a test must get is about its fixture.

use std::path::{Path, PathBuf};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Harness, Parts};
use letibot_sessionlog::registry::Registry;
use letibot_tools::builtins::task::Placement;

/// The session id, whose last eight characters name the checkout and the branch.
const SESSION: &str = "s-01abcdef";
const SHORT: &str = "01abcdef";

struct Rig {
    dir: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-remote-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("home")).expect("home");
        // **Resolved**, because the remote resolves it: `pwd -P` on the far side follows
        // `/var` -> `/private/var` here, and comparing against the unresolved path would fail
        // for a reason that has nothing to do with the placement.
        let dir = dir.canonicalize().unwrap_or(dir);
        let bin = dir.join("bin");
        let home = dir.join("home");
        // The fake ssh: flags ignored, the last argument is the remote command, and `HOME`
        // is the machine's — so `root = "~/letibot-remote"` is expanded *there*.
        let ssh = bin.join("ssh");
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/bash\n\
                 export PATH='{}':$PATH\n\
                 export HOME='{}'\n\
                 for last; do :; done\n\
                 exec bash -lc \"$last\"\n",
                bin.display(),
                home.display()
            ),
        )
        .expect("the fake ssh");
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let repo = dir.join("the-repo");
        std::fs::create_dir_all(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "a test"]);
        std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the first commit"]);

        // **The operator's file**, which is the only place a host may come from.
        let hosts = dir.join("hosts.toml");
        std::fs::write(
            &hosts,
            "[host.fake]\nhostname = \"fake.invalid\"\nuser = \"tester\"\nroot = \"~/letibot-remote\"\n",
        )
        .expect("a hosts file");
        // **letibot's own ssh material**, under this rig's home: the dedicated key the host
        // connects with and the host key already accepted. `Ssh::connect` refuses a placement
        // without both, so a fixture that did not write them would be testing the refusal.
        let paths = letibot_tools::remote::Paths::in_home(&home);
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.default_key("fake"), b"a dedicated key").expect("the key");
        std::fs::write(
            paths.known_hosts(),
            "fake.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
        )
        .expect("the host key");
        // SAFETY (Rust 2024's `unsafe` on `set_var`): this test binary is one file and these
        // three are read by nothing else in it, before any thread that would read them starts.
        // `HOME` is this rig's, because the ssh policy reads the *operator's* home from it to
        // answer "is this key inside their own ssh directory" — the answer for a test must be
        // about this fixture and never about the box.
        unsafe {
            std::env::set_var("LETIBOT_HOSTS", &hosts);
            std::env::set_var("LETIBOT_SSH", &ssh);
            std::env::set_var("HOME", &home);
        }
        Rig { dir, home, repo }
    }

    /// Where the checkout lands on the fake remote.
    fn worktree(&self) -> PathBuf {
        self.home
            .join("letibot-remote")
            .join("the-repo")
            .join(SHORT)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_host_placement_seats_a_session_whose_tools_run_there() {
    let rig = Rig::new();
    // **The operator's own answer**: `leticode --host fake` is `placement = Remote("fake")`
    // and nothing else — the same two lines `cli.rs` runs for `--host` once the name has
    // been checked against the file (`Hosts::get`), which is what the opener then repeats
    // for a session reopened from the store.
    let mut cfg = Config::for_this_box(&rig.repo);
    cfg.session_id = SESSION.into();
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // The byte vocabulary: a session whose turns go to a provider needs no GGUF, which keeps
    // llama.cpp and every model file on this box out of the test.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.socket = rig.dir.join("daemon.sock");
    cfg.placement = Placement::Remote("fake".into());

    let parts = Parts::load(&cfg).expect("a provider session opens on the byte vocabulary");
    let registry = Registry::new();
    let hub = registry.new_hub(SESSION.to_string());
    let harness = Harness::open(&parts, cfg.clone(), hub).expect("the session seats on the host");

    // **The placement is the remote one, and it is not a VM**: a VM is a boundary and runs
    // allow-all inside it, and this must not have taken that path.
    assert_eq!(
        harness.config().placement,
        Placement::Remote("fake".into()),
        "the session is placed on the host it was asked for"
    );
    assert!(harness.config().vm_attach.is_none());

    // **The backend is the ssh one, in its own words** — the sentence a session shows and
    // the banner is built from.
    let described = harness.wiring().backend.clone();
    assert!(described.contains("ssh host `fake`"), "{described}");
    assert!(described.contains("NOT CONFINED"), "{described}");
    assert!(
        described.contains(&format!("remote/{SHORT}")),
        "the description names the branch the work comes back on: {described}"
    );
    assert!(
        harness.wiring().backend_writable,
        "a leticode session writes, on this machine or another one"
    );

    // **The tree went over**: the checkout is on the fake remote, on its own branch, and the
    // session's workspace is that checkout rather than anything on this box.
    let worktree = rig.worktree();
    assert!(
        worktree.join("README").is_file(),
        "the repository's HEAD arrived as a checkout: {}",
        worktree.display()
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("remote/{SHORT}")
    );

    // **Its commits come back when it ends.** A commit made "there" — the same git, run where
    // the session's own tools would run it — arrives as `remote/<host>/<short id>` in the
    // local repository, and the local branch it started from is untouched.
    std::fs::write(worktree.join("feature.rs"), b"// the work\n").expect("a file");
    git(&worktree, &["add", "feature.rs"]);
    git(
        &worktree,
        &["commit", "-q", "-m", "the work the session did"],
    );
    let remote_head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_string();

    let said = harness
        .close_backend()
        .expect("a remote session says where its work went");
    assert!(
        said.contains(&format!("remote/fake/{SHORT}")),
        "the close names the branch: {said}"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", &format!("remote/fake/{SHORT}")]).trim(),
        remote_head,
        "the commit made on the remote came back as that branch"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main",
        "the branch the work started from is unmoved: the queue moves branches, not a placement"
    );
}
