//! No test file spawns a letibot binary with the session's doors still on it: a file that names a
//! `CARGO_BIN_EXE_` binary goes through `letibot_testenv` (its `command`, or `isolate` when the
//! spawn is a tool's or a shell's), and none hands that path straight to `Command::new`.

use std::path::{Path, PathBuf};

/// Binaries that are fixtures rather than letibot: they dial no daemon.
const NOT_LETIBOT: &[&str] = &["letibot-mcp-fake"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The binaries a file names through `CARGO_BIN_EXE_<name>`.
fn named_binaries(text: &str) -> Vec<String> {
    text.split("CARGO_BIN_EXE_")
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect()
        })
        .filter(|n: &String| !n.is_empty())
        .collect()
}

#[test]
fn every_test_that_spawns_a_letibot_binary_goes_through_testenv() {
    let mut files = Vec::new();
    for c in std::fs::read_dir(root().join("crates")).unwrap().flatten() {
        rust_files(&c.path().join("tests"), &mut files);
    }
    files.sort();
    assert!(
        files.len() > 50,
        "the walk found the tests: {}",
        files.len()
    );
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let rel = f.strip_prefix(root()).unwrap_or(f).display().to_string();
        let letibot = named_binaries(&text)
            .into_iter()
            .any(|b| !NOT_LETIBOT.contains(&b.as_str()));
        if !letibot {
            continue;
        }
        if !text.contains("letibot_testenv::") {
            bad.push(format!(
                "{rel}: names a letibot binary and never uses letibot_testenv"
            ));
        }
        if text.contains("Command::new(env!(\"CARGO_BIN_EXE_") {
            bad.push(format!(
                "{rel}: `Command::new(env!(\"CARGO_BIN_EXE_..\"))` keeps the doors; use \
                 letibot_testenv::command"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The lint can fail: a file of the shape it refuses is refused.
#[test]
fn the_lint_sees_a_bare_spawn() {
    let text = "let c = std::process::Command::new(env!(\"CARGO_BIN_EXE_harnessd\"));";
    assert_eq!(named_binaries(text), vec!["harnessd".to_string()]);
    assert!(!text.contains("letibot_testenv::"));
}
