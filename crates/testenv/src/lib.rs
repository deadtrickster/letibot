//! Tests kept off the box they run on: a session's `LETIBOT_*` doors reach every command it spawns,
//! so a `cargo test` inherits the operator's live daemon. [`command`] spawns without them,
//! [`isolate`] clears this process, and [`EnvGuard`] is the one lock for tests that change the env.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The `LETIBOT_*` variables a test keeps: read-only apparatus the tests measure against, and the
/// gate's own switches. Every other `LETIBOT_*` is a door to something live.
pub const KEPT: &[&str] = &[
    "LETIBOT_VOCAB_GGUF",
    "LETIBOT_GLM_GGUF",
    "LETIBOT_CALIBRATION",
    "LETIBOT_LLAMA_DIR",
    "LETIBOT_LLAMA_LIB",
    "LETIBOT_REQUIRE_APPARATUS",
    "LETIBOT_APPARATUS_REFUSALS",
    "LETIBOT_CHECK_REAL_STORE",
];

/// Whether `name` is a door: a `LETIBOT_*` variable not in [`KEPT`].
pub fn is_door(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with("LETIBOT_") && !KEPT.contains(&name)
}

/// The doors in this process's environment now.
pub fn doors() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| is_door(k))
        .collect()
}

/// `Command::new(program)` with every door removed. A variable the test sets on the command
/// afterwards wins, so a test that means to reach its own daemon says so with `.env(..)`.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    for d in doors() {
        cmd.env_remove(d);
    }
    cmd
}

static ENV: Mutex<()> = Mutex::new(());

/// The environment, held: every test in a binary that sets or removes a variable takes this one
/// lock, and the variables it changed are put back when it drops. Not reentrant: [`isolate`]
/// takes it too.
pub struct EnvGuard {
    saved: Vec<(OsString, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn lock() -> EnvGuard {
        EnvGuard {
            saved: Vec::new(),
            _lock: ENV.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    fn save(&mut self, key: &OsStr) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_owned(), std::env::var_os(key)));
        }
    }

    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.save(key.as_ref());
        // SAFETY: under ENV, which every test of this binary that writes the environment holds.
        unsafe { std::env::set_var(key, value) };
        self
    }

    pub fn remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.save(key.as_ref());
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var(key) };
        self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            // SAFETY: the lock is still held; `_lock` drops after this body.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(&k, v),
                    None => std::env::remove_var(&k),
                }
            }
        }
    }
}

/// Take the doors off this process and point its XDG dirs at a scratch of its own, once per test
/// binary; answers the scratch. `HOME` stays, so read-only apparatus under it is found. For spawns
/// this crate cannot reach (a tool, a shell, a library) and for config read through XDG.
pub fn isolate() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("letibot-testenv-{}", std::process::id()));
        let mut g = EnvGuard::lock();
        for d in doors() {
            g.remove(d);
        }
        for (var, sub) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            let p = dir.join(sub);
            std::fs::create_dir_all(&p).expect("the scratch XDG dir");
            g.set(var, &p);
        }
        // Kept for the life of the process: the guard would put the doors back.
        g.saved.clear();
        dir
    })
}
