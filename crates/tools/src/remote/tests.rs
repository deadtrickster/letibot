/// A wait's last few milliseconds still give a status poll the floor: a poll shorter than a
/// round trip never answers, and the job it asks about reads as running for ever.
#[test]
fn a_status_poll_is_never_given_less_than_the_floor() {
    assert_eq!(
        super::poll_budget(Duration::from_millis(30), Duration::from_secs(5)),
        super::POLL_FLOOR
    );
    assert_eq!(
        super::poll_budget(Duration::from_secs(60), Duration::from_secs(5)),
        Duration::from_secs(5)
    );
    assert_eq!(
        super::poll_budget(Duration::from_secs(3), Duration::from_secs(5)),
        Duration::from_secs(3)
    );
}

/// A pgid file is a claim: the group it names is signalled only when that process was started
/// by [`grouped`] for this very file, so a rewritten file cannot aim the kill at a neighbour.
#[test]
fn a_pgid_file_naming_someone_elses_process_signals_nothing() {
    let dir = std::env::temp_dir().join(format!("letibot-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let alive = |pid: u32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let sh = |script: &str| {
        std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .output()
            .expect("bash")
    };
    // A bystander: not started by `grouped`, named by a forged file.
    let mut bystander = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .unwrap();
    let forged = dir.join("run-forged.pgid");
    std::fs::write(&forged, format!("{}\n", bystander.id())).unwrap();
    sh(&cancel_script(&forged.display().to_string()));
    let kept = alive(bystander.id());
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(
        kept,
        "the cancel signalled a process that never wrote the file"
    );
    assert!(!forged.exists(), "a forged file is removed, not trusted");

    // And the real thing is still ended: the wrapper's own process carries the file path.
    let real = dir.join("run-real.pgid");
    let wrapped = grouped("sleep 300", &real.display().to_string());
    let mut leader = std::process::Command::new("bash")
        .arg("-c")
        .arg(&wrapped)
        .spawn()
        .unwrap();
    // The value, not the file's existence: a wrapper that has just created its group file has not
    // necessarily written a number into it yet, and reading it twice would be two observations.
    let pid: u32 = crate::testing::wait_for("the wrapper's group to be readable", || {
        std::fs::read_to_string(&real)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    sh(&cancel_script(&real.display().to_string()));
    let _ = leader.wait();
    let gone = crate::testing::wait_within(Duration::from_secs(1), || !alive(pid));
    let _ = sh(&format!("kill -KILL -{pid} 2>/dev/null"));
    assert!(gone, "the wrapper's own group was not ended");
    std::fs::remove_dir_all(&dir).ok();
}
use super::*;

const FILE: &str = r#"
# the operator's machines
[host.lab2x1]
hostname = "lab2x1"
user     = "dead"
key      = "~/.config/letibot/keys/lab2x1"
path     = ["~/.cargo/bin", "/opt/bin"]
root     = "~/letibot-remote"

[host."bench-2"]
hostname = "10.0.0.7"
user     = "root"
"#;

/// A test's own home and config directory, with a key for `host` and its host key already
/// accepted - the two facts [`Ssh::connect`] refuses without.
fn paths_for(tag: &str, host: &str, hostname: &str) -> (PathBuf, Paths) {
    let dir = std::env::temp_dir().join(format!(
        "letibot-policy-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let paths = Paths::in_home(dir.join("home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key(host), b"a key").expect("the dedicated key");
    std::fs::write(
        paths.known_hosts(),
        format!("{hostname} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n"),
    )
    .expect("a known host");
    (dir, paths)
}

#[test]
fn a_missing_file_is_no_hosts() {
    let h = Hosts::load_from("/nonexistent/hosts.toml");
    assert!(h.is_empty());
    assert!(h.names().is_empty());
}

#[test]
fn the_file_the_contract_documents_parses() {
    let h = Hosts::parse(FILE).expect("it parses");
    assert_eq!(h.names(), vec!["bench-2".to_string(), "lab2x1".to_string()]);
    let lab = h.get("lab2x1").expect("lab2x1");
    assert_eq!(lab.hostname, "lab2x1");
    assert_eq!(lab.user.as_deref(), Some("dead"));
    assert_eq!(lab.root, "~/letibot-remote");
    assert_eq!(lab.destination(), "dead@lab2x1");
    assert_eq!(
        lab.path,
        vec!["~/.cargo/bin".to_string(), "/opt/bin".to_string()],
        "a host's `path` list is read, and left as the REMOTE spells it"
    );
    assert!(
        lab.key.as_ref().is_some_and(|k| k.ends_with("keys/lab2x1")),
        "a `~` in a key is this machine's home: {:?}",
        lab.key
    );
    // The two defaults: hostname falls back to the name, root to the contract's.
    let bench = h.get("bench-2").expect("a quoted section name");
    assert_eq!(bench.hostname, "10.0.0.7");
    assert_eq!(bench.root, "~/letibot-remote");
    assert_eq!(bench.destination(), "root@10.0.0.7");
}

#[test]
fn a_wrong_host_is_told_which_ones_are_there() {
    let h = Hosts::parse(FILE).expect("it parses");
    let said = h.refusal("lab2");
    assert!(said.contains("lab2x1"), "{said}");
    assert!(said.contains("bench-2"), "{said}");
    assert!(said.contains("Nothing was placed"), "{said}");
    let none = Hosts::default().refusal("lab2x1");
    assert!(none.contains("no `[host."), "{none}");
}

#[test]
fn a_branch_name_is_eight_safe_characters() {
    assert_eq!(short_branch_id("s-17ab93cd-ef01-2345"), "ef012345");
    assert_eq!(short_branch_id("x"), "x");
    assert_eq!(short_branch_id("---"), "session");
}

#[test]
fn quoting_survives_a_quote() {
    assert_eq!(quote("plain/path.ext"), "plain/path.ext");
    assert_eq!(quote("it's"), r#"'it'\''s'"#);
    assert_eq!(quote(""), "''");
}

/// A `~` in the wrapped directory is expanded on the REMOTE and never left quoted: `mise exec
/// -C` resolves a literal `~` against the directory it is in, so a quoted tilde names the wrong
/// tree. The same rule `path`'s entries follow ([`one_entry`]), asked of `-C`.
#[test]
fn a_tilde_directory_is_expanded_on_the_remote() {
    let h = Hosts::parse(FILE).expect("it parses");
    let host = h.get("lab2x1").expect("lab2x1").clone();
    let (dir, paths) = paths_for("tilde-dir", &host.name, &host.hostname);
    let host = Host {
        key: Some(paths.default_key("lab2x1")),
        mise: Some(Mise::At("/opt/mise".into())),
        ..host
    };
    let ssh =
        Ssh::connect(host, Some(PathBuf::from("/bin/false")), paths).expect("the connection opens");

    let absolute = ssh
        .clone()
        .with_dir("/home/dead/letibot-remote/repo/01abcdef");
    let said = absolute.remote_command("true");
    assert!(
        said.starts_with("/opt/mise exec -C /home/dead/letibot-remote/repo/01abcdef -- bash -lc "),
        "an absolute directory is handed over as it is: {said}"
    );

    let tilde = ssh.with_dir("~/letibot-remote/repo/01abcdef");
    let said = tilde.remote_command("true");
    assert!(
        said.contains("exec -C \"$HOME/letibot-remote/repo/01abcdef\" --"),
        "a `~` is the REMOTE's home, expressed so its shell expands it: {said}"
    );
    assert!(
        !said.contains("'~") && !said.contains(" -C ~"),
        "and never handed over as a literal tilde: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A refused mux session is read as transport, and nothing else is. Each sentence says the
/// connection is up and this call got no channel; `Permission denied` and `Could not resolve
/// hostname` are not transport and must not be retried under "nothing ran".
#[test]
fn the_four_ways_a_mux_loses_a_session_are_read_as_transport() {
    for (said, want) in [
        (
            "mux_client_request_session: session request failed",
            MuxRefusal::Session,
        ),
        ("mm_send_fd: sendmsg(1): Message too long", MuxRefusal::Fds),
        ("mm_send_fd: sendmsg(2): Message too long", MuxRefusal::Fds),
        (
            "channel 3: open failed: administratively prohibited: open failed",
            MuxRefusal::Channel,
        ),
        (
            "Control socket connect /tmp/letibot/ssh-lab2x1-7-call.sock: Connection refused",
            MuxRefusal::StaleSocket,
        ),
    ] {
        assert_eq!(mux_refusal(said), Some(want), "as transport: {said}");
        assert!(
            auth_refusal(said).is_none(),
            "and not as an auth refusal: {said}"
        );
    }
    for said in [
        "Permission denied (publickey).",
        "ssh: Could not resolve hostname gone.invalid: Name or service not known",
        "Connection timed out during banner exchange",
        "Host key verification failed.",
        "",
    ] {
        assert_eq!(mux_refusal(said), None, "not a refusal of the mux: {said}");
    }
}

/// Every daemon gets its own master, and the socket fits: a name of the host alone would
/// multiplex every daemon on the box over one connection, dividing `MaxSessions` among them.
/// Git gets a socket beside it, for an ssh this process never starts and so cannot count.
#[test]
fn one_daemon_has_one_master_and_git_has_a_second() {
    let mine = control_path("lab2x1", 4242);
    assert_eq!(mine, control_path("lab2x1", 4242));
    assert_ne!(control_path("lab2x1", 4243), mine);
    assert_ne!(git_control_path("lab2x1", 4242), mine);
    assert_ne!(
        git_control_path("lab2x1", 4243),
        git_control_path("lab2x1", 4242)
    );
    assert_eq!(
        mine,
        control_path("lab2x1", 4242),
        "and the same name again"
    );
    assert!(
        mine.as_os_str().len() <= 100,
        "under the `SUN_LEN` cap: {}",
        mine.display()
    );
    assert!(
        mine.file_name()
            .map(|n| n.to_string_lossy().contains("4242"))
            .unwrap_or(false),
        "the daemon is named in the file: {}",
        mine.display()
    );
}

/// One close asks for ONE transfer, so a slot wait, a run and the retry come out of one
/// `GIT_BOUND`: the arithmetic that stops a refused fetch costing four of them.
#[test]
fn one_fetch_spends_one_budget_however_many_attempts() {
    let spent = FetchBudget {
        began: Instant::now() - (GIT_BOUND + Duration::from_secs(1)),
        within: GIT_BOUND,
    };
    assert_eq!(
        spent.left(),
        Duration::ZERO,
        "a spent fetch has nothing left"
    );
    assert_eq!(
        spent.wait(STEP_BOUND),
        Duration::ZERO,
        "and no wait may outlive it"
    );
    let fresh = FetchBudget::new(GIT_BOUND);
    assert_eq!(
        fresh.wait(STEP_BOUND),
        STEP_BOUND,
        "a fresh fetch waits as long as the connection is given"
    );
}

/// The refusal that caused a retry is the only clue pointing at the master's `MaxSessions`, so
/// the report for a retry that found no slot names it. Dropping it would leave `busy`'s words,
/// which say nothing was sent about a fetch whose first attempt did send and was refused.
#[test]
fn a_retry_that_finds_no_slot_still_says_what_was_refused() {
    let said = refused_then_no_slot(
        b"mux_client_request_session: session request failed\nmore words",
        "fake (git) already has 8 ssh calls in flight from this process",
    );
    assert!(
        said.contains("mux_client_request_session: session request failed"),
        "the refusal is quoted: {said}"
    );
    assert!(
        !said.contains("more words"),
        "one line, not the whole stderr: {said}"
    );
    assert!(
        said.contains("earlier attempt"),
        "said of which attempt: {said}"
    );
    assert!(
        said.contains("no slot on the retry") && said.contains("8 ssh calls in flight"),
        "and what the retry got instead: {said}"
    );
}

/// The ssh argv is the policy's, and nothing in it names the operator's ssh material. The
/// assertions about what is missing are the point: a list with `-i` and no `IdentitiesOnly`
/// looks complete.
#[test]
fn the_ssh_argv_is_the_contract_flags_then_dest_then_command() {
    let h = Hosts::parse(FILE).expect("it parses");
    let host = h.get("lab2x1").expect("lab2x1").clone();
    let (dir, paths) = paths_for("argv", &host.name, &host.hostname);
    // The key the FILE names is this fixture's home, not the operator's.
    let declared = paths.default_key("lab2x1");
    let host = Host {
        key: Some(declared.clone()),
        ..host
    };
    let ssh = Ssh::connect(host, Some(PathBuf::from("/bin/false")), paths.clone())
        .expect("the connection opens");
    let argv = ssh.argv("bash -lc 'true'");
    let flags = argv.join(" ");
    for want in [
        "-F /dev/null".to_string(),
        format!("-o UserKnownHostsFile={}", paths.known_hosts().display()),
        "-o GlobalKnownHostsFile=/dev/null".to_string(),
        "-o StrictHostKeyChecking=yes".to_string(),
        "-o BatchMode=yes".to_string(),
        "-o IdentitiesOnly=yes".to_string(),
        "-o IdentityAgent=none".to_string(),
        "-o ControlMaster=auto".to_string(),
        format!("-i {}", declared.display()),
    ] {
        assert!(flags.contains(&want), "`{want}` is missing from: {flags}");
    }
    assert!(
        !flags.contains(&paths.user_ssh().display().to_string()),
        "nothing on the command line names the operator's ssh material: {flags}"
    );
    assert_eq!(argv[argv.len() - 2], "dead@lab2x1");
    assert_eq!(argv[argv.len() - 1], "bash -lc 'true'");
    std::fs::remove_dir_all(&dir).ok();
}

/// Two refusals, before anything is arranged: a key that is not there (or is inside the
/// operator's own ssh directory), and a host key nobody has accepted here.
#[test]
fn a_host_is_refused_by_name_before_anything_is_placed() {
    let h = Hosts::parse(FILE).expect("it parses");
    let host = h.get("lab2x1").expect("lab2x1").clone();
    let (dir, paths) = paths_for("refuse", &host.name, &host.hostname);

    // A host whose key is one of the operator's own.
    std::fs::create_dir_all(paths.user_ssh()).expect("their directory");
    let theirs = paths.user_ssh().join("id_ed25519");
    std::fs::write(&theirs, b"k").expect("their key");
    let why = Ssh::connect(
        Host {
            key: Some(theirs),
            ..host.clone()
        },
        None,
        paths.clone(),
    )
    .expect_err("a key inside the operator's directory");
    assert!(why.contains("forbidden to touch"), "{why}");

    // A key that is simply not there.
    let why = Ssh::connect(
        Host {
            key: Some(dir.join("missing")),
            ..host.clone()
        },
        None,
        paths.clone(),
    )
    .expect_err("a key with no file");
    assert!(why.contains("no file there"), "{why}");

    // A host key nobody has accepted: the refusal carries the command that adds it.
    std::fs::write(paths.known_hosts(), "").expect("an empty known_hosts");
    // The key left to its default, which `paths_for` made in this test's own home: the fixture's
    // `~/...` key is expanded against the REAL `$HOME` when the file is parsed, so a fixture that
    // relies on it passes only where that key exists.
    let why = Ssh::connect(Host { key: None, ..host }, None, paths.clone())
        .expect_err("an unknown host key");
    assert!(why.contains("ssh-keyscan -t ed25519"), "{why}");
    assert!(why.contains("Nothing was connected"), "{why}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A host's `path` list is in front of every remote command, and a host without one runs exactly
/// the command it always did.
#[test]
fn a_hosts_path_list_is_a_prefix_on_every_remote_command() {
    assert_eq!(path_export(&[]), None);
    let export = path_export(&["~/.cargo/bin".into(), "/opt/x y/bin".into()]).expect("export");
    assert!(export.starts_with("export PATH="), "{export}");
    assert!(
        export.contains("\"$HOME/.cargo/bin\""),
        "a `~` is expanded by the REMOTE shell and never quoted into a literal tilde: {export}"
    );
    assert!(export.contains("'/opt/x y/bin'"), "{export}");
    assert!(export.ends_with(":\"$PATH\""), "{export}");
    assert_eq!(
        host_with_path(vec!["~/.cargo/bin".into()]).with_path("cd x && git status"),
        "export PATH=\"$HOME/.cargo/bin\":\"$PATH\"; cd x && git status"
    );
    assert_eq!(host_with_path(Vec::new()).with_path("true"), "true");
    assert_eq!(
        path_prefix(&["~/.cargo/bin".into()]).as_deref(),
        Some("PATH=\"$HOME/.cargo/bin\":\"$PATH\"")
    );
    // An entry that is not a tilde path is quoted as any word is, and an awkward one cannot
    // escape its quoting.
    assert_eq!(
        path_export(&["/opt/x y".into()]).as_deref(),
        Some("export PATH='/opt/x y':\"$PATH\"")
    );
    assert_eq!(
        path_export(&["~/a b".into()]).as_deref(),
        Some("export PATH=\"$HOME\"/'a b':\"$PATH\"")
    );
}

fn host_with_path(path: Vec<String>) -> Host {
    Host {
        name: "lab2x1".into(),
        hostname: "lab2x1".into(),
        user: Some("dead".into()),
        key: None,
        path,
        root: "~/letibot-remote".into(),
        // A fixture's host is not the operator's machine: no probe, nothing to find.
        mise: Some(Mise::Off),
        ..Host::default()
    }
}

/// The file's refusals: a key it does not have, a key stated twice, a `path` that is not a list,
/// and a host that omits `key` getting the dedicated default.
#[test]
fn hosts_toml_refuses_what_it_does_not_understand() {
    let why =
        Hosts::parse("[host.a]\nhostname = \"a\"\nkeyy = \"x\"\n").expect_err("an unknown key");
    assert!(why.contains("keyy"), "{why}");
    let why = Hosts::parse("[host.a]\nhostname = \"a\"\nhostname = \"b\"\n").expect_err("twice");
    assert!(why.contains("twice"), "{why}");
    let why = Hosts::parse("[host.a]\npath = \"~/.cargo/bin\"\n").expect_err("not a list");
    assert!(why.contains("one line of quoted strings"), "{why}");

    let h = Hosts::parse("[host.a]\nhostname = \"a\"\n").expect("it parses");
    let a = h.get("a").expect("a");
    assert!(
        a.key.is_none(),
        "omitted, so the dedicated default answers it"
    );
    assert!(a.path.is_empty());
    assert_eq!(a.root, "~/letibot-remote");
    // The three keys a shared host needs default to nothing held back, which is the safe
    // direction: a host nobody declared anything for behaves exactly as it did before this
    // package, and only a file that says otherwise changes a teardown's reach.
    assert_eq!(a.reserve, 0);
    assert!(a.flags.is_empty());
    assert!(a.never_touch.is_empty());
}

/// `reserve`, `flags` and `never_touch`: what the operator says about sharing a machine, read
/// strictly like every other key - a number, two lists, and nothing guessed.
#[test]
fn hosts_toml_reads_the_three_sharing_keys() {
    let h = Hosts::parse(
        "[host.lab2x1]\nhostname = \"lab2x1\"\nreserve = 4\n\
             flags = [\"heavy\", \"tmpfs\"]\nnever_touch = [\"pgfuzz\", \"etcd\"]\n",
    )
    .expect("it parses");
    let a = h.get("lab2x1").expect("lab2x1");
    assert_eq!(a.reserve, 4);
    assert_eq!(a.flags, vec!["heavy", "tmpfs"]);
    assert_eq!(a.never_touch, vec!["pgfuzz", "etcd"]);
    assert!(a.has_flag("TMPFS"), "a flag is a word a person types");
    // A substring, case-insensitively: the operator declared the workload and not its spelling, and
    // matching less than they meant is the failure this list exists to prevent.
    assert_eq!(a.never_touch_hit("pgfuzz -t 3600"), Some("pgfuzz"));
    assert_eq!(a.never_touch_hit("/usr/bin/ETCD-fuzz"), Some("etcd"));
    assert_eq!(a.never_touch_hit("cargo test --offline"), None);
    // cores minus load minus reserve, and a negative is reported rather than clamped.
    assert_eq!(a.spare_cores(32, "4.00 2.00 1.00"), Some(24.0));
    assert_eq!(a.spare_cores(8, "12.00 3.00 1.00"), Some(-8.0));
    assert_eq!(a.spare_cores(0, "1.00"), None);
    assert_eq!(a.spare_cores(8, ""), None);

    // A `reserve` that is not a number is refused by name rather than read as zero: a
    // scheduler that believes a busy machine is empty is the collision this is against.
    let why = Hosts::parse("[host.a]\nreserve = \"two\"\n").expect_err("not a number");
    assert!(why.contains("whole number of cores"), "{why}");
    let why = Hosts::parse("[host.a]\nflags = \"heavy\"\n").expect_err("not a list");
    assert!(why.contains("one line of quoted strings"), "{why}");
    assert!(why.contains("flags"), "{why}");
    let why = Hosts::parse("[host.a]\nnever_touch = pgfuzz\n").expect_err("not a list either");
    assert!(why.contains("never_touch"), "{why}");
}

/// The wrapper is withheld from a boundary with no network, and from nothing else. The rule is a
/// free function because no boundary can be measured on every box a test runs on.
#[test]
fn the_build_wrapper_is_withheld_only_inside_a_boundary_with_no_network() {
    use crate::exec::Egress;
    assert!(super::wrapper_withheld(true, &Egress::Denied));
    assert!(!super::wrapper_withheld(
        true,
        &Egress::Host {
            why: "allow-all".into()
        }
    ));
    // Unconfined, the command has the machine's own network: the cache works there as it does for
    // a `cargo` run by hand.
    assert!(!super::wrapper_withheld(false, &Egress::Denied));
}

/// `rustc_wrapper` and `sccache_dir`: the machine's compile cache, two plain paths, and the default
/// is no cache at all - which is what every host did before this key existed.
#[test]
fn hosts_toml_reads_the_build_wrapper_and_its_cache() {
    let h = Hosts::parse(
        "[host.xmg]\nhostname = \"xmg\"\nrustc_wrapper = \"~/.cargo/bin/sccache-rustc\"\n\
         sccache_dir = \"~/.cache/sccache\"\n",
    )
    .expect("it parses");
    let a = h.get("xmg").expect("xmg");
    // `~` is left as written: it is the FAR shell's home, expanded where the command runs.
    assert_eq!(
        a.rustc_wrapper.as_deref(),
        Some("~/.cargo/bin/sccache-rustc")
    );
    assert_eq!(a.sccache_dir.as_deref(), Some("~/.cache/sccache"));

    // Unset by default, both of them: a host nobody declared a wrapper for is a cold build, not a
    // refused placement.
    let plain = Hosts::parse("[host.a]\nhostname = \"a\"\n").expect("it parses");
    let a = plain.get("a").expect("a");
    assert!(a.rustc_wrapper.is_none());
    assert!(a.sccache_dir.is_none());

    // A wrapper with no cache line is a wrapper whose cache is its own business, and it is taken.
    let one = Hosts::parse("[host.b]\nrustc_wrapper = \"/opt/bin/w\"\n").expect("it parses");
    let b = one.get("b").expect("b");
    assert_eq!(b.rustc_wrapper.as_deref(), Some("/opt/bin/w"));
    assert!(b.sccache_dir.is_none());

    // A typo in either name is refused by name, like every other key in this file.
    let why = Hosts::parse("[host.a]\nrustc_wraper = \"/x\"\n").expect_err("a typo's refusal");
    assert!(why.contains("rustc_wraper"), "{why}");
    assert!(
        why.contains("rustc_wrapper"),
        "the list names the keys it has: {why}"
    );
    let why = Hosts::parse("[host.a]\nsccache_dirr = \"/x\"\n").expect_err("a typo's refusal");
    assert!(why.contains("sccache_dir"), "{why}");
}

/// `cargo_home`: one path, the machine's, left as written for the far shell to expand — and unset by
/// default, which is every session fetching its own copy of every crate.
#[test]
fn hosts_toml_reads_the_shared_cargo_home() {
    let h = Hosts::parse("[host.xmg]\nhostname = \"xmg\"\ncargo_home = \"~/.cargo\"\n")
        .expect("it parses");
    assert_eq!(
        h.get("xmg").expect("xmg").cargo_home.as_deref(),
        Some("~/.cargo")
    );

    let plain = Hosts::parse("[host.a]\nhostname = \"a\"\n").expect("it parses");
    assert!(plain.get("a").expect("a").cargo_home.is_none());
    // It is read beside the two build-wrapper keys and not instead of them.
    let both = Hosts::parse(
        "[host.b]\nrustc_wrapper = \"/opt/bin/w\"\nsccache_dir = \"/c\"\ncargo_home = \"/r\"\n",
    )
    .expect("it parses");
    let b = both.get("b").expect("b");
    assert_eq!(b.cargo_home.as_deref(), Some("/r"));
    assert_eq!(b.sccache_dir.as_deref(), Some("/c"));
    assert_eq!(b.rustc_wrapper.as_deref(), Some("/opt/bin/w"));

    // A typo is refused by name, and the list it is refused against names this key.
    let why = Hosts::parse("[host.a]\ncargo_hom = \"/x\"\n").expect_err("a typo's refusal");
    assert!(why.contains("cargo_hom"), "{why}");
    assert!(why.contains("cargo_home"), "{why}");
}

/// A path is "under" a directory by its components and not by its bytes: `/a/cargo-bin` is not
/// inside `/a/cargo`, and getting this wrong would leave a shared cargo home read-only.
#[test]
fn a_path_is_under_a_directory_by_its_components() {
    assert!(super::under("/h/.cargo", "/h"));
    assert!(super::under("/h/.cargo", "/h/.cargo"));
    assert!(super::under("/h/.cargo/", "/h/.cargo"));
    assert!(!super::under("/h/cargo-bin", "/h/cargo"));
    assert!(!super::under("/h", "/h/.cargo"));
}

/// `stable_paths` is a switch: `true`, `false`, or a refusal that names the key. It moves every
/// path a session's tools are given, so a value nobody can act on is not read as "on".
#[test]
fn hosts_toml_reads_stable_paths_as_a_switch() {
    let h = Hosts::parse("[host.a]\nhostname = \"a\"\n").expect("it parses");
    assert!(!h.get("a").expect("a").stable_paths, "absent is off");
    let h = Hosts::parse("[host.a]\nhostname = \"a\"\nstable_paths = true\n").expect("it parses");
    assert!(h.get("a").expect("a").stable_paths);
    let h = Hosts::parse("[host.a]\nstable_paths = false\n").expect("it parses");
    assert!(!h.get("a").expect("a").stable_paths);
    let why = Hosts::parse("[host.a]\nstable_paths = yes\n").expect_err("not a switch");
    assert!(why.contains("stable_paths"), "{why}");
    assert!(why.contains("true") && why.contains("false"), "{why}");
}

/// One port per session, out of a band of its own: below the ephemeral range, away from sccache's
/// default 4226 (the host's own server), and the same port for every command of one session.
#[test]
fn a_session_holds_one_cache_port_of_its_own() {
    let first = sccache_port("s-01abcdef");
    assert_eq!(first, sccache_port("s-01abcdef"), "one session, one port");
    assert!(
        (SCCACHE_PORT_FIRST..SCCACHE_PORT_FIRST + SCCACHE_PORT_SPAN).contains(&first),
        "{first} is in the band"
    );
    assert!(first < 32768, "no port the kernel hands out: {first}");
    assert_ne!(first, 4226, "the host's own server is the one to stay off");
    let ports: std::collections::BTreeSet<u16> = (0..200)
        .map(|n| sccache_port(&format!("s-{n:05}")))
        .collect();
    assert!(
        ports.len() > 180,
        "two hundred ids over 4096 ports: {} distinct",
        ports.len()
    );
}

/// A host that mounts every session's tree at fixed names points cargo at the sccache PROGRAM, on a
/// port of the session's own: the server it starts is inside the view, which is the only place a
/// server can write the objects and the dep-info files under those names.
#[test]
fn a_rebinding_host_points_the_client_at_a_server_of_the_sessions_own() {
    let build = BuildCache {
        env: vec![(
            "RUSTC_WRAPPER".to_string(),
            "/home/dead/.cargo/bin/sccache-rustc".to_string(),
        )],
        wrapper: Some("/home/dead/.cargo/bin/sccache-rustc".to_string()),
        program: Some("/home/dead/.cargo/bin/sccache".to_string()),
        ..BuildCache::default()
    };
    let mut env = build.env.clone();
    let mut notes = Vec::new();
    let server = a_server_of_its_own(&build, "s-01abcdef", "xmg", &mut env, &mut notes)
        .expect("a host with a program beside its wrapper gets a server");
    assert_eq!(server.program, "/home/dead/.cargo/bin/sccache");
    assert_eq!(server.port, sccache_port("s-01abcdef"));
    let pairs: std::collections::BTreeMap<String, String> = env.into_iter().collect();
    assert_eq!(
        pairs.get("RUSTC_WRAPPER").map(String::as_str),
        Some("/home/dead/.cargo/bin/sccache"),
        "the wrapper is a script that may pin a port, so it is not the client here"
    );
    assert_eq!(
        pairs.get("SCCACHE_SERVER_PORT").map(String::as_str),
        Some(server.port.to_string().as_str())
    );
    let said = notes.join("\n");
    assert!(
        said.contains(&server.port.to_string()) && said.contains("inside this session's view"),
        "the placement says where the server runs: {said}"
    );
}

/// With no program beside the wrapper there is nothing to start a server inside the view, so the
/// wrapper goes and the build compiles cold: a wrong client fails the build, and a cold one does not.
#[test]
fn a_rebinding_host_with_no_program_withholds_the_wrapper() {
    let build = BuildCache {
        env: vec![(
            "RUSTC_WRAPPER".to_string(),
            "/opt/bin/sccache-rustc".to_string(),
        )],
        wrapper: Some("/opt/bin/sccache-rustc".to_string()),
        program: None,
        ..BuildCache::default()
    };
    let mut env = build.env.clone();
    let mut notes = Vec::new();
    assert!(
        a_server_of_its_own(&build, "s-01abcdef", "xmg", &mut env, &mut notes).is_none(),
        "no program, no server"
    );
    assert!(
        env.iter()
            .all(|(k, _)| k != "RUSTC_WRAPPER" && k != "SCCACHE_SERVER_PORT"),
        "nothing is left to fail a build with: {env:?}"
    );
    let said = notes.join("\n");
    assert!(
        said.contains("no `sccache` program") && said.contains("compile the tree cold"),
        "{said}"
    );
}

/// A host with no wrapper at all has no cache to point anywhere, and is left alone.
#[test]
fn a_host_with_no_wrapper_gets_no_server() {
    let mut env = Vec::new();
    let mut notes = Vec::new();
    assert!(
        a_server_of_its_own(&BuildCache::default(), "s-01", "xmg", &mut env, &mut notes).is_none()
    );
    assert!(
        env.is_empty() && notes.is_empty(),
        "nothing said, nothing set"
    );
}

/// The two spellings of a rebound tree, and the joins a prefix must not fool:
/// `/run/letibot/workbench` is not under `/run/letibot/work`, so it is not the checkout.
#[test]
fn a_stable_tree_is_spelled_two_ways_and_a_prefix_does_not_fool_either() {
    let s = Stable {
        work: "/home/dead/letibot-remote/repo/01abcdef".to_string(),
        target: "/home/dead/letibot-remote/.letibot-home/01abcdef/target".to_string(),
    };
    // What a command is given.
    assert_eq!(s.for_a_command(""), STABLE_WORK);
    assert_eq!(s.for_a_command("."), STABLE_WORK);
    assert_eq!(s.for_a_command("src/"), format!("{STABLE_WORK}/src"));
    assert_eq!(s.for_a_command(STABLE_WORK), STABLE_WORK);
    // A session that read its own real path out of a note still builds at the fixed name.
    assert_eq!(s.for_a_command(&s.work), STABLE_WORK);
    assert_eq!(
        s.for_a_command(&format!("{}/src/main.rs", s.work)),
        format!("{STABLE_WORK}/src/main.rs")
    );
    assert_eq!(
        s.for_a_command("/etc/hosts"),
        "/etc/hosts",
        "not the tree's"
    );
    // What a call that runs outside the view is given.
    assert_eq!(s.on_the_host(STABLE_WORK), s.work);
    assert_eq!(
        s.on_the_host(&format!("{STABLE_WORK}/src/main.rs")),
        format!("{}/src/main.rs", s.work)
    );
    assert_eq!(
        s.on_the_host(&format!("{STABLE_TARGET}/debug")),
        format!("{}/debug", s.target)
    );
    assert_eq!(s.on_the_host("src"), format!("{}/src", s.work));
    assert_eq!(s.on_the_host("/etc/hosts"), "/etc/hosts");
    assert_eq!(
        s.on_the_host("/run/letibot/workbench/x"),
        "/run/letibot/workbench/x",
        "a sibling name is not the mount"
    );
    // What a listing answers with, so a row the model is handed can be read back.
    assert_eq!(
        s.as_said(&format!("{}/README", s.work)),
        format!("{STABLE_WORK}/README")
    );
    assert_eq!(s.as_said(&s.work), STABLE_WORK);
    assert_eq!(
        s.as_said(&format!("{}/debug", s.target)),
        format!("{STABLE_TARGET}/debug")
    );
    assert_eq!(s.as_said("/etc/hosts"), "/etc/hosts");
    assert_eq!(
        s.as_said("/home/dead/letibot-remote/repo/01abcdef-2/x"),
        "/home/dead/letibot-remote/repo/01abcdef-2/x",
        "a session whose id begins with another's is still its own tree"
    );
}

/// A link into the ssh directory is the ssh directory, after every symlink and for a path that
/// does not exist yet: the rule is about the bytes, not the spelling.
#[test]
fn a_path_that_resolves_into_the_ssh_directory_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-policy-link-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let paths = Paths::in_home(dir.join("home"));
    let store = paths.user_ssh();
    std::fs::create_dir_all(&store).expect("the store");
    std::fs::write(store.join("id_ed25519"), b"k").expect("a key");
    std::fs::create_dir_all(dir.join("workspace")).expect("ws");
    std::os::unix::fs::symlink(&store, dir.join("workspace/keys")).expect("link");

    assert!(inside_user_ssh(
        &dir.join("workspace/keys/id_ed25519"),
        paths.home()
    ));
    assert!(inside_user_ssh(
        &store.join("a-key-not-written-yet"),
        paths.home()
    ));
    assert!(!inside_user_ssh(
        &dir.join("workspace/ordinary.txt"),
        paths.home()
    ));
    let said = dot_ssh_refusal(&dir.join("workspace/keys/id_ed25519"), paths.home());
    assert!(said.contains("forbidden to touch"), "{said}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_walk_prints_what_tree_in_walk_order_parses() {
    // The format is firecode's `find -printf '%y\t%s\t%p'` by another hand, so the one parser
    // serves both substrates, and the walk runs on the far side of an ssh, where GNU's `find`
    // may not be.
    let dir = std::env::temp_dir().join(format!(
        "letibot-walk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(dir.join("sub")).expect("tree");
    std::fs::write(dir.join("sub/a.txt"), b"hello").expect("file");
    std::fs::write(dir.join("b.txt"), b"hi").expect("file");
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "{} walk {}",
            TREE_WALK,
            quote(&dir.display().to_string())
        ))
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (entries, _) =
        crate::firecode::tree_in_walk_order(&out.stdout, &dir.display().to_string(), 100, &|_| {
            false
        });
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["b.txt", "sub", "a.txt"], "{names:?}");
    assert_eq!(entries[0].bytes, 2, "the file's size, not a directory's");
    std::fs::remove_dir_all(&dir).ok();
}

/// The fake remote of [`a_poll_that_finds_the_pid_gone_looks_at_the_status_file_again`]: its
/// own directory, and a job table that answers from two files the test writes.
struct PollRig {
    dir: PathBuf,
    ssh: PathBuf,
    /// Its answers, in order, one line per call.
    log: PathBuf,
    /// What the job's `.exit` file holds; empty is a job that wrote no status.
    status: PathBuf,
    /// Present once the job's pid is to be reported GONE.
    gone: PathBuf,
    jobs: String,
}

impl PollRig {
    fn new(tag: &str, status: &str) -> PollRig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-poll-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let rig = PollRig {
            ssh: dir.join("ssh"),
            log: dir.join("ssh.log"),
            status: dir.join("status"),
            gone: dir.join("gone"),
            jobs: dir.join("remote").display().to_string(),
            dir,
        };
        std::fs::create_dir_all(&rig.dir).expect("the fixture directory");
        std::fs::write(&rig.status, status).expect("the status file");
        rig
    }

    /// The ssh, answering three commands and running everything else here. The poll is answered
    /// from [`PollRig::gone`], the status from [`PollRig::status`], and the launch is
    /// acknowledged without starting anything: a real launcher would write files of its own.
    fn written(self) -> PollRig {
        let script = format!(
            "#!/bin/bash\n\
                 printf '%s\\n' \"$*\" >> {log}\n\
                 for last; do :; done\n\
                 case \"$last\" in\n\
                 \x20 *'echo started'*) echo started; exit 0;;\n\
                 \x20 *\"lc 'e=;\"*)\n\
                 \x20   if [ -f {gone} ]; then printf '\\t0\\t19\\n'; \\\n\
                 \x20   else printf '\\t4242\\t19\\n'; fi; exit 0;;\n\
                 \x20 *'.exit 2>/dev/null || true'*) cat {status} 2>/dev/null; exit 0;;\n\
                 esac\n\
                 exec bash -lc \"$last\"\n",
            log = quote(&self.log.display().to_string()),
            gone = quote(&self.gone.display().to_string()),
            status = quote(&self.status.display().to_string()),
        );
        crate::testing::write_stub(&self.ssh, &script);
        self
    }

    /// Every ssh call this rig answered, in order.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A host wired to this rig, and the job table over it.
    fn host(&self, tag: &str) -> RemoteProcesses {
        let (_, paths) = paths_for(tag, "fake", "fake.invalid");
        let host = Host {
            name: "fake".into(),
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        };
        let ssh = Ssh::connect(host, Some(self.ssh.clone()), paths).expect("the connection opens");
        RemoteProcesses::new(ssh, "fake".into(), self.jobs.clone())
            .with_ledger(format!("{}/jobs", self.jobs))
    }
}

/// A job whose pid is gone, as a view of it: started (so it has a pid), then the pid reported
/// gone, then asked. The status file is whatever the rig was built with.
fn started_then_gone(rig: &PollRig, tag: &str) -> JobView {
    let host = rig.host(tag);
    let job = host
        .spawn(&SpawnRequest {
            command: "true".into(),
            slug: None,
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the job starts");
    // The finish the poll reads as a kill: the status file is written and the pid is gone by
    // the time the next question is asked.
    std::fs::write(&rig.gone, b"").expect("the pid is gone");
    host.job(&job).expect("the job")
}

/// A pid file is a claim, and a kill is aimed with it: the number has to be the launcher this job
/// wrote, or a forged file sends `kill -TERM -<pid>` to a stranger. This fake answers every poll
/// with pid 4242, which runs no launcher at all.
#[test]
fn a_pid_that_is_not_the_jobs_launcher_is_not_signalled() {
    let rig = PollRig::new("forged", "").written();
    let host = rig.host("forged-pid");
    let id = host
        .spawn(&SpawnRequest {
            command: "true".into(),
            slug: None,
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the job starts");
    let reaping = host
        .kill_job_as(&id, "the test asking")
        .expect("a kill is recorded even when it is refused");
    let note = reaping.note.clone().unwrap_or_default();
    assert_eq!(reaping.mechanism, "nothing to kill", "{reaping:?}");
    assert!(note.contains("not this job's launcher"), "{note}");
    assert!(
        !rig.calls().iter().any(|c| c.contains("kill -TERM 4242")
            || c.contains("kill -TERM -4242")
            || c.contains("kill -KILL 4242")
            || c.contains("kill -KILL -4242")),
        "nothing may be signalled when the pid is unproven: {:?}",
        rig.calls()
    );
}

/// A poll that comes back "no status and the pid gone" reads the status file once more. The
/// launcher writes the status before it exits, so a pid that is gone cannot be about to write
/// one: the second look is the difference between a finished job and a killed one.
#[test]
fn a_poll_that_finds_the_pid_gone_looks_at_the_status_file_again() {
    let exited = PollRig::new("status", "0\n").written();
    let view = started_then_gone(&exited, "poll-status");
    assert_eq!(
        view.state,
        JobState::Exited { code: 0 },
        "a status file that is there is the job's own ending, whatever the pid did: {view:?}"
    );
    assert!(
        exited
            .calls()
            .iter()
            .any(|c| c.contains(".exit 2>/dev/null || true")),
        "the status file was asked for a second time: {:?}",
        exited.calls()
    );
    std::fs::remove_dir_all(&exited.dir).ok();

    // An empty status file is still a killing: the second look must not turn every dead pid
    // into a graceful exit.
    let nothing = PollRig::new("nothing", "").written();
    let view = started_then_gone(&nothing, "poll-nothing");
    assert!(
        matches!(&view.state, JobState::Killed { by } if by.contains("wrote no exit status")),
        "a pid that went away and wrote nothing is still a kill: {view:?}"
    );
    std::fs::remove_dir_all(&nothing.dir).ok();
}

#[test]
fn a_granted_gitconfig_is_the_global_one() {
    let grants = [
        Path::new("/home/dead/.cargo"),
        Path::new("/home/dead/.gitconfig"),
    ];
    let env = crate::exec::seat::env_for_grants(grants.iter().copied());
    assert!(env.contains(&("GIT_CONFIG_GLOBAL".into(), "/home/dead/.gitconfig".into())));
    assert!(env.contains(&("CARGO_HOME".into(), "/home/dead/.cargo".into())));
    let none = crate::exec::seat::env_for_grants([Path::new("/home/dead/.cargo")].iter().copied());
    assert!(none.iter().all(|(k, _)| k != "GIT_CONFIG_GLOBAL"));
}

/// The git master's bound belongs to the whole fetch and is taken once. Paid per step it is paid
/// twice - once waiting for a slot, once running - and the daemon closes sessions one at a time,
/// so every session of a fanout that ends inherits the sum.
#[test]
fn a_fetch_that_queued_for_a_slot_runs_on_what_is_left_of_its_bound() {
    let (dir, paths) = paths_for("git-deadline", "git-deadline", "git-deadline.invalid");
    let host = Host {
        name: "git-deadline".into(),
        hostname: "git-deadline.invalid".into(),
        user: Some("tester".into()),
        key: None,
        path: Vec::new(),
        root: "~/letibot-remote".into(),
        mise: Some(Mise::Off),
        ..Host::default()
    };
    let program = dir.join("git");
    crate::testing::write_stub(&program, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nsleep 30\n");
    let ssh = Ssh::connect(host, Some(program.clone()), paths).expect("the connection opens");

    // Eight fetches ahead of this one, as a fanout of eight closes would be: the slot comes free
    // 800 ms into a 2 s bound, so a fresh bound for the run would take the call to 2.8 s.
    let held: Vec<rpc::Entered> = (0..rpc::MAX_SESSIONS_PER_HOST)
        .map(|_| rpc::enter_git("git-deadline", Duration::from_secs(1)).expect("a free slot"))
        .collect();
    let queue = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        drop(held);
    });

    let bound = Duration::from_secs(2);
    let began = Instant::now();
    let out = ssh.fetch_within(
        &program,
        &dir,
        "https://example.invalid/repo",
        "main",
        bound,
    );
    let spent = began.elapsed();
    queue.join().expect("the queue thread");
    assert!(
        matches!(out, Ok(FetchEnded::Killed(_))),
        "a git that never answers is killed at the deadline: {out:?}"
    );
    assert!(
        spent < bound + Duration::from_millis(400),
        "one deadline for the whole fetch: the slot wait and the run are both measured against \
             the {} s it was given, and this call took {spent:?}",
        bound.as_secs()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fetch killed at its deadline says which deadline. The production bound is `GIT_BOUND`, but a
/// caller may give less, and a fetch with nothing left must not fork a git to kill on its first
/// poll: both are read off the answer rather than assumed.
#[test]
fn a_fetch_that_is_killed_says_how_long_it_was_given() {
    let (dir, paths) = paths_for("git-killed", "git-killed", "git-killed.invalid");
    let host = Host {
        name: "git-killed".into(),
        hostname: "git-killed.invalid".into(),
        user: Some("tester".into()),
        key: None,
        path: Vec::new(),
        root: "~/letibot-remote".into(),
        mise: Some(Mise::Off),
        ..Host::default()
    };
    let program = dir.join("git");
    let ran = dir.join("ran");
    crate::testing::write_stub(
        &program,
        &format!(
            "#!/bin/sh\n[ $# -eq 0 ] && exit 0\ntouch {}\nsleep 30\n",
            ran.display()
        ),
    );
    let ssh = Ssh::connect(host, Some(program.clone()), paths).expect("the connection opens");

    let url = "https://example.invalid/repo";
    let nothing = ssh.fetch_within(&program, &dir, url, "main", Duration::ZERO);
    assert!(
        !ran.exists(),
        "a fetch with no time left in it started a program it could only kill: {nothing:?}"
    );
    assert!(
        matches!(nothing, Ok(FetchEnded::Killed(_))),
        "a fetch that ran nothing says so, and is not read as a transfer that timed out: \
             {nothing:?}"
    );
    let _ = std::fs::remove_file(&ran);

    let bound = Duration::from_millis(200);
    let began = Instant::now();
    let out = ssh.fetch_within(&program, &dir, url, "main", bound);
    let spent = began.elapsed();
    assert!(
        ran.exists(),
        "a fetch with time in it runs the program: {out:?}"
    );
    match out {
        Ok(FetchEnded::Killed(said)) => {
            assert!(
                said >= bound && said <= spent,
                "the kill is reported against the {bound:?} this fetch was given, rather than a \
                     bound it was not: it said {said:?} of {spent:?}"
            );
        }
        other => panic!("a git that never answers is killed at its deadline: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// One path outside the view must not be the whole page's answer, which the trait reads as *the
/// channel failed and says nothing about any file* - so every path in a page of 64 went back to
/// one round trip of its own, for a verb's whole budget.
#[test]
fn a_page_with_one_path_outside_the_view_still_reads_the_rest() {
    let seen = std::cell::RefCell::new(Vec::new());
    let answers = batched_page(
        &["in/a.md", "/etc/passwd", "in/b.md"],
        |p| {
            if p.starts_with('/') {
                Err(BackendError::Outside(p.to_string()))
            } else {
                Ok(format!("/w/{p}"))
            }
        },
        |asked| {
            *seen.borrow_mut() = asked.iter().map(|s| s.to_string()).collect();
            Ok(vec![Ok(b"a".to_vec()), Ok(b"b".to_vec())])
        },
    )
    .expect("a path outside the view is not a failed page");
    assert_eq!(
        seen.borrow().as_slice(),
        ["/w/in/a.md", "/w/in/b.md"],
        "the page went over the wire once, with only the paths that are in the view"
    );
    assert_eq!(answers.len(), 3, "one answer per path asked, in order");
    assert!(matches!(&answers[0], Ok(b) if b == b"a"));
    assert!(
        matches!(&answers[1], Err(BackendError::Outside(p)) if p == "/etc/passwd"),
        "the path outside the view is its own refusal: {:?}",
        answers[1]
    );
    assert!(matches!(&answers[2], Ok(b) if b == b"b"));
}

/// A fake ssh that answers one `/proc/meminfo` read from a file the test writes, and logs every
/// call it was given. The file absent is a host that did not answer.
struct MemRig {
    dir: PathBuf,
    ssh: PathBuf,
    log: PathBuf,
    answer: PathBuf,
}

impl MemRig {
    fn new(tag: &str) -> MemRig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-meminfo-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("the fixture directory");
        MemRig {
            ssh: dir.join("ssh"),
            log: dir.join("ssh.log"),
            answer: dir.join("meminfo"),
            dir,
        }
    }

    fn written(self, meminfo: &str) -> MemRig {
        let script = format!(
            "#!/bin/bash\n\
                 printf '%s\\n' \"$*\" >> {log}\n\
                 for last; do :; done\n\
                 case \"$last\" in\n\
                 \x20 *'cat /proc/meminfo'*)\n\
                 \x20   [ -f {answer} ] || exit 1\n\
                 \x20   cat {answer}; exit 0;;\n\
                 esac\n\
                 exit 0\n",
            log = quote(&self.log.display().to_string()),
            answer = quote(&self.answer.display().to_string()),
        );
        crate::testing::write_stub(&self.ssh, &script);
        std::fs::write(&self.answer, meminfo).expect("the host's meminfo");
        self
    }

    /// The same stub with no answer at all: a host that does not answer.
    fn silent(self) -> MemRig {
        let rig = self;
        crate::testing::write_stub(
            &rig.ssh,
            &format!(
                "#!/bin/bash\nprintf '%s\\n' \"$*\" >> {log}\nfor last; do :; done\n\
                     case \"$last\" in *'cat /proc/meminfo'*) exit 1;; esac\nexit 0\n",
                log = quote(&rig.log.display().to_string()),
            ),
        );
        rig
    }

    /// A connection to it, with a bound a fake's silence cannot outlive.
    fn ssh(&self, tag: &str) -> Ssh {
        let (_, paths) = paths_for(tag, "fake", "fake.invalid");
        let host = Host {
            name: "fake".into(),
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        };
        Ssh::connect(host, Some(self.ssh.clone()), paths)
            .expect("the connection opens")
            .with_bound(Duration::from_secs(2))
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("cat /proc/meminfo"))
            .map(str::to_string)
            .collect()
    }
}

const FAKE_MEMINFO: &str = "MemTotal:       129000000 kB\nMemFree:  1000 kB\n\
                                   MemAvailable:   15800000 kB\nSwapFree: 4096 kB\n";

/// A placement's memory is read off ITS host, over the link the session already has, and the
/// card's numbers come from there — the daemon's box is a different machine (R155).
#[test]
fn a_hosts_memory_is_read_over_its_own_link_and_cached() {
    let rig = MemRig::new("read").written(FAKE_MEMINFO);
    let ssh = rig.ssh("mem-read");
    let said = ssh.meminfo().expect("the host answered");
    assert_eq!(said.total, 129_000_000 * 1024);
    assert_eq!(said.available, 15_800_000 * 1024);
    assert_eq!(said.swap_free, 4096 * 1024);
    assert_ne!(
        Some(said),
        Meminfo::read(),
        "the placement's figures are not this box's"
    );
    // A second call inside the interval costs no round trip: a cap is decided per call.
    assert_eq!(ssh.meminfo(), Some(said));
    assert_eq!(rig.calls().len(), 1, "one round trip per interval");
    let _ = std::fs::remove_dir_all(&rig.dir);
}

/// The interval is a cache and not a lifetime: a reading older than it is taken again, and a
/// changed host is a changed answer.
#[test]
fn a_reading_older_than_the_interval_is_taken_again() {
    let rig = MemRig::new("ttl").written(FAKE_MEMINFO);
    let ssh = rig.ssh("mem-ttl").with_meminfo_ttl(Duration::ZERO);
    let first = ssh.meminfo().expect("the host answered");
    std::fs::write(&rig.answer, "MemTotal: 1000 kB\nMemAvailable: 500 kB\n")
        .expect("a host that freed memory");
    let second = ssh.meminfo().expect("the host answered again");
    assert_eq!(
        second.available,
        500 * 1024,
        "the second reading is the host's"
    );
    assert_ne!(first.available, second.available);
    assert_eq!(rig.calls().len(), 2);
    let _ = std::fs::remove_dir_all(&rig.dir);
}

/// A host that does not answer is `None`: the caller has to say *unknown*, and the answer it
/// must never get instead is the daemon's own box.
#[test]
fn a_silent_host_is_unknown() {
    let rig = MemRig::new("silent").silent();
    let ssh = rig.ssh("mem-silent");
    assert_eq!(
        ssh.meminfo(),
        None,
        "a host that says nothing has no numbers"
    );
    let _ = std::fs::remove_dir_all(&rig.dir);
}

/// Pattern B's door, unit by unit: the argv carries a reverse socket forward in front of the
/// destination, the command carries the variables the three programs read, and the far side's
/// helper is a script rather than a program it must already have.
///
/// The argv is what a fake ssh has to tolerate, so this is the whole of what a placement can
/// prove on a box with no other machine: the forward exists, names BOTH ends, and rides every
/// command the connection runs.
#[test]
fn a_placement_carries_the_askpass_forward_in_its_ssh_argv() {
    use super::*;
    let h = Hosts::parse(FILE).expect("it parses");
    let host = h.get("lab2x1").expect("lab2x1").clone();
    let (dir, paths) = paths_for("askpass", &host.name, &host.hostname);
    let declared = paths.default_key("lab2x1");
    let host = Host {
        key: Some(declared),
        ..host
    };
    let ssh = Ssh::connect(host, Some(PathBuf::from("/bin/false")), paths.clone())
        .expect("the connection opens")
        .with_askpass(AskpassForward {
            dir: "/home/op/letibot-remote/askpass/s179".to_string(),
            ..AskpassForward::new("/run/user/1000/letibot/harnessd-ask.sock", "s-179")
        });
    let argv = ssh.argv("bash -lc 'true'");
    let line = argv.join(" ");
    assert!(
        line.contains(
            "-R /home/op/letibot-remote/askpass/s179/ask.sock:/run/user/1000/letibot/harnessd-ask.sock"
        ),
        "the forward names both ends: {line}"
    );
    assert!(
        line.contains("-o StreamLocalBindUnlink=yes"),
        "a stale socket is replaced rather than refusing the command: {line}"
    );
    // In front of the destination, which is where ssh's own options belong: a forward after it
    // would be read as part of the remote command.
    let at = argv
        .iter()
        .position(|a| a == "dead@lab2x1")
        .expect("the destination");
    let forward = argv.iter().position(|a| a == "-R").expect("-R");
    assert!(forward < at, "{line}");

    // And the command the far side runs carries what the three programs read.
    let command = ssh.remote_command("sudo apt-get -y upgrade");
    for want in [
        "SUDO_ASKPASS=/home/op/letibot-remote/askpass/s179/letibot-askpass",
        "SSH_ASKPASS_REQUIRE=force",
        "GIT_ASKPASS=",
        "LETIBOT_ASKPASS_SOCKET=/home/op/letibot-remote/askpass/s179/ask.sock",
        "LETIBOT_SECRET_SESSION=s-179",
    ] {
        assert!(
            command.contains(want),
            "`{want}` is missing from: {command}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The far side's helper is three files, and neither the shim nor the helper can print something
/// that is not a secret or hang: a password that is not a password is worse than a failed `sudo`.
#[test]
fn the_far_sides_helper_refuses_rather_than_printing_something() {
    use super::*;
    let f = AskpassForward {
        dir: "/home/op/letibot-remote/askpass/s179".to_string(),
        ..AskpassForward::new("/run/user/1000/letibot/harnessd-ask.sock", "s-179")
    };
    let files: Vec<String> = f
        .files()
        .into_iter()
        .map(|(name, body, mode)| {
            assert!(!body.is_empty(), "{name} has no body");
            assert_eq!(mode, 0o755, "{name} is run by a program");
            format!("-- {name}\n{body}")
        })
        .collect();
    let all = files.join("\n");
    for want in [
        "-- shims/sudo",
        "-- letibot-askpass",
        "-- askpass.py",
        "-n|--non-interactive",
        "LETIBOT_ASKPASS_SOCKET",
        "no secret was given",
    ] {
        assert!(all.contains(want), "`{want}` is missing from:\n{all}");
    }
}
