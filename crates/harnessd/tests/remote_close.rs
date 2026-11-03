//! A placed session closed by id - a head's `CloseSession`, and what the TUI and the bridge both
//! end in - settles its placement and says where the work went. The sentence is the one the
//! operator needs: the branch and its commit, or why neither came.

use std::path::{Path, PathBuf};

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::sessions::Sessions;
use letibot_sessionlog::registry::{Registry, SessionWiring};
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
            "letibot-close-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("home")).expect("home");
        // Resolved, because the remote resolves it: `pwd -P` on the far side turns `/var` into
        // `/private/var`, so comparing against the unresolved path fails for the wrong reason.
        let dir = dir.canonicalize().unwrap_or(dir);
        let bin = dir.join("bin");
        let home = dir.join("home");
        // The fake ssh: flags ignored, the last argument is the remote command, and `HOME` is the
        // machine's - so `root = "~/letibot-remote"` is expanded *there*.
        let ssh = bin.join("ssh");
        letibot_tools::testing::write_stub(
            &ssh,
            &format!(
                "#!/bin/bash\n\
                 export PATH='{}':$PATH\n\
                 export HOME='{}'\n\
                 for last; do :; done\n\
                 exec bash -lc \"$last\"\n",
                bin.display(),
                home.display()
            ),
        );
        let repo = dir.join("the-repo");
        std::fs::create_dir_all(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "a test"]);
        std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the first commit"]);

        // `mise = false`: this host is a directory on the box running the test, and the fake ssh
        // inherits this `PATH`, so the default's probe would find the real mise.
        let hosts = dir.join("hosts.toml");
        std::fs::write(
            &hosts,
            "[host.fake]\nhostname = \"fake.invalid\"\nuser = \"tester\"\n\
             root = \"~/letibot-remote\"\nmise = false\n",
        )
        .expect("a hosts file");
        let paths = letibot_tools::remote::Paths::in_home(&home);
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.default_key("fake"), b"a dedicated key").expect("the key");
        std::fs::write(
            paths.known_hosts(),
            "fake.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
        )
        .expect("the host key");
        // SAFETY: this binary holds one test and these are set before any thread reads them.
        unsafe {
            std::env::set_var("LETIBOT_HOSTS", &hosts);
            std::env::set_var("LETIBOT_SSH", &ssh);
            std::env::set_var("HOME", &home);
        }
        Rig {
            dir,
            home,
            repo,
        }
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
    let out = letibot_tools::testing::git(repo)
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

/// A session whose tools run on the fake host, on the byte vocabulary so no GGUF is needed.
fn config_for(rig: &Rig) -> Config {
    let mut cfg = Config::for_this_box(&rig.repo);
    cfg.session_id = SESSION.into();
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
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
    cfg
}

#[test]
fn a_closed_session_says_where_its_branch_went() {
    let rig = Rig::new();
    let cfg = config_for(&rig);
    let parts = Parts::load(&cfg).expect("a provider session opens on the byte vocabulary");
    let registry = Registry::new();
    registry
        .create(
            SESSION,
            "the package",
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: cfg.workspace.display().to_string(),
            },
        )
        .expect("the session is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // The work the package did: a commit made "there", by the same git the session's own tools
    // would run.
    let worktree = rig.worktree();
    std::fs::write(worktree.join("feature.rs"), b"// the work\n").expect("a file");
    git(&worktree, &["add", "feature.rs"]);
    git(
        &worktree,
        &["commit", "-q", "-m", "the work the session did"],
    );
    let remote_head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_string();
    let back = format!("remote/fake/{SHORT}");

    let said = sessions
        .close(SESSION)
        .expect("the session was held here, and its close says where the work went");
    assert!(
        said.contains(&back),
        "the close named the branch the work comes back on: {said}"
    );
    assert!(
        said.contains(&remote_head[..7]),
        "the close named the commit it landed at: {said}"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", &back]).trim(),
        remote_head,
        "the commit made on the host came back as `{back}`: {said}"
    );
    assert!(
        registry.get(SESSION).is_none(),
        "the session is closed here, and stays on disk for whoever opens it next"
    );
}
