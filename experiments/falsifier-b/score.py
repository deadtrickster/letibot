"""Objective scorer: drop a model code block into a scratch crate, build it,
then run the rig's own tests against it.

compiles    = `cargo build --lib` succeeds on the model's code alone.
tests_pass  = `cargo test --test spec` succeeds with the rig's tests.

`cargo test --test spec` builds the lib WITHOUT cfg(test), so any tests the
model wrote itself are not compiled and cannot contribute to the score.
"""
import os, re, subprocess, shutil, pathlib

HERE = pathlib.Path(__file__).resolve().parent
TESTS_REF = HERE / "tests_ref"

FENCE = re.compile(r"```(?:rust|rs)?[ \t]*\r?\n(.*?)```", re.S)

def extract_code(text):
    """Last fenced block wins; models sometimes restate the signature first."""
    if not text:
        return None
    blocks = FENCE.findall(text)
    if blocks:
        return max(blocks, key=len).strip()
    # unfenced fallback: accept it only if it plausibly is Rust
    t = text.strip()
    if t.startswith(("pub fn", "use ", "pub struct", "fn ", "//", "#!")):
        return t
    return None

def make_crate(root: pathlib.Path, task_id: str, code: str):
    if root.exists():
        shutil.rmtree(root)
    (root / "src").mkdir(parents=True)
    (root / "tests").mkdir(parents=True)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "scratch"\nversion = "0.0.0"\nedition = "2021"\n\n'
        '[lib]\npath = "src/lib.rs"\n\n[workspace]\n')
    (root / "src" / "lib.rs").write_text(code + "\n")
    shutil.copy(TESTS_REF / f"{task_id}.rs", root / "tests" / "spec.rs")

def run(cmd, cwd, target_dir, timeout=180):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir), RUSTFLAGS="",
               CARGO_TERM_COLOR="never")
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True,
                           text=True, timeout=timeout)
        return p.returncode, (p.stdout + p.stderr)[-4000:]
    except subprocess.TimeoutExpired:
        return 124, "TIMEOUT"

def score(task_id: str, response_text: str, workdir: pathlib.Path):
    """-> dict(has_code, compiles, tests_pass, build_err, test_err)"""
    code = extract_code(response_text)
    out = {"has_code": code is not None, "compiles": False, "tests_pass": False,
           "build_err": "", "test_err": ""}
    if code is None:
        return out
    crate = workdir / "crate"
    target = workdir / "target"
    make_crate(crate, task_id, code)
    rc, log = run(["cargo", "build", "--lib", "--quiet"], crate, target)
    out["compiles"] = (rc == 0)
    if rc != 0:
        out["build_err"] = log
        return out
    rc, log = run(["cargo", "test", "--test", "spec", "--quiet"], crate, target)
    out["tests_pass"] = (rc == 0)
    if rc != 0:
        out["test_err"] = log
    return out
