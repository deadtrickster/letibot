//! A seat's stable identity: `<host>/<local>`, minted once and never again.
//!
//! Three things key on it - a todo wait, an ask and an inbox cursor - so it may not move when the
//! seat reconnects, when its name changes, or when the box is renamed. The host half is the box's
//! short name *at minting*, pinned in a file ([`seat_host`]); the local half is `s-<session>` for a
//! letibot session and `cc-<Claude Code session id>` for a bridge. A nick ([`crate::chat::valid_nick`])
//! is a label: display only, changeable, and nothing but a reader's eyes depends on it.

use std::fmt;
use std::path::{Path, PathBuf};

/// The prefix a session's local half carries, kept from its own session id.
pub const SESSION: &str = "s-";
/// The prefix a bridge's local half carries, so an id says what kind of seat minted it.
pub const BRIDGE: &str = "cc-";
/// The file under the data directory that pins this box's half of every id it mints.
pub const HOST_FILE: &str = "seat-host";
/// How many characters of a host name are kept.
pub const HOST_MAX: usize = 24;
/// How many characters of a local part are kept. A Claude Code session id is 36.
pub const LOCAL_MAX: usize = 64;
/// The directory under the data directory that holds the id each working folder was minted.
pub const FOLDERS: &str = "seats";

/// A seat's id, as `Display` writes it and [`SeatId::parse`] reads it back.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SeatId {
    /// The box that minted it: `lab2x1`.
    pub host: String,
    /// What that box alone knows: `s-1792687430252160872`, `cc-<a Claude Code session id>`.
    pub local: String,
}

impl SeatId {
    pub fn new(host: impl Into<String>, local: impl Into<String>) -> Self {
        SeatId {
            host: host.into(),
            local: local.into(),
        }
    }

    /// A letibot session's seat: its session id is already the local half.
    pub fn of_session(host: &str, session: &str) -> Self {
        let local = local_part(session, "");
        let local = if local.starts_with(SESSION) {
            local
        } else {
            format!("{SESSION}{local}")
        };
        SeatId::new(host, local)
    }

    /// A bridge's seat: `cc-` and the Claude Code session id Claude Code passes its MCP servers.
    /// Two bridges of one Claude Code session are one seat on purpose - the plugin's server and a
    /// user-scope one - which is what `Hello.share` is for.
    pub fn of_bridge(host: &str, claude_session: &str) -> Self {
        let local = local_part(claude_session, BRIDGE);
        SeatId::new(host, local)
    }

    /// A bridge's seat with no Claude Code session id to mint from: this process, and when it was
    /// asked. The folder's last id is what a bridge without the variable reuses
    /// ([`read_folder`]); this is only for the second bridge in a folder, whose seat is new.
    pub fn fresh_bridge(host: &str, stamp: &str) -> Self {
        let local = local_part(stamp, BRIDGE);
        SeatId::new(host, local)
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn local(&self) -> &str {
        &self.local
    }

    /// Was this seat minted by a Claude Code bridge?
    pub fn is_bridge(&self) -> bool {
        self.local == BRIDGE.trim_end_matches('-') || self.local.starts_with(BRIDGE)
    }

    /// Was it minted by a letibot session?
    pub fn is_session(&self) -> bool {
        self.local.starts_with(SESSION)
    }

    /// The same seat with another local half - what a twin takes when the folder's id is held.
    pub fn with_local(&self, local: impl Into<String>) -> Self {
        SeatId::new(self.host.clone(), local)
    }

    /// Is this an id at all: two halves, one `/`, each of the shape a minting writes?
    pub fn valid(&self) -> bool {
        !self.host.is_empty()
            && self.host == short_name(&self.host)
            && !self.local.is_empty()
            && self.local.len() <= LOCAL_MAX
            && self
                .local
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }

    /// Read an id back. `None` for a handle, a session id on its own, or anything a minting would
    /// not have written - so this is the one test that tells an id from a name.
    pub fn parse(raw: &str) -> Option<SeatId> {
        let raw = raw.trim();
        let (host, local) = raw.split_once('/')?;
        let id = SeatId::new(host, local);
        (id.valid() && !local.contains('/')).then_some(id)
    }
}

impl fmt::Display for SeatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.host, self.local)
    }
}

/// One half of an id, made the shape [`SeatId::valid`] accepts: lowercase, `[a-z0-9-]`, no edge
/// `-`. `keep` is a prefix the half already has and must not lose.
fn local_part(raw: &str, keep: &str) -> String {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    let room = LOCAL_MAX.saturating_sub(keep.len());
    let cut = cleaned.chars().take(room).collect::<String>();
    let cut = cut.trim_end_matches('-');
    if cut.is_empty() {
        // An empty half is no id: the prefix alone still says which kind of seat minted it.
        return keep.trim_end_matches('-').to_string();
    }
    format!("{keep}{cut}")
}

/// A box's short name: the first label of what `uname -n` answered, `lab1x2.h.khaprov.com` →
/// `lab1x2`, made the shape an id's host half keeps. `host` when nothing survives.
pub fn short_name(raw: &str) -> String {
    let first = raw.trim().split('.').next().unwrap_or_default();
    let mut safe: String = first
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    safe.truncate(HOST_MAX);
    let safe = safe.trim_matches('-').to_string();
    if safe.is_empty() {
        "host".to_string()
    } else {
        safe
    }
}

/// This box's half of every id it mints, pinned in `<dir>/seat-host` the first time it is asked.
///
/// Pinned and not derived: a box is renamed (`lab1x2` becomes something else, or DHCP hands out a
/// new name), and an id issued before the rename must still name the box it was issued on - a
/// migration, a wait or an ask that moved with the rename would point at a seat that never existed.
/// A file that cannot be written answers the derived name: a bridge must still connect.
pub fn seat_host(dir: &Path) -> String {
    let file = dir.join(HOST_FILE);
    if let Ok(text) = std::fs::read_to_string(&file) {
        let pinned = text.trim();
        if !pinned.is_empty() && pinned == short_name(pinned) {
            return pinned.to_string();
        }
    }
    let minted = short_name(&crate::lease::host_name());
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&file, format!("{minted}\n"));
    minted
}

/// Where a working folder's last minted id is kept, keyed by the folder's canonical path - the same
/// key the bridge keeps its nick under, so one folder is one row of both.
pub fn folder_file(dir: &Path, cwd: &Path) -> PathBuf {
    dir.join(FOLDERS).join(folder_key(cwd))
}

/// A folder's key: FNV-1a over its canonical path, sixteen hex characters.
pub fn folder_key(cwd: &Path) -> String {
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canon.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The id this working folder's bridge was last minted, if it was: what a bridge with no
/// `CLAUDE_CODE_SESSION_ID` reuses. An id whose host half moved (this is a copy of another box's
/// data directory) is not this box's to hand out again.
pub fn read_folder(dir: &Path, cwd: &Path) -> Option<SeatId> {
    let text = std::fs::read_to_string(folder_file(dir, cwd)).ok()?;
    SeatId::parse(text.trim())
}

/// Keep `id` as the one this working folder's bridge speaks as.
pub fn write_folder(dir: &Path, cwd: &Path, id: &SeatId) -> Result<(), String> {
    let file = folder_file(dir, cwd);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot keep the seat in {}: {e}", parent.display()))?;
    }
    std::fs::write(&file, format!("{id}\n"))
        .map_err(|e| format!("cannot keep the seat in {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letibot-seat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    #[test]
    fn an_id_is_a_host_and_a_local_half_that_read_back() {
        let id = SeatId::of_session("lab2x1", "s-1792687430252160872");
        assert_eq!(id.to_string(), "lab2x1/s-1792687430252160872");
        assert_eq!(id, SeatId::parse("lab2x1/s-1792687430252160872").unwrap());
        assert!(id.is_session() && !id.is_bridge());

        let bridge = SeatId::of_bridge("lab2x1", "B7B1C0DE-4C2A-4C5E-9B8A-1B2C3D4E5F60");
        assert_eq!(
            bridge.to_string(),
            "lab2x1/cc-b7b1c0de-4c2a-4c5e-9b8a-1b2c3d4e5f60"
        );
        assert!(bridge.is_bridge() && !bridge.is_session());

        // The host half is the short name, so a fully qualified one is not what a minting writes.
        assert!(SeatId::parse("lab2x1.h.khaprov.com/s-1").is_none());
        // A handle, a bare session id and a name are not ids: this is the one id-or-name test.
        for name in [
            "cc@etcd",
            "cc@etcd:db-fuzz",
            "s-1792687430252160872",
            "db-fuzz",
            "",
        ] {
            assert!(SeatId::parse(name).is_none(), "{name}");
        }
        // Two slashes are no id either, and neither is an empty half.
        assert!(SeatId::parse("lab2x1/s-1/extra").is_none());
        assert!(SeatId::parse("lab2x1/").is_none());
        assert!(SeatId::parse("/s-1").is_none());
    }

    #[test]
    fn the_short_name_is_the_first_label_made_safe() {
        assert_eq!(short_name("lab1x2.h.khaprov.com"), "lab1x2");
        assert_eq!(short_name("Lab2X1"), "lab2x1");
        assert_eq!(short_name("  mac.fritz.box "), "mac");
        let long = short_name("a-very-long-host-name-that-keeps-going.fritz.box");
        assert_eq!(long, "a-very-long-host-name-th");
        assert!(long.len() <= HOST_MAX);
        assert_eq!(short_name(""), "host");
        assert_eq!(short_name("..."), "host");
    }

    #[test]
    fn the_host_half_is_pinned_so_a_rename_does_not_reissue_an_id() {
        let dir = a_dir("pinned");
        // What a box minted under its old name: the file is the answer, not the machine.
        std::fs::write(dir.join(HOST_FILE), "lab9x9\n").unwrap();
        assert_eq!(seat_host(&dir), "lab9x9");
        // A file that is not a name this box could have minted is replaced, not used.
        std::fs::write(dir.join(HOST_FILE), "not a name\n").unwrap();
        let minted = seat_host(&dir);
        assert_eq!(minted, short_name(&crate::lease::host_name()));
        assert_eq!(seat_host(&dir), minted, "the second ask reads the file");
    }

    #[test]
    fn a_folder_keeps_the_id_its_bridge_was_minted_and_reuses_it() {
        let dir = a_dir("folder");
        let cwd = dir.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        assert_eq!(read_folder(&dir, &cwd), None);
        let id = SeatId::of_bridge("lab2x1", "2E4A9C1B-0000-4000-8000-000000000001");
        write_folder(&dir, &cwd, &id).unwrap();
        assert_eq!(read_folder(&dir, &cwd), Some(id.clone()));
        // The file is the id and nothing else, so a hand-written one is read back the same way.
        let kept = std::fs::read_to_string(folder_file(&dir, &cwd)).unwrap();
        assert_eq!(kept.trim(), id.to_string());
        // Two folders are two rows, and the same folder spelled with a `.` is one row.
        let other = dir.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(read_folder(&dir, &other), None);
        assert_eq!(read_folder(&dir, &cwd.join(".")), Some(id));
    }

    #[test]
    fn a_local_half_keeps_its_prefix_and_loses_everything_a_name_cannot_hold() {
        // Nothing to mint from still leaves an id with a valid local half.
        let id = SeatId::of_bridge("lab2x1", "  ");
        assert_eq!(id.local(), BRIDGE.trim_end_matches('-'));
        assert!(id.valid() && id.is_bridge(), "{id}");
        let id = SeatId::of_session("lab2x1", "S-1792687430252160872");
        assert_eq!(id.local(), "s-1792687430252160872");
        let long = SeatId::of_bridge("lab2x1", &"9".repeat(200));
        assert_eq!(long.local().len(), LOCAL_MAX);
        assert!(long.valid());
    }

    #[test]
    fn a_fresh_bridge_is_its_own_seat() {
        let a = SeatId::fresh_bridge("lab2x1", "4242-1700000000000");
        assert_eq!(a.to_string(), "lab2x1/cc-4242-1700000000000");
        assert!(a.is_bridge() && a.valid());
        assert_ne!(a, SeatId::fresh_bridge("lab2x1", "4243-1700000000000"));
    }
}
