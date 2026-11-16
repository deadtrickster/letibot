//! A `#!/bin/sh` that is dash drops every env var whose name is not a shell identifier, so
//! `CARGO_BIN_EXE_<bin-with-hyphen>` does not survive one. Read at compile time it fails the
//! build; read at run time it falls back to some other binary without a word.

use std::path::Path;
use std::process::Command;

#[test]
fn no_test_reads_a_cargo_bin_path_at_run_time() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "--", "*.rs"])
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git ls-files failed");
    let me = file!().rsplit('/').next().unwrap_or_default().to_string();
    let mut hits = Vec::new();
    for rel in String::from_utf8_lossy(&out.stdout).lines() {
        if rel.ends_with(&me) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            let runtime =
                line.contains("var(\"CARGO_BIN_EXE_") || line.contains("var_os(\"CARGO_BIN_EXE_");
            if runtime {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "read CARGO_BIN_EXE_* with env!(), not at run time:\n{}",
        hits.join("\n")
    );
}
