//! Whether this Claude Code session's channel is live, and the marker a waiter reads instead of
//! guessing.
//!
//! Claude Code delivers `notifications/claude/channel` only from an MCP server the
//! `--dangerously-load-development-channels` flag names, and that flag is on the command line of the
//! `claude` process this bridge was started by - so the command line is the one place the fact can
//! be read from. When the channel is live the plugin's room waiter is a SECOND wake for the same
//! message and a second connection of the same seat, so the bridge marks the session and the waiter
//! steps aside; with no channel (or no flag) the waiter is the only wake and runs.

use std::path::{Path, PathBuf};

use crate::rooms;

/// The flag, spelled once.
pub const FLAG: &str = "--dangerously-load-development-channels";
/// The plugin whose channel this bridge can deliver.
const PLUGIN: &str = "plugin:leticode";

/// Where the mark for one seat is kept, beside the names the bridge already keeps per folder.
pub fn marker(dir: &Path, name: &str) -> PathBuf {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    dir.join("waiters").join(format!("channel-{h:016x}"))
}

/// Publish whether this session's channel is live, for the waiter to read.
pub fn publish(dir: &Path, name: &str, live: bool) {
    let file = marker(dir, name);
    if !live {
        let _ = std::fs::remove_file(&file);
        return;
    }
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &file,
        "the channel delivers room messages for this session\n",
    );
}

/// Is the channel live for the seat `name`? A mark that is there says so; nothing else does, and a
/// waiter that reads no mark is the wake.
pub fn is_live(dir: &Path, name: &str) -> bool {
    marker(dir, name).is_file()
}

/// Publish the fact for THIS process's seat: what the MCP server does once at startup, before the
/// plugin's waiter can start.
pub fn publish_for_this_seat() {
    publish(&data_dir(), &rooms::handle(), live());
}

/// Does this process's `claude` ancestor carry the development-channels flag naming this plugin?
/// The walk is bounded and answers `false` when it cannot read the process table: an unreadable
/// answer must leave the waiter running, because a waiter nobody needs costs a process and a
/// listener, and a session with no wake at all costs the message.
pub fn live() -> bool {
    let mut pid = std::process::id();
    for _ in 0..16 {
        let Some((ppid, command)) = ps(pid) else {
            return false;
        };
        if command.starts_with("claude")
            || command.contains("/claude ")
            || command.contains(" claude ")
        {
            return names_this_plugin(&command);
        }
        if ppid <= 1 {
            return false;
        }
        pid = ppid;
    }
    false
}

/// Is the flag on this command line, naming this plugin's channel (and not another server's)?
fn names_this_plugin(command: &str) -> bool {
    if !command.contains(FLAG) {
        return false;
    }
    // `server:leticode` is a hand-registered server, which Claude Code drops every frame from.
    if command.contains("server:leticode") {
        return false;
    }
    let at = match command.find(FLAG) {
        Some(at) => at + FLAG.len(),
        None => return false,
    };
    let rest: Vec<&str> = command[at..].split_whitespace().collect();
    rest.first().is_some_and(|a| a.starts_with(PLUGIN))
}

/// One process's parent and its command line, from `ps`: the same tool on every box this runs on,
/// and the one a person reading the box would use.
fn ps(pid: u32) -> Option<(u32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim_start();
    let (ppid, command) = line.split_once(char::is_whitespace)?;
    Some((ppid.trim().parse().ok()?, command.trim().to_string()))
}

/// The data directory, by the crate's own rule: here so a test can name the place a mark lives.
pub fn data_dir() -> PathBuf {
    rooms::data_dir_for_marks()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag names a SERVER, and only `plugin:leticode@…` is this bridge's channel: a
    /// hand-registered `server:leticode` is a server Claude Code drops every frame from.
    #[test]
    fn the_flag_names_this_plugins_channel_or_it_does_not() {
        assert!(names_this_plugin(
            "claude --dangerously-load-development-channels plugin:leticode@leticode"
        ));
        assert!(!names_this_plugin(
            "claude --dangerously-load-development-channels server:leticode"
        ));
        assert!(!names_this_plugin("claude --model opus"));
        // The flag names something else entirely.
        assert!(!names_this_plugin(
            "claude --dangerously-load-development-channels plugin:other@market"
        ));
        // A command line whose words merely contain the flag's letters is not the flag.
        assert!(!names_this_plugin(
            "vim notes.txt -- about --dangerously-load-development-channels-nonsense"
        ));
    }

    /// The mark is per seat, and a mark that is there is the whole of what a waiter reads.
    #[test]
    fn a_marker_says_the_channel_is_live_for_one_seat() {
        let dir = std::env::temp_dir().join(format!("bridge-channel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_live(&dir, "cc@letibot"));
        publish(&dir, "cc@letibot", true);
        assert!(is_live(&dir, "cc@letibot"));
        assert!(
            !is_live(&dir, "cc@other-folder"),
            "one session's channel says nothing about another's"
        );
        // A session restarted without the flag takes the mark down.
        publish(&dir, "cc@letibot", false);
        assert!(!is_live(&dir, "cc@letibot"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
