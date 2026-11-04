//! Every shell script in this repository is executable, because the docs tell a person to run some
//! by name. The gate runs `sh scripts/...`, so nothing else notices a lost mode bit.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The repository this test was built in.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether a file is a shell script, by the program its shebang names: `/bin/sh`, and bash through
/// `env`. A `python3` shebang is a script of another kind and is not this test's subject.
fn is_shell(path: &Path) -> bool {
    let first = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().next().map(str::to_string));
    let Some(first) = first.filter(|line| line.starts_with("#!")) else {
        return false;
    };
    first
        .split(|c: char| c.is_whitespace() || c == '/')
        .any(|word| word == "sh" || word == "bash")
}

#[test]
fn every_shell_script_is_executable() {
    let mut seen = 0;
    for dir in ["scripts", "tests/acceptance", "tests/acceptance/local"] {
        let entries = std::fs::read_dir(root().join(dir))
            .unwrap_or_else(|e| panic!("no {dir} directory: {e}"));
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || !is_shell(&path) {
                continue;
            }
            seen += 1;
            let mode = std::fs::metadata(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                .permissions()
                .mode();
            assert!(
                mode & 0o111 != 0,
                "{} is {mode:o}. A shell script in this tree is executable: `docs/tui-testing.md` \
                 and `docs/gate.md` invoke some of them by path, and a rebase that rewrites a \
                 file can drop the bit while every `sh scripts/...` step stays green.",
                path.display()
            );
        }
    }
    assert!(
        seen >= 15,
        "the walk found {seen} shell scripts, which is too few for this tree"
    );
}
