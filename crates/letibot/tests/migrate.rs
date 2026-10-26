//! **`letibot --migrate`, end to end, with a fake ssh and two fake remotes.**
//!
//! What this proves is the orchestration and every byte that crosses: the ssh argv (`-i`,
//! `IdentitiesOnly`, `BatchMode`, a ControlMaster per host, `-N -L` for the forward), the git
//! bundle both ways, the store export and import — run by the FAR SIDE's own binary, which here
//! is this build's binary behind a shim — the marker on both ends, and the record a head reads.
//!
//! What is NOT proved here, because it needs a real machine: a real `ssh -L` carrying a live
//! daemon's socket, a real `cargo build --release` on a host, and a head attached through the
//! forward. The fake `cargo` makes the two binaries the move expects, and the fake `harnessd`
//! records its argv instead of starting a daemon — no test may start a real one, and none may
//! need a network.
//!
//! The fake ssh runs each "remote" command against a temp home of its own, chosen by the ssh
//! target, so `lab2x1` and `lubuntu1` are two machines as far as every path in the move is
//! concerned.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::TranscriptItem;

const SESSION: &str = "s-1789919514688401228";
/// The last eight characters of [`SESSION`] — the branch and worktree suffix.
const SHORT: &str = "88401228";

/// One throwaway world: a laptop, two hosts, a store, a checkout and a fake ssh.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    store: PathBuf,
    checkout: PathBuf,
    hosts: PathBuf,
    ssh: PathBuf,
    ssh_log: PathBuf,
    remotes: PathBuf,
    run: PathBuf,
    src: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = std::env::temp_dir().join("letibot-rh4-tests").join(format!(
            "{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let run = Fixture::short_run(name);
        let _ = std::fs::remove_dir_all(&run);
        for dir in ["home", "bin", "tmp"] {
            std::fs::create_dir_all(root.join(dir)).expect("the fixture's dirs");
        }
        std::fs::create_dir_all(&run).expect("the fixture's runtime dir");
        let mut f = Fixture {
            home: root.join("home"),
            store: root.join("store/sessions.db"),
            checkout: root.join("home/checkout"),
            hosts: root.join("hosts.toml"),
            ssh: root.join("bin/ssh"),
            ssh_log: root.join("ssh.log"),
            remotes: root.join("remotes"),
            run,
            src: root.join("fake-letibot-src"),
            root,
        };
        std::fs::create_dir_all(f.store.parent().unwrap()).unwrap();
        f.write_hosts();
        f.write_fake_ssh();
        f.make_fake_source();
        f.make_checkout();
        f.make_remotes();
        // **Resolved LAST, because a Mac's `/var` is a symlink to `/private/var`** and the host's
        // side answers `pwd -P`: two spellings of one directory would fail every comparison.
        // After this, `seed_store` writes the resolved checkout, which is what the move reads.
        for dir in [&mut f.home, &mut f.checkout, &mut f.remotes] {
            *dir = std::fs::canonicalize(&*dir).unwrap_or_else(|_| dir.clone());
        }
        f.seed_store();
        f
    }

    fn write_hosts(&self) {
        // **`mise = false`, because these hosts are fakes on this box.** The default is *find the
        // host's mise and wrap every command in `mise exec`* — which on a developer's Mac would
        // find the real `/opt/homebrew/bin/mise` behind the fake ssh and put it in front of every
        // assertion here. A fixture says what it means.
        std::fs::write(
            &self.hosts,
            "[host.lab2x1]\nhostname = \"lab2x1\"\nuser = \"dead\"\nroot = \"~/letibot-remote\"\n\
             mise = false\n\
             \n[host.lubuntu1]\nhostname = \"lubuntu1\"\nuser = \"dead\"\n\
             root = \"~/letibot-remote\"\nmise = false\n",
        )
        .unwrap();
        // **The two facts a connection is refused without**, and both are letibot's own rather
        // than the operator's: a dedicated key per host under the config directory (the default
        // when the file says nothing), and the host key already accepted in letibot's own
        // known-hosts file. A migration that did not have them would be refused by
        // `Ssh::connect` before it moved a byte — which is the behaviour its own test asserts.
        let paths = letibot_tools::remote::Paths::in_home(&self.home);
        std::fs::create_dir_all(paths.keys()).unwrap();
        let mut known = String::new();
        for host in ["lab2x1", "lubuntu1"] {
            std::fs::write(paths.default_key(host), format!("the {host} key")).unwrap();
            known.push_str(&format!(
                "{host} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{host}\n"
            ));
        }
        std::fs::write(paths.known_hosts(), known).unwrap();
    }

    /// **The fake ssh**: skip ssh's own options, then run the command against the home the target
    /// names. A forward (`-N`) is recorded and exits — there is nothing to forward to here.
    fn write_fake_ssh(&self) {
        self.write_fake_ssh_with("");
    }

    /// The same fake, with a prelude — how a test makes a host misbehave. `prelude` runs **before**
    /// the command is dispatched, so a `sleep 300` there is a host that has stopped answering and an
    /// `exit 255` is one whose connection went away.
    fn write_fake_ssh_with(&self, prelude: &str) {
        std::fs::write(
            &self.ssh,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> \"$LETIBOT_FAKE_SSH_LOG\"\n\
                 case \" $* \" in *\" -N \"*) exit 0 ;; esac\n\
                 while [ $# -gt 0 ]; do\n\
                   case \"$1\" in\n\
                     -i|-o|-L|-F) shift 2 ;;\n\
                     -*) shift ;;\n\
                     *) break ;;\n\
                   esac\n\
                 done\n\
                 {prelude}\
\
                 target=\"$1\"; shift\n\
                 cmd=\"$1\"\n\
                 HOME=\"$LETIBOT_FAKE_REMOTES/${{target#*@}}\"\n\
                 export HOME\n\
                 mkdir -p \"$HOME/bin\"\n\
                 PATH=\"$HOME/bin:$PATH\"\n\
                 export PATH\n\
                 exec sh -c \"$cmd\"\n"
            ),
        )
        .unwrap();
        set_exec(&self.ssh);
    }

    /// The tree a host is built from: a tiny checkout, because the real one is 226 MB of history
    /// that this test has no business bundling to prove an argv is right.
    fn make_fake_source(&self) {
        std::fs::create_dir_all(self.src.join("crates/harnessd")).unwrap();
        std::fs::write(self.src.join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&self.src, &["init", "-q"]);
        git(&self.src, &["add", "-A"]);
        git(&self.src, &["commit", "-q", "-m", "a tree to build"]);
    }

    /// **The session's checkout**: one commit, one uncommitted file — the file is what proves the
    /// move LISTS what it does not carry.
    fn make_checkout(&self) {
        std::fs::create_dir_all(&self.checkout).unwrap();
        std::fs::write(self.checkout.join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&self.checkout, &["init", "-q"]);
        git(&self.checkout, &["add", "-A"]);
        git(
            &self.checkout,
            &["commit", "-q", "-m", "the session's tree"],
        );
        std::fs::write(self.checkout.join("uncommitted.txt"), "left behind\n").unwrap();
    }

    /// A session with three rows, a todo, a job and a mode — enough that a table the transfer
    /// forgot would be missed by the assertions below.
    fn seed_store(&self) {
        let store = Store::open(&self.store).unwrap();
        let tokens = vec![1u32, 2, 3, 4];
        let prefix = StablePrefixRecord {
            dialect_sha: "aa".repeat(32),
            system: "you are a harness".into(),
            tools_json: vec![r#"{"name":"read"}"#.into()],
            h_init: letibot_tokencore::ledger::hash_tokens(&tokens),
            tokens: tokens.clone(),
            vocab_source: "/models/x.gguf".into(),
        };
        let prefix_id = store.put_stable_prefix(&prefix).unwrap();
        store
            .put_session(&SessionRecord {
                id: SESSION.into(),
                title: Some("the one that moves".into()),
                model_id: "qwen-3.8-27b".into(),
                dialect_sha: prefix.dialect_sha.clone(),
                workspace_root: self.checkout.display().to_string(),
                owner: "dead".into(),
                role: Some("leticode".into()),
                approvers: vec!["dead".into()],
                parent_session_id: None,
            })
            .unwrap();
        store.put_transcript("tr-1", SESSION, &prefix_id).unwrap();
        let mut ledger = letibot_tokencore::ledger::TokenLedger::new("tr-1", &tokens).unwrap();
        let items: Vec<(TranscriptItem, Vec<u32>)> = vec![
            (
                TranscriptItem::System {
                    text: "boot".into(),
                    origin: letibot_transcript::SystemOrigin::Bootstrap,
                },
                vec![20, 21],
            ),
            (
                TranscriptItem::User {
                    speaker: Default::default(),
                    parts: vec![letibot_transcript::UserPart::Text {
                        text: "move me".into(),
                    }],
                },
                vec![22],
            ),
            (
                TranscriptItem::Assistant {
                    text: "on it".into(),
                    tool_calls: vec![],
                    truncated: false,
                },
                vec![23, 24],
            ),
        ];
        for (seq, (item, toks)) in items.iter().enumerate() {
            let row = ledger.append(&format!("it-{seq}"), toks).unwrap().clone();
            store
                .append_item("tr-1", seq as u32, item, &row, toks)
                .unwrap();
        }
        store
            .set_session_mode(SESSION, "writes-allowed", None)
            .unwrap();
    }

    /// **The two hosts**: a home each, and the `cargo` a move would find there. The fake cargo
    /// writes the two binaries the move copies out of `target/release`: a `letibot` that IS this
    /// build (so the far side's export/import/mark are real code) and a `harnessd` that records
    /// its argv and exits — no test starts a daemon.
    fn make_remotes(&self) {
        for name in ["lab2x1", "lubuntu1"] {
            let home = self.remotes.join(name);
            std::fs::create_dir_all(home.join("bin")).unwrap();
            std::fs::create_dir_all(home.join(".config/letibot")).unwrap();
            // **The host has its own keys** — a move refuses without them, and that refusal has
            // its own case below.
            std::fs::write(
                home.join(".config/letibot/providers.toml"),
                "[deepseek]\nkey = \"the host's own\"\n",
            )
            .unwrap();
            let cargo = home.join("bin/cargo");
            std::fs::write(
                &cargo,
                format!(
                    "#!/bin/sh\n\
                     printf 'cargo %s\\n' \"$*\" >> \"$LETIBOT_FAKE_SSH_LOG\"\n\
                     mkdir -p target/release\n\
                     printf '#!/bin/sh\\ncase \"$(basename \"$0\")\" in\\n  harnessd) printf \
                     \"harnessd %s\\\\n\" \"$*\" >> \"$LETIBOT_FAKE_SSH_LOG\"; exit 0 ;;\\nesac\\n\
                     exec \"{}\" \"$@\"\\n' \"$LETIBOT_REAL_LETIBOT\" > target/release/letibot\n\
                     chmod +x target/release/letibot\n",
                    env!("CARGO_BIN_EXE_letibot")
                ),
            )
            .unwrap();
            set_exec(&cargo);
        }
    }

    /// The environment a migration subprocess runs in: this world and nothing of the operator's.
    fn env(&self) -> Vec<(String, String)> {
        let real = env!("CARGO_BIN_EXE_letibot");
        vec![
            ("HOME".into(), self.home.display().to_string()),
            ("XDG_RUNTIME_DIR".into(), self.run.display().to_string()),
            ("TMPDIR".into(), self.root.join("tmp").display().to_string()),
            ("LETIBOT_STORE".into(), self.store.display().to_string()),
            ("LETIBOT_HOSTS".into(), self.hosts.display().to_string()),
            ("LETIBOT_SSH".into(), self.ssh.display().to_string()),
            ("LETIBOT_SRC".into(), self.src.display().to_string()),
            (
                "LETIBOT_FAKE_SSH_LOG".into(),
                self.ssh_log.display().to_string(),
            ),
            (
                "LETIBOT_FAKE_REMOTES".into(),
                self.remotes.display().to_string(),
            ),
            ("LETIBOT_REAL_LETIBOT".into(), real.to_string()),
        ]
    }

    /// **The runtime dir has to be SHORT.** A unix socket path is bounded by `SUN_LEN` (104 here),
    /// and a per-user temp dir under `/var/folders/…` plus `letibot/migrated-<36 chars>.sock` is
    /// past it — the bind fails with `path must be shorter than SUN_LEN`, which reads like a
    /// launcher bug and is a filesystem limit.
    fn short_run(name: &str) -> PathBuf {
        PathBuf::from("/tmp").join(format!("rh4-{}-{name}", std::process::id()))
    }

    fn migrate(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_letibot"));
        cmd.arg("--migrate").args(args);
        for (k, v) in self.env() {
            cmd.env(k, v);
        }
        cmd.output().expect("the migration verb ran")
    }

    fn ssh_log(&self) -> String {
        std::fs::read_to_string(&self.ssh_log).unwrap_or_default()
    }

    fn record(&self) -> PathBuf {
        self.run
            .join("letibot")
            .join(format!("migrated-{SESSION}.json"))
    }

    /// Where a host's copy of the session lives: `<home>/letibot-remote/...`.
    fn remote_store(&self, host: &str) -> PathBuf {
        self.remotes
            .join(host)
            .join("letibot-remote/.letibot/sessions.db")
    }

    fn remote_checkout(&self, host: &str) -> PathBuf {
        self.remotes
            .join(host)
            .join("letibot-remote/checkout")
            .join(SHORT)
    }

    fn local_lives_on(&self) -> Option<String> {
        Store::open(&self.store)
            .unwrap()
            .lives_on(SESSION)
            .unwrap()
            .map(|(on, _)| on)
    }

    fn remote_lives_on(&self, host: &str) -> Option<Option<String>> {
        let path = self.remote_store(host);
        if !path.is_file() {
            return None;
        }
        Some(
            Store::open(&path)
                .unwrap()
                .lives_on(SESSION)
                .unwrap()
                .map(|(on, _)| on),
        )
    }
}

/// The launcher's key: the first twelve hex of the workspace's SHA-256, with nothing added.
fn key12(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    format!(
        "{:.12}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn set_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "letibot test")
        .env("GIT_AUTHOR_EMAIL", "test@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot test")
        .env("GIT_COMMITTER_EMAIL", "test@letibot.invalid")
        .output()
        .expect("git ran");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git ran");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn said(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---------------------------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------------------------

/// **Laptop → host.** The rows, the tree, the daemon's argv, the forward and both markers.
#[test]
fn a_move_to_a_host_carries_the_rows_the_tree_and_the_socket() {
    let f = Fixture::new("to-host");
    let out = f.migrate(&[SESSION, "--to", "lab2x1"]);
    let text = said(&out);
    assert!(out.status.success(), "the move failed: {text}");
    assert!(
        text.contains("uncommitted change(s) are NOT carried"),
        "the person has to be told what stayed behind: {text}"
    );
    assert!(text.contains("uncommitted.txt"), "{text}");

    // The rows are in the host's own store, and the host's row is the live one.
    assert_eq!(
        f.remote_lives_on("lab2x1"),
        Some(None),
        "the copy on the host is live: {text}"
    );
    let there = Store::open(&f.remote_store("lab2x1")).unwrap();
    let row = there.session(SESSION).unwrap().expect("the row moved");
    assert_eq!(
        row.workspace_root,
        f.remote_checkout("lab2x1").display().to_string(),
        "the workspace is the host's checkout, not the laptop's"
    );
    assert_eq!(row.title.as_deref(), Some("the one that moves"));
    assert_eq!(
        there.session_mode(SESSION).unwrap().map(|(m, _)| m),
        Some("writes-allowed".into()),
        "the mode is the session's and it travels"
    );
    assert_eq!(there.load_transcript("tr-1").unwrap().items.len(), 3);

    // The tree is a checkout on `remote/<short>`, with the same HEAD.
    let wt = f.remote_checkout("lab2x1");
    assert!(wt.is_dir(), "the checkout landed at {}", wt.display());
    assert_eq!(
        git_out(&wt, &["rev-parse", "HEAD"]),
        git_out(&f.checkout, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git_out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        format!("remote/{SHORT}")
    );

    // The laptop's copy is marked, and is NOT deleted.
    assert_eq!(f.local_lives_on().as_deref(), Some("lab2x1"));
    assert!(
        Store::open(&f.store)
            .unwrap()
            .session(SESSION)
            .unwrap()
            .is_some(),
        "the rows stay: a move back has something to come back to"
    );

    // The record a head reads, and the ssh argv that made it.
    let record = f.record();
    assert!(record.is_file(), "no record at {}", record.display());
    let body = std::fs::read_to_string(&record).unwrap();
    assert!(body.contains("\"host\": \"lab2x1\""), "{body}");
    assert!(body.contains(&format!("migrated-{SESSION}.sock")), "{body}");
    let log = f.ssh_log();
    for needle in [
        "cargo build --release --offline -p letibot",
        "-N",
        "-L",
        "--session",
        "--no-queue",
        "base64",
    ] {
        assert!(
            log.contains(needle),
            "the move never ran `{needle}` over ssh:\n{log}"
        );
    }
    // And the ssh options the doc names, on every call that carried one.
    assert!(
        log.contains("BatchMode=yes") && log.contains("IdentitiesOnly=yes"),
        "a host that asks for a password must be refused by ssh, not prompted:\n{log}"
    );
    assert!(log.contains("ControlMaster=auto"), "{log}");
}

/// **Host → laptop.** The branch comes back as `remote/<host>/<short>`, the rows are caught up,
/// and the record goes: nothing forwards to a session that lives here.
#[test]
fn a_move_back_fetches_the_branch_and_clears_the_marker() {
    let f = Fixture::new("back-home");
    let out = f.migrate(&[SESSION, "--to", "lab2x1"]);
    assert!(out.status.success(), "{}", said(&out));

    let out = f.migrate(&[SESSION, "--to", "local"]);
    let text = said(&out);
    assert!(out.status.success(), "the move back failed: {text}");
    assert_eq!(
        f.local_lives_on(),
        None,
        "the copy here is the live one again: {text}"
    );
    assert_eq!(
        f.remote_lives_on("lab2x1"),
        Some(Some("local".to_string())),
        "and the host's copy says where it went"
    );
    assert!(
        !f.record().exists(),
        "there is no forward to find once the session is back"
    );
    assert_eq!(
        git_out(
            &f.checkout,
            &["rev-parse", &format!("refs/heads/remote/lab2x1/{SHORT}")]
        ),
        git_out(&f.checkout, &["rev-parse", "HEAD"]),
        "the branch came home under remote/<host>/<short>"
    );
    // One copy, three rows: the catch-up did not duplicate anything.
    let store = Store::open(&f.store).unwrap();
    assert_eq!(store.session(SESSION).unwrap().unwrap().items, 3);
}

/// **Host → host, relayed through this box.** The source is wherever the marker says, and the
/// laptop's own row follows the hop.
#[test]
fn a_hidden_hop_moves_the_session_on_and_the_row_here_says_where() {
    let f = Fixture::new("host-to-host");
    let out = f.migrate(&[SESSION, "--to", "lab2x1"]);
    assert!(out.status.success(), "{}", said(&out));

    let out = f.migrate(&[SESSION, "--to", "lubuntu1"]);
    let text = said(&out);
    assert!(out.status.success(), "the hop failed: {text}");
    assert!(
        text.contains("lives on `lab2x1`"),
        "the source is where the marker says, not this box: {text}"
    );

    assert_eq!(
        f.remote_lives_on("lubuntu1"),
        Some(None),
        "the new host's copy is live: {text}"
    );
    assert_eq!(
        f.remote_lives_on("lab2x1"),
        Some(Some("lubuntu1".to_string())),
        "the old host's copy names where it went"
    );
    assert_eq!(
        f.local_lives_on().as_deref(),
        Some("lubuntu1"),
        "and the laptop's row — the index every surface reads — follows the hop"
    );
    let there = Store::open(&f.remote_store("lubuntu1")).unwrap();
    assert!(
        there.session(SESSION).unwrap().is_some(),
        "the rows arrived at the second host"
    );
    assert!(
        f.remote_checkout("lubuntu1").is_dir(),
        "and so did the tree"
    );
}

/// **Quiesce refuses rather than waiting out a session stopped on a card.** The card is the one
/// state a person has to clear, and a verb that waited for them would look like a hang.
#[test]
fn a_session_stopped_on_a_card_is_refused_and_nothing_moves() {
    let f = Fixture::new("on-a-card");
    // A real daemon, in this process, on the socket the launcher's key says this folder's
    // daemon listens on — with its activity saying "stopped on a card".
    let registry = Registry::new();
    registry
        .create(SESSION, "the one that moves", SessionWiring::default())
        .expect("a session to hold");
    if let Some(hub) = registry.get(SESSION) {
        hub.publish(SessionEvent::ActivityChanged {
            activity: Activity::of(vec![Waiting::Ask("adj-1".into())]),
        });
    }
    let rundir = f.run.join("letibot");
    std::fs::create_dir_all(&rundir).unwrap();
    let socket = rundir.join(format!("{}.sock", key12(&f.checkout)));
    let server = serve_registry(registry.clone(), &socket).expect("bind");

    let out = f.migrate(&[SESSION, "--to", "lab2x1", "--patience", "2"]);
    let text = said(&out);
    assert!(
        !out.status.success(),
        "a card must not be waited out: {text}"
    );
    assert!(
        text.contains("stopped on a card"),
        "the refusal has to say WHY it will not wait: {text}"
    );
    assert_eq!(
        f.local_lives_on(),
        None,
        "nothing was marked, so nothing moved: {text}"
    );
    assert!(
        f.remote_lives_on("lab2x1").is_none(),
        "and no store was made on the host: {text}"
    );
    assert!(!f.record().exists());

    server.shutdown();
}

/// **A host with no provider keys is refused before a byte moves**, and `--copy-keys` is the way
/// to give it some — copied, mode 600, never printed.
#[test]
fn a_host_without_provider_keys_is_refused_unless_the_keys_are_copied() {
    let f = Fixture::new("keys");
    // A host that has none — removed here, because the fixture gives every host its own keys so
    // that the other cases are about the move and not about this refusal.
    std::fs::remove_file(f.remotes.join("lab2x1/.config/letibot/providers.toml")).unwrap();
    let out = f.migrate(&[SESSION, "--to", "lab2x1"]);
    let text = said(&out);
    assert!(!out.status.success(), "no keys, no move: {text}");
    assert!(
        text.contains("providers.toml"),
        "the refusal names the file: {text}"
    );
    assert!(text.contains("--copy-keys"), "{text}");
    assert!(f.remote_lives_on("lab2x1").is_none(), "{text}");

    // And with the flag: the file goes over, mode 600, and the move completes.
    std::fs::create_dir_all(f.home.join(".config/letibot")).unwrap();
    let secret = "secret-key-value-that-must-not-be-printed\n";
    std::fs::write(f.home.join(".config/letibot/providers.toml"), secret).unwrap();
    let out = f.migrate(&[SESSION, "--to", "lab2x1", "--copy-keys"]);
    let text = said(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        !text.contains("secret-key-value"),
        "the keys must never be printed: {text}"
    );
    let there = f.remotes.join("lab2x1/.config/letibot/providers.toml");
    assert_eq!(std::fs::read_to_string(&there).unwrap(), secret);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&there).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the keys land mode 600, not 644");
}

/// **A step that gets no answer is killed at its bound** — rule 1, measured on the second caller of
/// an ssh call.
///
/// The fake ssh sleeps for ever. The verb's first call to the destination is `test -f
/// providers.toml` (`Retried::Never`, one attempt), so the whole run is one [`STEP_BOUND`] — and
/// the sentence names the step, the host and the bound. Twenty seconds is what the bound IS; a test
/// that made it shorter would measure a number nothing in production uses.
///
/// What this does **not** measure is the breaker, and it cannot: every invocation of
/// `letibot --migrate` is its own process, and the breaker — like the call records — is per
/// process. The two are measured where they live: `letibot_tools::rpc`'s own tests for the state
/// machine, and `remote_hosts.rs` for the same state reached through a real backend.
#[test]
fn a_step_that_never_answers_is_killed_at_its_bound() {
    use std::time::Instant;
    let f = Fixture::new("hang");
    f.write_fake_ssh_with("case \"$*\" in *lubuntu1*) sleep 300 ;; esac\n");
    let began = Instant::now();
    let out = f.migrate(&[SESSION, "--to", "lubuntu1"]);
    let took = began.elapsed();
    let text = said(&out);
    assert!(
        !out.status.success(),
        "a host that never answers is a refusal: {text}"
    );
    assert!(
        took < letibot_tools::remote::STEP_BOUND * 3,
        "the verb came back inside its bound ({took:?}): {text}"
    );
    assert!(
        text.contains("lubuntu1") && text.contains("got no answer within"),
        "the refusal names the host and the bound: {text}"
    );
    assert!(
        text.contains("killed rather than waited on"),
        "and what was done about it: {text}"
    );
    assert_eq!(
        f.local_lives_on(),
        None,
        "nothing was moved, and no marker says otherwise"
    );
}
