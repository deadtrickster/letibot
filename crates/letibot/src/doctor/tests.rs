//! Every check against a fixture under a temp dir. Nothing here reads the operator's home,
//! config, store or `/proc`: each path the checks take is injected.

use super::*;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU32, Ordering};

struct Fx(PathBuf);

impl Fx {
    fn new(name: &str) -> Fx {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-doctor-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Fx(d)
    }
    fn at(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn file(&self, rel: &str, body: &[u8]) -> PathBuf {
        let p = self.at(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }
    fn exe(&self, rel: &str, body: &[u8]) -> PathBuf {
        let p = self.file(rel, body);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
    fn mode(&self, rel: &str, mode: u32) {
        std::fs::set_permissions(self.at(rel), std::fs::Permissions::from_mode(mode)).unwrap();
    }
    /// A fake `/proc/<pid>`: exe link (any text, so `(deleted)` works), cmdline, cwd, stat.
    fn proc(&self, pid: u32, ppid: u32, exe: &str, argv: &[&str], cwd: &Path, start_ticks: u64) {
        let d = self.at(&format!("proc/{pid}"));
        std::fs::create_dir_all(&d).unwrap();
        symlink(exe, d.join("exe")).unwrap();
        symlink(cwd, d.join("cwd")).unwrap();
        std::fs::write(d.join("cmdline"), argv.join("\0") + "\0").unwrap();
        // pid (comm) state ppid ... field 22 is starttime: 19 fields after the comm.
        let mut f: Vec<String> = vec!["S".into(), ppid.to_string()];
        f.extend((0..17).map(|_| "0".to_string()));
        f.push(start_ticks.to_string());
        std::fs::write(d.join("stat"), format!("{pid} (x) {}\n", f.join(" "))).unwrap();
        std::fs::create_dir_all(d.join("fd")).unwrap();
    }
    fn env(&self) -> Env {
        Env {
            home: self.at("home"),
            config_dir: self.at("home/.config/letibot"),
            data_dir: self.at("home/.local/share/letibot"),
            path: vec![self.at("bin")],
            uid: 4242,
            cgroup_root: self.at("cgroup"),
            proc_root: self.at("proc"),
            build_dir: self.at("build"),
            cargo_target_dir: None,
            room_socket: Some(self.at("room.sock")),
            endpoint_env: None,
            store_env: None,
            user: "tester".into(),
            dev_kvm: self.at("dev/kvm"),
            group_file: self.at("etc/group"),
            net_dir: self.at("net"),
            tmp: self.at("tmp"),
            run_programs: false,
            cpu_sample: Duration::ZERO,
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn find<'a>(checks: &'a [Check], label: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("no row {label:?} in {checks:#?}"))
}

// --- cgroups

fn manager(fx: &Fx, controllers: &str, subtree: &str) -> PathBuf {
    let d = fx.at("cgroup/user.slice/user-4242.slice/user@4242.service");
    fx.file("cgroup/cgroup.controllers", b"cpu io memory pids");
    fx.file(
        "cgroup/user.slice/user-4242.slice/user@4242.service/cgroup.controllers",
        controllers.as_bytes(),
    );
    fx.file(
        "cgroup/user.slice/user-4242.slice/user@4242.service/cgroup.subtree_control",
        subtree.as_bytes(),
    );
    d
}

#[test]
fn stock_manager_warns_for_io_and_fixes_with_the_drop_in() {
    let fx = Fx::new("cg-stock");
    let d = manager(&fx, "cpu memory pids\n", "cpu memory pids\n");
    let c = cgroups_with(&fx.env(), &d, Some("cpu memory pids"));
    let ctl = find(&c, "user manager controllers");
    assert_eq!(ctl.status, Status::Warn);
    assert!(
        ctl.fix
            .iter()
            .any(|l| l.contains("Delegate=cpu memory pids io"))
    );
    let live = find(&c, "io live in subtree_control");
    assert_eq!(live.status, Status::Warn);
    assert!(
        live.fix
            .iter()
            .any(|l| l.starts_with("echo +io > ") && l.contains("user@4242.service"))
    );
    assert!(live.fix.iter().any(|l| l.contains("daemon-reload")));
    assert_eq!(find(&c, "DelegateControllers").status, Status::Warn);
}

#[test]
fn delegated_but_not_live_asks_only_for_the_echo() {
    let fx = Fx::new("cg-half");
    let d = manager(&fx, "cpu io memory pids", "cpu memory pids");
    let c = cgroups_with(&fx.env(), &d, Some("cpu io memory pids"));
    assert_eq!(find(&c, "user manager controllers").status, Status::Ok);
    let live = find(&c, "io live in subtree_control");
    assert_eq!(live.status, Status::Warn);
    assert_eq!(live.fix.len(), 1, "{:?}", live.fix);
    assert_eq!(find(&c, "DelegateControllers").status, Status::Ok);
}

#[test]
fn full_delegation_is_all_ok_and_missing_memory_fails() {
    let fx = Fx::new("cg-full");
    let d = manager(&fx, "cpu io memory pids", "cpu io memory pids");
    let c = cgroups_with(&fx.env(), &d, Some("cpu io memory pids"));
    assert!(c.iter().all(|c| c.status == Status::Ok), "{c:#?}");
    let d = manager(&fx, "cpu pids", "cpu pids");
    let c = cgroups_with(&fx.env(), &d, None);
    assert_eq!(find(&c, "user manager controllers").status, Status::Fail);
    assert_eq!(find(&c, "DelegateControllers").status, Status::Info);
}

// --- binaries

#[test]
fn an_elf_named_letibot_fails_and_a_script_passes() {
    let fx = Fx::new("launcher");
    let elf = fx.exe("bin/letibot", b"\x7fELF\x02\x01\x01rest");
    assert_eq!(launcher_is_script(Some(&elf)).status, Status::Fail);
    let sh = fx.exe("bin2/letibot", b"#!/usr/bin/env bash\necho hi\n");
    assert_eq!(launcher_is_script(Some(&sh)).status, Status::Ok);
    assert_eq!(launcher_is_script(None).status, Status::Fail);
    // Through PATH, as `binaries` finds it.
    let env = fx.env();
    let row = binaries(&env);
    assert_eq!(
        find(&row, "letibot is the launcher script").status,
        Status::Fail
    );
}

#[test]
fn askpass_is_the_multicall_by_inode_or_bytes() {
    let fx = Fx::new("askpass");
    let multi = fx.exe("bin/letibot-multi", b"\x7fELF multicall");
    std::fs::hard_link(&multi, fx.at("bin/letibot-askpass")).unwrap();
    let c = askpass_is_multicall(Some(&fx.at("bin/letibot-askpass")), Some(&multi));
    assert_eq!((c.status, c.value.as_str()), (Status::Ok, "same inode"));
    let copy = fx.exe("b2/letibot-askpass", b"\x7fELF multicall");
    let c = askpass_is_multicall(Some(&copy), Some(&multi));
    assert_eq!((c.status, c.value.as_str()), (Status::Ok, "same bytes"));
    let other = fx.exe("b3/letibot-askpass", b"\x7fELF somethingelse");
    let c = askpass_is_multicall(Some(&other), Some(&multi));
    assert_eq!(c.status, Status::Fail);
    assert!(c.fix[0].starts_with("ln -sf letibot-multi "), "{:?}", c.fix);
}

#[test]
fn versions_are_parsed_and_must_agree() {
    assert_eq!(
        parse_version("harnessd 0.9.1\n"),
        Some(("0.9.1".into(), None))
    );
    assert_eq!(
        parse_version("letibot-mcp 0.9.1 (abc1234)"),
        Some(("0.9.1".into(), Some("abc1234".into())))
    );
    let same = [
        ("a".into(), "1".into(), None),
        ("b".into(), "1".into(), None),
    ];
    assert_eq!(versions_agree(&same).status, Status::Ok);
    let skew = [
        ("a".into(), "1".into(), None),
        ("b".into(), "2".into(), None),
    ];
    assert_eq!(versions_agree(&skew).status, Status::Fail);
    let commits = [
        ("a".into(), "1".into(), Some("aaaaaaa".into())),
        ("b".into(), "1".into(), Some("bbbbbbb".into())),
    ];
    assert_eq!(versions_agree(&commits).status, Status::Fail);
}

#[test]
fn a_missing_binary_fails_with_the_install_line() {
    let fx = Fx::new("missing");
    let c = binaries(&fx.env());
    let h = find(&c, "harnessd on PATH");
    assert_eq!(h.status, Status::Fail);
    assert_eq!(h.fix, vec![INSTALL.to_string()]);
    assert_eq!(find(&c, "letibot-mcp on PATH").status, Status::Warn);
}

// --- daemons

fn boot(fx: &Fx, btime: u64) {
    fx.file("proc/stat", format!("cpu 0 0\nbtime {btime}\n").as_bytes());
}

#[test]
fn a_daemon_on_a_deleted_or_older_binary_is_listed_by_folder() {
    let fx = Fx::new("daemons");
    let installed = fx.exe("bin/harnessd", b"\x7fELF");
    let built = mtime_secs(&installed).unwrap();
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    boot(&fx, built - 10_000);
    let w = fx.at("ws");
    std::fs::create_dir_all(&w).unwrap();
    // Started long before the install: stale.
    fx.proc(
        10,
        1,
        "/x/harnessd",
        &["harnessd", "--workspace", "/w/old"],
        &w,
        0,
    );
    // Deleted binary: stale whatever its age.
    fx.proc(
        11,
        1,
        "/x/harnessd (deleted)",
        &["harnessd"],
        &w,
        20_000 * hz,
    );
    // Started after the install: current.
    fx.proc(
        12,
        1,
        "/x/harnessd",
        &["harnessd", "--workspace=/w/new"],
        &w,
        20_000 * hz,
    );
    // Not a daemon.
    fx.proc(13, 1, "/usr/bin/bash", &["bash"], &w, 0);
    let c = stale_daemons(&fx.at("proc"), Some(&installed));
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(c.value.starts_with("2 of 3"), "{}", c.value);
    let fix = c.fix.join("\n");
    assert!(fix.contains("cd /w/old && letibot --restart"), "{fix}");
    assert!(
        fix.contains(&format!("cd {} && letibot --restart", w.display())),
        "{fix}"
    );
    assert!(!fix.contains("/w/new"), "{fix}");
}

#[test]
fn no_daemons_is_info_and_scope_is_checked_from_the_cgroup_file() {
    let fx = Fx::new("scope");
    std::fs::create_dir_all(fx.at("proc")).unwrap();
    assert_eq!(stale_daemons(&fx.at("proc"), None).status, Status::Info);
    let w = fx.at("ws");
    std::fs::create_dir_all(&w).unwrap();
    fx.proc(20, 1, "/x/harnessd", &["harnessd"], &w, 0);
    fx.file("proc/20/cgroup", b"0::/user.slice/app.slice/tmux.scope\n");
    assert_eq!(daemons_in_scope(&fx.at("proc")).status, Status::Warn);
    fx.file(
        "proc/20/cgroup",
        b"0::/user.slice/letibot.slice/letibot-harnessd-ab-1.scope/daemon\n",
    );
    assert_eq!(daemons_in_scope(&fx.at("proc")).status, Status::Ok);
}

#[test]
fn the_room_socket_is_reachable_only_when_something_listens() {
    let fx = Fx::new("room");
    let sock = fx.at("room.sock");
    assert_eq!(room_reachable(Some(&sock)).status, Status::Warn);
    let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    assert_eq!(room_reachable(Some(&sock)).status, Status::Ok);
}

#[test]
fn a_store_env_naming_the_db_file_warns() {
    assert_eq!(store_env(None).status, Status::Ok);
    let c = store_env(Some("/home/x/.local/share/letibot/sessions.db"));
    assert_eq!(c.status, Status::Warn);
    assert!(c.fix[0].starts_with("unset LETIBOT_STORE"));
}

// --- config

#[test]
fn unknown_keys_are_listed_by_dotted_path() {
    let lc = cref::find("leticode.toml").unwrap();
    let doc: toml::Table = "[exec]\nforeground_seconds = 60\nbogus = 1\n[embeddings]\nmode = \"off\"\n[nosuch]\nx = 1\n"
        .parse()
        .unwrap();
    assert_eq!(unknown_keys(lc, &doc), vec!["exec.bogus", "nosuch"]);
    let pv = cref::find("providers.toml").unwrap();
    let doc: toml::Table = "[model.\"local.qwen\"]\nurl = \"http://127.0.0.1:8080\"\nmystery = 2\n"
        .parse()
        .unwrap();
    assert_eq!(unknown_keys(pv, &doc), vec!["model.local.qwen.mystery"]);
}

#[test]
fn providers_errors_never_quote_the_file() {
    let fx = Fx::new("cfg-secret");
    let dir = fx.at("cfg");
    fx.file(
        "cfg/providers.toml",
        b"[deepseek]\nkey = \"sk-SECRET-VALUE\n",
    );
    let (c, _) = config_file(&dir, "providers.toml", true);
    assert_eq!(c.status, Status::Warn);
    let all = format!("{} {}", c.value, c.fix.join(" "));
    assert!(!all.contains("SECRET"), "{all}");
    assert!(c.value.contains("line 2"), "{}", c.value);
    // A parsing file with a key prints the key's NAME only, never its value.
    fx.file(
        "cfg/providers.toml",
        b"[deepseek]\nkey = \"sk-SECRET-VALUE\"\nzzz = \"sk-OTHER\"\n",
    );
    let (c, _) = config_file(&dir, "providers.toml", true);
    let all = format!("{} {}", c.value, c.fix.join(" "));
    assert!(!all.contains("SECRET") && !all.contains("OTHER"), "{all}");
}

#[test]
fn config_rows_for_absent_good_and_bad_files() {
    let fx = Fx::new("cfg");
    let env = fx.env();
    let c = config(&env);
    assert_eq!(find(&c, "providers.toml parses").status, Status::Info);
    fx.file(
        "home/.config/letibot/leticode.toml",
        b"[exec]\nforeground_seconds = 60\n",
    );
    fx.file("home/.config/letibot/hosts.toml", b"this is not toml\n");
    fx.file(
        "home/.config/letibot/providers.toml",
        b"[default]\nprovider = \"glm\"\n",
    );
    fx.mode("home/.config/letibot/providers.toml", 0o664);
    std::fs::create_dir_all(fx.at("home/.config/letibot/keys")).unwrap();
    fx.mode("home/.config/letibot/keys", 0o755);
    let c = config(&env);
    assert_eq!(find(&c, "leticode.toml parses").status, Status::Ok);
    assert_eq!(find(&c, "hosts.toml parses").status, Status::Fail);
    assert_eq!(find(&c, "providers.toml parses").status, Status::Ok);
    let m = find(&c, "providers.toml mode 0600");
    assert_eq!(m.status, Status::Fail);
    assert!(m.fix[0].starts_with("chmod 600 "), "{:?}", m.fix);
    let k = find(&c, "keys/ mode 0700");
    assert_eq!(k.status, Status::Fail);
    assert!(k.fix[0].starts_with("chmod 700 "));
    assert_eq!(
        find(&c, "providers.toml reference header").status,
        Status::Warn
    );
    fx.mode("home/.config/letibot/providers.toml", 0o600);
    fx.mode("home/.config/letibot/keys", 0o700);
    let c = config(&env);
    assert_eq!(find(&c, "providers.toml mode 0600").status, Status::Ok);
    assert_eq!(find(&c, "keys/ mode 0700").status, Status::Ok);
}

#[test]
fn a_duplicate_table_is_named_and_hosts_tables_nest() {
    let fx = Fx::new("cfg-dup");
    let dir = fx.at("cfg");
    fx.file(
        "cfg/providers.toml",
        b"[glm]\nkey = \"sk-SECRET\"\n[glm]\nurl = \"u\"\n",
    );
    let (c, _) = config_file(&dir, "providers.toml", true);
    assert_eq!(c.status, Status::Warn);
    assert!(
        c.value.contains("duplicate key") && c.value.contains("line 3"),
        "{}",
        c.value
    );
    assert!(!c.value.contains("SECRET"), "{}", c.value);
    let hosts = cref::find("hosts.toml").unwrap();
    let doc: toml::Table = "[host.lab]\nhostname = \"lab\"\nweird = \"x\"\n"
        .parse()
        .unwrap();
    let unknown = unknown_keys(hosts, &doc);
    assert_eq!(unknown, vec!["host.lab.weird"]);
}

#[test]
fn a_current_header_is_ok() {
    let fx = Fx::new("hdr");
    let p = fx.file("leticode.toml", b"[exec]\nforeground_seconds = 60\n");
    let f = cref::find("leticode.toml").unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, cref::splice(&text, f).unwrap()).unwrap();
    assert_eq!(header_current(&p, "leticode.toml").status, Status::Ok);
}

// --- models

#[test]
fn local_servers_are_the_on_box_model_urls() {
    let doc: toml::Table = "[model.\"local.a\"]\nurl = \"http://127.0.0.1:8080\"\n\
                            [model.fleet]\nurl = \"http://10.0.0.2:8080\"\n\
                            [model.b]\nendpoint = \"localhost:9000/v1\"\n"
        .parse()
        .unwrap();
    let s = local_servers(Some(&doc), Some("127.0.0.1:8081"));
    let addrs: Vec<&str> = s.iter().map(|(_, a)| a.as_str()).collect();
    assert_eq!(
        addrs,
        vec!["127.0.0.1:8081", "localhost:9000", "127.0.0.1:8080"]
    );
}

#[test]
fn health_needs_a_200() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut b = [0u8; 256];
        let _ = s.read(&mut b);
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    assert_eq!(health(&addr), Ok(()));
    t.join().unwrap();
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = dead.local_addr().unwrap().to_string();
    drop(dead);
    assert!(health(&gone).is_err());
}

#[test]
fn internal_embeddings_need_the_model_files() {
    let fx = Fx::new("emb");
    let data = fx.at("data");
    let off: toml::Table = "[embeddings]\nmode = \"off\"\n".parse().unwrap();
    assert_eq!(embeddings(Some(&off), &data).status, Status::Info);
    let on: toml::Table = "[embeddings]\nmode = \"internal\"\n".parse().unwrap();
    let c = embeddings(Some(&on), &data);
    assert_eq!(c.status, Status::Warn);
    let dir = data
        .join("models")
        .join(letibot_embeddings::BUILTIN_MODEL)
        .join(letibot_embeddings::builtin::REVISION);
    for f in ["config.json", "tokenizer.json", "model.safetensors"] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(f), b"x").unwrap();
    }
    assert_eq!(embeddings(Some(&on), &data).status, Status::Ok);
}

// --- Claude Code

fn claude_home(fx: &Fx, settings: &str, claude_json: &str, aliases: &str) {
    fx.file("home/.claude/settings.json", settings.as_bytes());
    fx.file("home/.claude.json", claude_json.as_bytes());
    fx.file("home/.bash_aliases", aliases.as_bytes());
}

#[test]
fn a_user_scope_leticode_server_fails_and_the_plugin_must_be_enabled() {
    let fx = Fx::new("claude-dup");
    std::fs::create_dir_all(fx.at("proc")).unwrap();
    claude_home(
        &fx,
        r#"{"enabledPlugins":{"other@x":true}}"#,
        r#"{"mcpServers":{"leticode":{"command":"letibot-mcp"}},"oauthAccount":{"token":"SECRET"}}"#,
        "alias claude='claude --dangerously-load-development-channels server:leticode'\n",
    );
    let c = claude(&fx.env());
    assert_eq!(find(&c, "leticode plugin enabled").status, Status::Warn);
    let dup = find(&c, "no user-scope leticode MCP server");
    assert_eq!(dup.status, Status::Fail);
    assert_eq!(dup.fix, vec!["claude mcp remove -s user leticode"]);
    assert_eq!(find(&c, "claude channel flag").status, Status::Fail);
    let printed = render(&[Section {
        name: "c",
        checks: c,
    }]);
    assert!(!printed.contains("SECRET"), "{printed}");
}

#[test]
fn the_plugin_setup_is_ok() {
    let fx = Fx::new("claude-ok");
    std::fs::create_dir_all(fx.at("proc")).unwrap();
    claude_home(
        &fx,
        r#"{"enabledPlugins":{"leticode@leticode":true}}"#,
        r#"{"mcpServers":{"other":{}}}"#,
        "alias ls='ls -F'\nclaude() {\n  command claude --dangerously-load-development-channels plugin:leticode@leticode \"$@\"\n}\n",
    );
    let c = claude(&fx.env());
    for l in [
        "leticode plugin enabled",
        "no user-scope leticode MCP server",
        "claude channel flag",
    ] {
        assert_eq!(find(&c, l).status, Status::Ok, "{l}");
    }
}

#[test]
fn two_bridges_under_one_claude_fail_and_waiters_are_per_folder() {
    let fx = Fx::new("bridges");
    let a = fx.at("a");
    let b = fx.at("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    fx.proc(100, 1, "/v/claude", &["claude"], &a, 0);
    fx.proc(101, 100, "/i/letibot-mcp", &["letibot-mcp"], &a, 0);
    fx.proc(
        102,
        100,
        "/t/target/release/letibot-mcp",
        &["/t/target/release/letibot-mcp"],
        &a,
        0,
    );
    fx.proc(
        103,
        100,
        "/i/letibot-mcp",
        &["letibot-mcp", "room-wait"],
        &a,
        0,
    );
    fx.proc(200, 1, "/v/claude", &["claude"], &b, 0);
    fx.proc(
        201,
        200,
        "/i/letibot-mcp (deleted)",
        &["letibot-mcp"],
        &b,
        0,
    );
    for i in 0..5 {
        fx.proc(
            210 + i,
            200,
            "/bin/bash",
            &["bash", "-c", "until [ -s f ]; do sleep 1; done"],
            &b,
            0,
        );
    }
    let c = bridges(&fx.at("proc"), Duration::ZERO);
    let one = find(&c, "one bridge per Claude session");
    assert_eq!(one.status, Status::Fail);
    assert!(one.value.contains("claude 100"), "{}", one.value);
    assert!(!one.value.contains("claude 200"), "{}", one.value);
    let inst = find(&c, "bridges run an installed binary");
    assert_eq!(inst.status, Status::Warn);
    assert!(
        inst.value.contains("102") && inst.value.contains("201"),
        "{}",
        inst.value
    );
    let w = find(&c, "room waiter per Claude folder");
    assert_eq!(w.status, Status::Warn);
    assert!(
        w.value.contains(&b.display().to_string())
            && !w.value.contains(&format!("{} ", a.display()))
    );
    assert_eq!(find(&c, "no runaway wait loops").status, Status::Warn);
}

// --- host

#[test]
fn kvm_names_the_group_fix() {
    let fx = Fx::new("kvm");
    let env = fx.env();
    assert_eq!(
        kvm(&env.dev_kvm, &env.group_file, "tester").status,
        Status::Info
    );
    let dev = fx.file("dev/kvm", b"");
    fx.mode("dev/kvm", 0o000);
    fx.file("etc/group", b"root:x:0:\nkvm:x:991:\n");
    let c = kvm(&dev, &env.group_file, "tester");
    // A root test run can open a 000 file; the verdict then is ok.
    if c.status != Status::Ok {
        assert_eq!(c.status, Status::Warn);
        assert_eq!(
            c.fix[0],
            "sudo usermod -aG kvm tester   # then log out and back in"
        );
        fx.file("etc/group", b"kvm:x:991:other,tester\n");
        let c = kvm(&dev, &env.group_file, "tester");
        assert!(c.value.contains("this login is not"), "{}", c.value);
    }
    fx.mode("dev/kvm", 0o666);
    assert_eq!(kvm(&dev, &env.group_file, "tester").status, Status::Ok);
}

#[test]
fn drkonqi_failed_units_load_and_tmp() {
    assert_eq!(drkonqi(None).status, Status::Ok);
    assert_eq!(drkonqi(Some("masked\n")).status, Status::Ok);
    let c = drkonqi(Some("enabled\n"));
    assert_eq!(c.status, Status::Warn);
    assert_eq!(
        c.fix[0],
        "systemctl --user mask --now drkonqi-coredump-launcher.socket"
    );
    assert_eq!(failed_units(Some(3)).status, Status::Ok);
    assert_eq!(failed_units(Some(128_000)).status, Status::Warn);
    assert_eq!(load(Some("10.0 9 8 1/2 3"), 32).status, Status::Ok);
    assert_eq!(load(Some("136.2 9 8 1/2 3"), 32).status, Status::Warn);
    let mounts = "/dev/sda1 / ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n";
    assert_eq!(
        tmp_check(Some("/dev/sda1 / ext4 rw 0 0\n"), None, Path::new("/tmp")).status,
        Status::Ok
    );
    assert_eq!(mount_of(mounts, Path::new("/tmp/x")).unwrap().0, "tmpfs");
}

// --- disk

#[test]
fn floors_parse_and_judge_free_space() {
    assert_eq!(parse_floor("20G"), Some(Floor::Bytes(20 << 30)));
    assert_eq!(parse_floor("5%"), Some(Floor::Fraction(0.05)));
    assert_eq!(parse_floor("0.1"), Some(Floor::Fraction(0.1)));
    assert_eq!(parse_floor("junk"), None);
    let doc: toml::Table = "[exec]\ndisk_floor = \"20G\"\n".parse().unwrap();
    assert_eq!(disk_floor(Some(&doc)).0, Floor::Bytes(20 << 30));
    assert_eq!(disk_floor(None).0, Floor::Fraction(0.05));
    let fx = Fx::new("free");
    assert_eq!(
        free_check("x", &fx.0, Floor::Bytes(0), "0").status,
        Status::Ok
    );
    assert_eq!(
        free_check("x", &fx.0, Floor::Fraction(1.01), "all").status,
        Status::Fail
    );
}

#[test]
fn a_target_dir_without_a_live_gate_warns_with_its_rm() {
    let fx = Fx::new("targets");
    fx.file("build/target-dead/debug/big", &vec![1u8; 8192]);
    fx.file("build/target-live/debug/x", b"x");
    fx.file("build/other/x", b"x");
    fx.proc(
        300,
        1,
        "/usr/bin/cargo",
        &["cargo", "build"],
        &fx.at("build/target-live/debug"),
        0,
    );
    let c = targets(&fx.at("build"), &fx.at("proc"), Duration::from_secs(5));
    assert_eq!(c.len(), 2, "{c:#?}");
    let dead = find(&c, &fx.at("build/target-dead").display().to_string());
    assert_eq!(dead.status, Status::Warn);
    assert_eq!(
        dead.fix,
        vec![format!("rm -rf {}", fx.at("build/target-dead").display())]
    );
    let live = find(&c, &fx.at("build/target-live").display().to_string());
    assert_eq!(live.status, Status::Ok);
}

#[test]
fn an_old_gate_lock_holder_and_zero_byte_refs_warn() {
    let fx = Fx::new("lock");
    let lock = fx.file("build/gate.lock", b"");
    boot(&fx, 1_000);
    let w = fx.at("w");
    std::fs::create_dir_all(&w).unwrap();
    fx.proc(400, 1, "/bin/bash", &["bash", "gate-at.sh"], &w, 0);
    symlink(&lock, fx.at("proc/400/fd/3")).unwrap();
    let c = gate_lock(&lock, &fx.at("proc"), 1_000 + 3 * 3600);
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(c.fix.iter().any(|l| l.starts_with("kill 400")));
    assert_eq!(
        gate_lock(&lock, &fx.at("proc"), 1_000 + 60).status,
        Status::Ok
    );

    fx.file("Projects/p/repo/.git/refs/heads/main", b"abc\n");
    assert_eq!(zero_refs(&fx.at("Projects")).status, Status::Ok);
    fx.file("Projects/p/repo/.git/refs/heads/empty", b"");
    let c = zero_refs(&fx.at("Projects"));
    assert_eq!(c.status, Status::Warn);
    assert!(c.fix[0].starts_with("find ") && c.fix[0].ends_with("-type f -size 0 -delete"));
}

// --- firecode, output

#[test]
fn firecode_summary_and_taps() {
    let out = "x\n\n  19 ok, 1 warning(s), 2 failure(s)\n\n  what to do:\n";
    assert_eq!(
        firecode_summary(out).as_deref(),
        Some("19 ok, 1 warning(s), 2 failure(s)")
    );
    let fx = Fx::new("taps");
    std::fs::create_dir_all(fx.at("net/eno1")).unwrap();
    let fc = Path::new("/opt/firecode");
    let c = taps(&fx.at("net"), fc);
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.fix[0], "sudo /opt/firecode net-setup --count 14");
    std::fs::create_dir_all(fx.at("net/fccode1")).unwrap();
    assert_eq!(taps(&fx.at("net"), fc).status, Status::Ok);
}

#[test]
fn exit_codes_render_and_json() {
    let s = |st: Status| Section {
        name: "s",
        checks: vec![row(Status::Ok, "a", "1"), row(st, "b", "2").fix(["do it"])],
    };
    assert_eq!(exit_code(&[s(Status::Info)]), 0);
    assert_eq!(exit_code(&[s(Status::Warn)]), 1);
    assert_eq!(exit_code(&[s(Status::Warn), s(Status::Fail)]), 2);
    let text = render(&[s(Status::Fail)]);
    assert!(text.contains("  FAIL  b"), "{text}");
    assert!(text.contains("2 - see below"), "{text}");
    assert!(
        text.contains("what to do:\n    b\n        do it\n"),
        "{text}"
    );
    assert!(text.contains("not checked here"), "{text}");
    let j: serde_json::Value = serde_json::from_str(&to_json(&[s(Status::Warn)])).unwrap();
    assert_eq!(j["exit"], 1);
    assert_eq!(j["summary"]["warn"], 1);
    assert_eq!(j["sections"][0]["checks"][1]["status"], "warn");
    assert_eq!(j["sections"][0]["checks"][1]["fix"][0], "do it");
}

#[test]
fn the_whole_run_on_an_empty_fixture_reads_nothing_real() {
    let fx = Fx::new("all");
    std::fs::create_dir_all(fx.at("proc")).unwrap();
    let sections = all(&fx.env());
    let names: Vec<&str> = sections.iter().map(|s| s.name).collect();
    assert!(
        !names.contains(&"cgroups"),
        "no cgroup fixture, no section: {names:?}"
    );
    assert_eq!(exit_code(&sections), 2, "no binaries is a FAIL");
}
