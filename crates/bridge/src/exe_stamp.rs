//! Whether the file this bridge was started from has been replaced on disk. Claude Code keeps
//! one `letibot-mcp` for a conversation, so an install leaves it running the old build against
//! newer daemons, and nothing else says so.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;

struct Stamp {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

fn stamp() -> Option<&'static Stamp> {
    static STAMP: OnceLock<Option<Stamp>> = OnceLock::new();
    STAMP
        .get_or_init(|| {
            let path = std::env::current_exe().ok()?;
            let m = std::fs::metadata(&path).ok()?;
            Some(Stamp {
                dev: m.dev(),
                ino: m.ino(),
                path,
            })
        })
        .as_ref()
}

/// Taken when the bridge is made, so a later check compares against the file it was run from.
pub fn take() {
    let _ = stamp();
}

/// The refusal for a bridge whose executable now stands replaced. A missing file is a build in
/// progress, not a replacement.
pub fn refuse_if_replaced() -> Result<(), String> {
    let Some(s) = stamp() else { return Ok(()) };
    let Ok(m) = std::fs::metadata(&s.path) else {
        return Ok(());
    };
    if m.dev() == s.dev && m.ino() == s.ino {
        return Ok(());
    }
    let built = m
        .modified()
        .map(crate::daemon::rfc3339_utc)
        .unwrap_or_else(|_| "at an unknown time".into());
    Err(format!(
        "this letibot-mcp is older than the build installed at {} (built {built}), so it may \
         not speak to the daemons that build starts. Restart the MCP server to pick up build \
         {built}: in Claude Code, `/mcp`, then reconnect letibot. Nothing was started or attached.",
        s.path.display()
    ))
}
