//! Whether the executable this daemon runs has been replaced on disk. A rebuild leaves a running
//! daemon on the old, deleted file, and macOS rescans the new one at every code-signing check of
//! the process, so an idle daemon leaves and the next start runs the new build.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// How often an idle daemon looks.
pub const CHECK: Duration = Duration::from_secs(30);

struct Stamp {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Stamp {
    fn of(path: PathBuf) -> Option<Stamp> {
        let m = std::fs::metadata(&path).ok()?;
        Some(Stamp {
            dev: m.dev(),
            ino: m.ino(),
            path,
        })
    }

    fn replaced(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|m| m.dev() != self.dev || m.ino() != self.ino)
    }
}

fn stamp() -> Option<&'static Stamp> {
    static STAMP: OnceLock<Option<Stamp>> = OnceLock::new();
    STAMP
        .get_or_init(|| Stamp::of(std::env::current_exe().ok()?))
        .as_ref()
}

/// Taken at the daemon's start, so a later [`replaced`] compares against the file it was run from.
pub fn take() {
    let _ = stamp();
}

/// The executable's path when a different file now stands there. A missing file is a build in
/// progress, not a replacement.
pub fn replaced() -> Option<PathBuf> {
    let s = stamp()?;
    s.replaced().then(|| s.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_test_binary_is_not_replaced() {
        take();
        assert_eq!(replaced(), None);
    }

    #[test]
    fn a_file_renamed_over_is_replaced_and_a_missing_one_is_not() {
        let dir = std::env::temp_dir().join(format!("exe-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let bin = dir.join("bin");
        std::fs::write(&bin, "old").expect("write");
        let s = Stamp::of(bin.clone()).expect("stamped");
        assert!(!s.replaced(), "the same file");
        // Written while `bin` still holds its inode: a filesystem that hands the freed one to the
        // next create would leave two files at the same (dev, ino) and read as not replaced.
        let next = dir.join("next");
        std::fs::write(&next, "new").expect("write");
        std::fs::remove_file(&bin).expect("rm");
        assert!(!s.replaced(), "a build in progress");
        std::fs::rename(&next, &bin).expect("rename");
        assert!(s.replaced(), "a new file at the path");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
