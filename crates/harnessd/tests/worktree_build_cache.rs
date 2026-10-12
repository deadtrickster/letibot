//! **A `task_start` worktree is handed over exactly as its branch is, and the build cache is
//! shared through the child's environment.**
//!
//! The bug this pins: `start_worktree` wrote `<worktree>/.cargo/config.toml` (`[build] target-dir
//! = <main>/target`) and "hid" it by appending to `$(git rev-parse --git-dir)/info/exclude` — in
//! a worktree that is `.git/worktrees/<name>/info/exclude`, which git does not read. So every
//! worktree showed `?? .cargo/`, a child's `git add -A` committed it, and a repository that
//! TRACKS its own `.cargo/config.toml` had that file overwritten in the child's tree. The
//! operator: *".cargo/config.toml situation must be solved"*.
//!
//! What is required now:
//!
//! * the worktree `task_start` arranges has an empty `git status --porcelain`, and no `.cargo/`
//!   is written into it;
//! * a repository that tracks `.cargo/config.toml` keeps it byte for byte, in the child's tree
//!   and in the main one;
//! * the child's commands carry `CARGO_TARGET_DIR` = the main tree's `target/`
//!   (`harness::worktree_build_env`, through `Config::build_env` into the exec host's standing
//!   environment, which is what survives the host's `env_clear`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::worktree_build_env;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tools::NullToolSink;
use letibot_tools::builtins::task::{WorktreePlacement, worktree_branch, worktree_path};
use letibot_transcript::ToolCall;
use letibot_turn::Endpoint;

const PARENT: &str = "s-build-cache-parent";
/// A repository's own build config, as a project that tracks one would have it.
const TRACKED: &str = "[build]\nrustflags = [\"-Dwarnings\"]\ntarget-dir = \"own-target\"\n";

fn config(repo: &Path, session: &str) -> Config {
    let mut cfg = Config::for_this_box(repo);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // Nothing answers here: the child's first turn fails, which is after its worktree exists.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = None;
    cfg.session_id = session.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A scratch repository with one commit; with `tracked`, that commit holds a
/// `.cargo/config.toml` of the repository's own.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str, tracked: bool) -> Scratch {
        let root = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        // The worktrees live under the repository (`worktree_path`); the main tree ignores them
        // the way a project using `task_start` does.
        std::fs::write(repo.join(".gitignore"), ".claude/\ntarget/\n").unwrap();
        if tracked {
            std::fs::create_dir_all(repo.join(".cargo")).unwrap();
            std::fs::write(repo.join(".cargo/config.toml"), TRACKED).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        Scratch { root }
    }
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(self.repo())
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn call(h: &Harness, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
    let call = ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments: args.to_string(),
    };
    h.runtime_handle().invoke("t1", &call, &mut NullToolSink)
}

/// `task_start` in `repo`, from a parent seated there; the worktree's path once it exists.
fn start_a_child(parts: &Parts, cfg: &Config, slug: &str) -> (Harness, String) {
    let registry = Registry::new();
    let hub = registry.new_hub(PARENT.to_string());
    registry
        .adopt(
            hub.clone(),
            PARENT.to_string(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: cfg.workspace.display().to_string(),
            },
            None,
        )
        .expect("the parent is in the registry");
    let parent = Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry)
        .expect("the parent opens");
    let started = call(
        &parent,
        "task_start",
        serde_json::json!({"prompt": "build the thing", "slug": slug}),
    );
    assert!(
        matches!(
            started.outcome,
            letibot_transcript::ToolOutcome::Backgrounded { .. }
        ),
        "task_start did not start a child: {:?} {}",
        started.outcome,
        started.payload
    );
    let path = worktree_path(&cfg.workspace.display().to_string(), slug);
    assert!(Path::new(&path).is_dir(), "{path}");
    (parent, path)
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain"])
}

/// **The worktree is the branch and nothing else**: clean, no `.cargo/` written — and the child's
/// build-cache pair points at the main tree's `target/`.
#[test]
fn a_task_start_worktree_is_clean_and_has_no_cargo_dir_written() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = Scratch::new("build-cache-clean", false);
    let cfg = config(&dir.repo(), PARENT);
    let parts = parts_for(&cfg);
    let (parent, path) = start_a_child(&parts, &cfg, "clean-tree");
    let wt = Path::new(&path);
    assert_eq!(porcelain(wt), "", "the child is handed a dirty tree");
    assert!(
        !wt.join(".cargo").exists(),
        "a `.cargo/` was written into the worktree"
    );
    assert_eq!(porcelain(&dir.repo()), "", "the main tree is untouched");

    let (k, v) = worktree_build_env(&WorktreePlacement {
        path: path.clone(),
        branch: worktree_branch("clean-tree"),
        base_sha: String::new(),
        main_tree: false,
    })
    .expect("a worktree has a main tree to share the cache of");
    assert_eq!(k, "CARGO_TARGET_DIR");
    let v = Path::new(&v);
    assert_eq!(
        v.file_name().and_then(|n| n.to_str()),
        Some("target"),
        "{v:?}"
    );
    assert_eq!(
        v.parent().unwrap().canonicalize().unwrap(),
        dir.repo().canonicalize().unwrap(),
        "CARGO_TARGET_DIR is the MAIN tree's target/, not the worktree's: {v:?}"
    );
    // The main-tree exception IS the main tree: nothing to point anywhere.
    assert_eq!(
        worktree_build_env(&WorktreePlacement {
            path: dir.repo().display().to_string(),
            branch: "main".into(),
            base_sha: String::new(),
            main_tree: true,
        }),
        None
    );
    let _ = parent.stop_children();
    assert!(parent.wait_for_children(std::time::Duration::from_secs(30)));
}

/// **A repository that tracks its own `.cargo/config.toml` keeps it** — byte for byte in the
/// child's tree, and in the main tree, and neither status moves.
#[test]
fn a_tracked_cargo_config_is_left_as_the_repository_has_it() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = Scratch::new("build-cache-tracked", true);
    let cfg = config(&dir.repo(), PARENT);
    let parts = parts_for(&cfg);
    let (parent, path) = start_a_child(&parts, &cfg, "tracked-config");
    let wt = Path::new(&path);
    assert_eq!(
        std::fs::read_to_string(wt.join(".cargo/config.toml")).unwrap(),
        TRACKED,
        "the child's copy of the repository's own config was rewritten"
    );
    assert_eq!(porcelain(wt), "", "the child is handed a dirty tree");
    assert_eq!(
        std::fs::read_to_string(dir.repo().join(".cargo/config.toml")).unwrap(),
        TRACKED
    );
    assert_eq!(porcelain(&dir.repo()), "");
    let _ = parent.stop_children();
    assert!(parent.wait_for_children(std::time::Duration::from_secs(30)));
}

/// **The pair reaches the commands the session runs**: a harness opened with `build_env` (what
/// the spawn gives a worktree child) runs `bash` with `CARGO_TARGET_DIR` set, although the exec
/// host clears its environment before every command.
#[test]
fn the_childs_commands_carry_cargo_target_dir() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = Scratch::new("build-cache-env", false);
    let mut cfg = config(&dir.repo(), "s-build-cache-child");
    let want = dir.repo().join("target").display().to_string();
    cfg.build_env = vec![("CARGO_TARGET_DIR".into(), want.clone())];
    let parts = parts_for(&cfg);
    let registry: Arc<Registry> = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    let h = match Harness::open_with_registry(&parts, cfg.clone(), hub, None, None, registry) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("build-cache-env: no exec host on this box: {e}");
            return;
        }
    };
    let r = call(
        &h,
        "bash",
        serde_json::json!({"command": "printf 'target=[%s]' \"$CARGO_TARGET_DIR\""}),
    );
    let text = r.render();
    assert!(
        text.contains(&format!("target=[{want}]")),
        "the command did not carry CARGO_TARGET_DIR: {text}"
    );
}
