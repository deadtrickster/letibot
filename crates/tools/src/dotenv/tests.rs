//! The `.env` parser, the precedence a command's env is built in, and the carry rules measured
//! through a real spawn. A box with no cgroup v2 still runs these: `PlainTree` is a scope tree over
//! plain directories, so the host path under test is the host's own and not a fake of it.

use std::sync::Arc;

use serde_json::json;

use crate::backend::tempdir::TempDir;
use crate::builtins::retrieval::Unavailable;
use crate::events::RecordingToolSink;
use crate::exec::ProcessHost;
use crate::runtime::ToolRuntime;
use letibot_transcript::ToolCall;

use super::*;

fn vars(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    pairs.iter().cloned().collect()
}

/// The grammar, one row per fact: `export`, comments, quotes, escapes, expansion, CRLF, UTF-8, and
/// the two shapes that are not a pair at all.
#[test]
fn the_parser_reads_dotenv_basics() {
    // A raw string, so the file's own backslashes stay the file's: inside the double quotes `\n`
    // is an escape the parser reads, and `\\` is one literal backslash.
    let mut text = String::from(
        r##"# a comment
export EXPORTED=one
PLAIN=two

SPACED =  around  
SINGLE='a $literal # not a comment'
DOUBLE="line1\nline2\t\"q\" \\ \$HOME"
EXPANDED=${EXPORTED}-${BASE}-${UNSET}
UTF8=héllo → ✓
INLINE_HASH=abc#def
NOT A PAIR
MISSING_QUOTE="unclosed
9BAD=key
EMPTY=
"##,
    );
    // The CRLF line is the last one, because a raw string cannot hold a carriage return: it is the
    // fact that a Windows checkout's file reads the same as a Unix one's.
    text.push_str("CRLF=windows\r\n");
    let base = |name: &str| (name == "BASE").then(|| "base".to_string());
    let parsed = parse(&text, &base);
    let got = vars(&parsed.pairs);
    assert_eq!(got["EXPORTED"], "one");
    assert_eq!(got["PLAIN"], "two");
    // An unquoted value is trimmed of surrounding space and keeps its own `#`.
    assert_eq!(got["SPACED"], "around");
    assert_eq!(got["INLINE_HASH"], "abc#def");
    // Single quotes are literal: no expansion, no escape, `#` included.
    assert_eq!(got["SINGLE"], "a $literal # not a comment");
    // Double quotes expand `\n`, `\t`, `\"`, `\\` and `\$`, and leave `$HOME` alone for having been
    // escaped - the one spelling that says "this is not a reference".
    assert_eq!(got["DOUBLE"], "line1\nline2\t\"q\" \\ $HOME");
    // An earlier key in the file, then the base environment, then an unset name as empty.
    assert_eq!(got["EXPANDED"], "one-base-");
    assert_eq!(got["CRLF"], "windows");
    assert_eq!(got["UTF8"], "héllo → ✓");
    assert_eq!(got["EMPTY"], "");
    // The lines that are not pairs, by number: `NOT A PAIR` (11), the unclosed quote (12), the
    // name that cannot be a shell name (13). The comment, the blank line and the valid ones are
    // not reported.
    assert_eq!(parsed.bad, vec![11, 12, 13]);
}

/// Nothing in a file runs: a backtick or `$(…)` is data, which is what makes a `.env` safe to read.
#[test]
fn the_parser_never_runs_anything() {
    let parsed = parse(
        "A=$(touch /tmp/letibot-dotenv-ran)\nB=`touch /tmp/letibot-dotenv-ran`\n",
        &|_| None,
    );
    assert_eq!(vars(&parsed.pairs)["A"], "$(touch /tmp/letibot-dotenv-ran)");
    assert_eq!(vars(&parsed.pairs)["B"], "`touch /tmp/letibot-dotenv-ran`");
    assert!(!std::path::Path::new("/tmp/letibot-dotenv-ran").exists());
}

/// The later file wins, and a line that is not a pair is said once, by number.
#[test]
fn a_later_file_wins_and_a_broken_line_is_said_once() {
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join(".env"),
        "ONE=first\nTWO=kept\nbroken line\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".env.local"), "ONE=second\n").unwrap();
    let (pairs, said) = read(dir.path());
    let got = vars(&pairs);
    assert_eq!(got["ONE"], "second", ".env.local overrides .env");
    assert_eq!(got["TWO"], "kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("line 3"),
        "the NUMBER, not the content: {said:?}"
    );
    assert!(
        !said[0].contains("broken"),
        "the content is not repeated: {said:?}"
    );
    // The same version again says nothing: a session's every call reads the same file.
    let (again, said) = read(dir.path());
    assert_eq!(vars(&again)["ONE"], "second");
    assert!(said.is_empty(), "{said:?}");
}

/// A file whose mtime moved is read again, and its pairs replace the earlier version's.
#[test]
fn a_changed_file_is_read_again() {
    let dir = TempDir::new();
    let at = dir.path().join(".env");
    std::fs::write(&at, "SWITCH=before\n").unwrap();
    assert_eq!(vars(&read(dir.path()).0)["SWITCH"], "before");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::write(&at, "SWITCH=after\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&at)
        .unwrap()
        .set_modified(later)
        .expect("an mtime");
    let (pairs, said) = read(dir.path());
    assert_eq!(vars(&pairs)["SWITCH"], "after");
    assert!(said.is_empty(), "{said:?}");
}

/// `[dotenv]`: the switch, the host list, and a key nobody knows refused by name.
#[test]
fn the_table_is_read_and_an_unknown_key_refused() {
    let path = std::path::Path::new("/box/leticode.toml");
    let s = Settings::parse(path, "[dotenv]\ncarry_to = [\"xmg\"]\ncarry_to_vm = true\n").unwrap();
    assert!(s.load, "load defaults to true");
    assert!(s.carries_to("xmg"));
    assert!(s.carries_to_vm("xmg"));
    assert!(!s.carries_to("lab2x1"), "a host that was not named");
    assert!(
        !Settings::default().carries_to_vm("xmg"),
        "a VM is off by default"
    );

    let off = Settings::parse(path, "[dotenv]\nload = false\n").unwrap();
    assert!(!off.load);

    let typo = Settings::parse(path, "[dotenv]\ncarrytoo = [\"xmg\"]\n").unwrap_err();
    assert!(
        typo.contains("carrytoo") || typo.contains("unknown field"),
        "{typo}"
    );
}

/// A tree, a real process host over plain directories, and a runtime with `bash` and an allowing
/// gate: the path a session's command takes.
fn runner(root: &std::path::Path) -> ToolRuntime {
    let tree = TempDir::new();
    let host = crate::exec::HostProcesses::with_tree(
        root.to_path_buf(),
        crate::exec::host::tests::PlainTree::over(tree.path().join("scopes")),
    );
    // The tree is held by the host's own scope paths, which live under `tree`; a `TempDir` that
    // went away here would take the already-open scopes with it, so it is leaked on purpose.
    std::mem::forget(tree);
    let backend = crate::backend::HostBackend::executable_with(root, Arc::new(host))
        .expect("a backend over the fixture");
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins register");
    ToolRuntime::new(registry, Box::new(backend)).with_gate(crate::testing::allow_all())
}

/// One `bash` call: the rendered payload the model is sent, and the row the store would keep.
fn bash(rt: &ToolRuntime, command: &str) -> (String, String) {
    let mut sink = RecordingToolSink::new();
    let call = ToolCall {
        id: "call_env".into(),
        name: "bash".into(),
        arguments: json!({ "command": command }).to_string(),
    };
    let result = rt.invoke("turn_1", &call, &mut sink);
    let row =
        serde_json::to_string(&ToolRuntime::transcript_item(&result)).expect("a serialisable row");
    (result.payload, row)
}

/// A command starts with the project's `.env`, and `.env.local` wins over it. The verdict is
/// decided IN the shell, because a value of eight bytes or more is masked in what comes back -
/// which is the whole point of loading it as a secret.
#[test]
fn a_command_sees_the_files_and_the_local_one_wins() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".env"), "CARRIED_BY_ENV=from-dot-env\n").unwrap();
    std::fs::write(
        dir.path().join(".env.local"),
        "CARRIED_BY_ENV=from-local\nSECOND_OF_LOCAL=two\n",
    )
    .unwrap();
    let rt = runner(dir.path());
    let (payload, _) = bash(
        &rt,
        "[ \"$CARRIED_BY_ENV\" = from-local ] && echo override-ok || echo override-bad; \
         [ \"$SECOND_OF_LOCAL\" = two ] && echo second-ok || echo second-bad",
    );
    assert!(payload.contains("override-ok"), "{payload}");
    assert!(payload.contains("second-ok"), "{payload}");
}

/// A later version of the file is read by the next call, with no new runtime and no restart.
#[test]
fn the_file_is_reread_when_it_changes() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".env"), "REREAD=first\n").unwrap();
    let rt = runner(dir.path());
    let (payload, _) = bash(
        &rt,
        "[ \"$REREAD\" = first ] && echo first-ok || echo first-bad",
    );
    assert!(payload.contains("first-ok"), "{payload}");

    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::write(dir.path().join(".env"), "REREAD=second-version\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(dir.path().join(".env"))
        .unwrap()
        .set_modified(later)
        .expect("an mtime");
    let (payload, _) = bash(
        &rt,
        "[ \"$REREAD\" = second-version ] && echo second-ok || echo second-bad",
    );
    assert!(payload.contains("second-ok"), "{payload}");
}

/// Docker compose's precedence, at the one place it is decided: a pair letibot itself set wins
/// over the file, and a pair the CALL named wins over both. Measured at the spawn, where the four
/// lists are merged, because that is the order a reading of the code has to be checked against.
#[test]
fn the_file_loses_to_letibots_own_env_and_to_the_calls_own_pair() {
    let root = TempDir::new();
    let scopes = TempDir::new();
    let host = crate::exec::HostProcesses::with_tree(
        root.path().to_path_buf(),
        crate::exec::host::tests::PlainTree::over(scopes.path().join("scopes")),
    );
    std::mem::forget(scopes);
    host.set_dotenv_env(vec![("ORDER".to_string(), "from-file".to_string())]);
    let spawn = |env: Vec<(String, String)>| {
        let out = root.path().join("said");
        // The file is read once it has bytes, so a previous spawn's answer has to go first: a stale
        // one would make the second measurement the first one's.
        let _ = std::fs::remove_file(&out);
        let req = crate::exec::SpawnRequest {
            command: format!("printf %s \"$ORDER\" > {}", out.display()),
            slug: None,
            cwd: ".".into(),
            scope: crate::exec::ScopeKind::Turn,
            scope_name: None,
            background: false,
            env,
            tty: false,
            memory: None,
            env_trailer: false,
        };
        host.spawn(&req).expect("a job runs");
        let mut saw = String::new();
        for _ in 0..600 {
            if let Ok(text) = std::fs::read_to_string(&out) {
                saw = text;
                if !saw.is_empty() {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        saw
    };
    assert_eq!(spawn(Vec::new()), "from-file");
    // letibot's own pair for the name: the daemon's, not the file's.
    host.set_confined_env(vec![("ORDER".to_string(), "from-letibot".to_string())]);
    assert_eq!(spawn(Vec::new()), "from-letibot");
    // And the call's own pair, which comes last of the four.
    assert_eq!(
        spawn(vec![("ORDER".to_string(), "from-call".to_string())]),
        "from-call"
    );
}

/// A loaded value is a secret from the moment it is read: the model's payload and the store's row
/// both get the mask, and the file on disk still holds the real bytes.
#[test]
fn a_loaded_value_never_reaches_a_row() {
    let dir = TempDir::new();
    const TOKEN: &str = "glpat-fixtureSECRET000000000000000000";
    std::fs::write(dir.path().join(".env"), format!("KNOX_TOKEN={TOKEN}\n")).unwrap();
    let rt = runner(dir.path());
    let (payload, row) = bash(&rt, "printf 'token is %s\\n' \"$KNOX_TOKEN\"");
    assert!(
        !payload.contains(TOKEN),
        "the model was sent it:\n{payload}"
    );
    assert!(!row.contains(TOKEN), "the store would keep it:\n{row}");
    assert!(
        payload.contains(&"*".repeat(TOKEN.len())),
        "the mask keeps the length so offsets still hold: {payload}"
    );
    // A redaction and not a boundary: the file is untouched.
    let on_disk = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert!(on_disk.contains(TOKEN));
}

/// The switch is the whole of `load = false`: the same call loses the pair.
#[test]
fn the_load_switch_turns_the_files_off() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".env"), "SWITCHED=on\n").unwrap();
    let tree = TempDir::new();
    let host = crate::exec::HostProcesses::with_tree(
        dir.path().to_path_buf(),
        crate::exec::host::tests::PlainTree::over(tree.path().join("scopes")),
    );
    std::mem::forget(tree);
    let backend = crate::backend::HostBackend::executable_with(dir.path(), Arc::new(host))
        .expect("a backend")
        .with_dotenv(false);
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins");
    let rt = ToolRuntime::new(registry, Box::new(backend)).with_gate(crate::testing::allow_all());
    let (payload, _) = bash(&rt, "printf '[%s]' \"$SWITCHED\"");
    assert!(
        payload.contains("[]"),
        "the pair reached a command anyway: {payload}"
    );
}

/// The operator's own case: a project `.env` names a kubeconfig, and the command `kubectl` runs
/// reads that file. Measured through the bash tool, confined where this box can make a namespace:
/// where it cannot, the same assertion runs unconfined and says which half it measured.
#[test]
fn a_kubectl_shaped_command_reads_the_config_its_env_names() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join(".kube")).unwrap();
    std::fs::write(
        dir.path().join(".kube/config"),
        "apiVersion: v1\nkind: Config\nname: the-project\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".env"), "KUBECONFIG=./.kube/config\n").unwrap();
    // A stub on `PATH`: it refuses unless the env names a file it can read, and says what it read.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // `write_stub` waits for the file to stop being busy, which a fork on the same inode makes it:
    // the fixture's own launcher forks here too.
    crate::testing::write_stub(
        &bin.join("kubectl"),
        "#!/bin/sh\n\
         [ -n \"$KUBECONFIG\" ] || { echo no-kubeconfig; exit 9; }\n\
         [ -f \"$KUBECONFIG\" ] || { echo no-such-file; exit 8; }\n\
         cat \"$KUBECONFIG\"\n",
    );

    // The confined half first: a boundary is what makes this the operator's case rather than a
    // shell's. Its absence on this box is announced, not swallowed.
    let confined = crate::backend::HostBackend::confined(dir.path()).ok();
    if confined.is_none() {
        letibot_tokencore::apparatus::absent("a bwrap user namespace for the confined half");
        eprintln!("a kubectl stub: this box cannot make a namespace, so the unconfined path runs");
    }
    let backend = confined.unwrap_or_else(|| {
        let tree = TempDir::new();
        let host = crate::exec::HostProcesses::with_tree(
            dir.path().to_path_buf(),
            crate::exec::host::tests::PlainTree::over(tree.path().join("scopes")),
        );
        std::mem::forget(tree);
        crate::backend::HostBackend::executable_with(dir.path(), Arc::new(host))
            .expect("a backend over the fixture")
    });
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins");
    let rt = ToolRuntime::new(registry, Box::new(backend)).with_gate(crate::testing::allow_all());
    let (payload, _) = bash(&rt, "PATH=./bin:$PATH kubectl; printf '[rc=%s]' \"$?\"");
    assert!(payload.contains("[rc=0]"), "{payload}");
    // The path a file gave is itself a value of eight bytes or more, so it is masked in the
    // output - the read is what this test measures, and the file's CONTENT is not a secret here.
    assert!(
        payload.contains("kind: Config"),
        "the command did not read the config the env named: {payload}"
    );
}
