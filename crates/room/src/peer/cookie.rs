//! The operator's cookie: one file of theirs, read by the coordinator and never written by it.
//!
//! Erlang says `~/.erlang.cookie` and this says `~/.config/letibot/cookie`; the rule is the same
//! one, and so is the reason it is a FILE and not a key in a config: a secret is a thing the
//! operator puts on a box, at a mode they can see, and no parse of a config decides whether a box
//! peers. A file that anybody else may read is refused by name rather than used, because a cookie
//! read by one more account is a cookie that is no longer the operator's alone.

use std::fmt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The file's name under the config directory.
pub const FILE: &str = "cookie";
/// How short a cookie may be before it is refused: a word a guess could reach by trying.
pub const MIN_LEN: usize = 8;
/// The longest cookie read: a file this long is a key, not the operator's shared word.
pub const MAX_LEN: usize = 512;

/// Where the cookie lives, given the config directory (`~/.config/letibot`).
pub fn path_in(config: &Path) -> PathBuf {
    config.join(FILE)
}

/// Why a box does not peer, in the operator's words. [`CookieError::Absent`] is not a failure: it
/// is every box that was never told to peer, and it is silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieError {
    /// There is no cookie file, so there is no peering at all.
    Absent,
    /// The file exists at a mode anybody else could read, and it is not used.
    Mode { mode: u32, path: PathBuf },
    /// The file is there and says nothing, or says too much.
    Length { len: usize, path: PathBuf },
    /// The file is there and could not be read.
    Unreadable { path: PathBuf, why: String },
}

impl CookieError {
    pub fn sentence(&self) -> String {
        match self {
            CookieError::Absent => {
                "no cookie file: this box does not peer with any other box".to_string()
            }
            CookieError::Mode { mode, path } => format!(
                "{} is mode {mode:04o} and a cookie must be mode 0600 — anybody who can read it \
                 could join this box's mesh. Nothing was read and nothing peers. Fix it with \
                 `chmod 600 {}`.",
                path.display(),
                path.display()
            ),
            CookieError::Length { len, path } => format!(
                "{} is {len} bytes and a cookie must be {MIN_LEN}..{MAX_LEN}: shorten it, or write \
                 a word only this LAN's boxes know.",
                path.display()
            ),
            CookieError::Unreadable { path, why } => format!(
                "{} could not be read ({why}), so nothing peers. A cookie with no newline is fine; \
                 one this process cannot open is not.",
                path.display()
            ),
        }
    }
}

impl fmt::Display for CookieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

/// The shared secret itself. Its `Debug` says how long it is and never what it is: a cookie in a
/// log or a panic message is a cookie the whole LAN has.
#[derive(Clone, PartialEq, Eq)]
pub struct Cookie(String);

impl Cookie {
    /// Read the cookie at `path`, or say why this box does not peer.
    pub fn load(path: &Path) -> Result<Cookie, CookieError> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CookieError::Absent),
            Err(e) => {
                return Err(CookieError::Unreadable {
                    path: path.to_path_buf(),
                    why: e.to_string(),
                });
            }
        };
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(CookieError::Mode {
                mode,
                path: path.to_path_buf(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|e| CookieError::Unreadable {
            path: path.to_path_buf(),
            why: e.to_string(),
        })?;
        // A file written by an editor may end in a newline or two, and one of them is not part of
        // the word two boxes agree on.
        let word = text.trim_end_matches(['\n', '\r']).trim();
        // A passphrase may hold spaces; a control character is a file that is not a word at all.
        if word.len() < MIN_LEN || word.len() > MAX_LEN || word.chars().any(char::is_control) {
            return Err(CookieError::Length {
                len: word.len(),
                path: path.to_path_buf(),
            });
        }
        Ok(Cookie(word.to_string()))
    }

    /// A cookie made from a word, for a test and for the coordinator that was handed one.
    pub fn of(word: impl Into<String>) -> Cookie {
        Cookie(word.into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Is this the same word, in constant time? A comparison that stopped at the first wrong byte
    /// answers a guesser one byte at a time, and the cookie is the whole of a peer's trust.
    pub fn same(&self, other: &Cookie) -> bool {
        let (a, b) = (self.0.as_bytes(), other.0.as_bytes());
        let mut diff = a.len() ^ b.len();
        for i in 0..a.len().max(b.len()) {
            diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
        }
        diff == 0
    }

    /// The proof a handshake sends: HMAC-SHA256 with the cookie as the key, hex. Written out here
    /// rather than taken from a crate for the reason the room takes no dependency it does not
    /// read - and pinned by the RFC's own vectors below, so a wrong spelling fails a test rather
    /// than a box.
    pub fn proof(&self, what: &str) -> String {
        let mac = hmac_sha256(self.0.as_bytes(), what.as_bytes());
        mac.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Is `given` this cookie's proof of `what`?
    pub fn proves(&self, what: &str, given: &str) -> bool {
        let mine = self.proof(what);
        let (a, b) = (given.as_bytes(), mine.as_bytes());
        let mut diff = a.len() ^ b.len();
        for i in 0..a.len().max(b.len()) {
            diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
        }
        diff == 0
    }
}

impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cookie(<{} bytes>)", self.0.len())
    }
}

/// HMAC-SHA256 as RFC 2104 defines it, over `sha2`'s SHA-256.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn a_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lb-peer-cookie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    fn write_at(dir: &Path, word: &str, mode: u32) -> PathBuf {
        let path = path_in(dir);
        std::fs::write(&path, word).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn the_rfcs_own_vectors_pin_the_hmac() {
        // RFC 4231 cases 1 and 6 - a short key and one longer than the block, so both arms of the
        // key schedule are pinned by the RFC rather than by this file.
        assert_eq!(
            Cookie::of("Jefe").proof("what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let long = hmac_sha256(
            &[0xaau8; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        let hex: String = long.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_cookie_is_read_from_a_private_file_and_nothing_its_mode_allows_is_used() {
        let dir = a_dir("mode");
        // Absent is not a failure: it is a box that was never told to peer.
        assert_eq!(Cookie::load(&path_in(&dir)), Err(CookieError::Absent));
        let path = write_at(&dir, "hunter2hunter2\n", 0o644);
        let refused = Cookie::load(&path).expect_err("mode 0644");
        assert!(matches!(refused, CookieError::Mode { mode: 0o644, .. }));
        // Refused by name, and the sentence says what to do.
        let said = refused.sentence();
        assert!(said.contains("mode 0600") && said.contains(&path.display().to_string()));
        assert!(
            !said.contains("hunter2"),
            "a refusal never quotes the secret"
        );
        // 0600 is read, and the trailing newline an editor left is not part of the word.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let cookie = Cookie::load(&path).expect("a private cookie");
        assert_eq!(cookie, Cookie::of("hunter2hunter2"));
        assert_eq!(cookie.len(), 14);
        // And a file with nothing in it is refused, not read as the empty word.
        let path = write_at(&dir, "  \n", 0o600);
        assert!(matches!(
            Cookie::load(&path),
            Err(CookieError::Length { .. })
        ));
        // Nor is a word a guess could reach.
        let path = write_at(&dir, "abc\n", 0o600);
        assert!(matches!(
            Cookie::load(&path),
            Err(CookieError::Length { len: 3, .. })
        ));
    }

    #[test]
    fn a_cookie_answers_only_its_own_word_and_never_draws_itself() {
        let mine = Cookie::of("a word for the lan");
        assert!(mine.same(&Cookie::of("a word for the lan")));
        assert!(!mine.same(&Cookie::of("a word for the wan")));
        assert!(!mine.same(&Cookie::of("a word for the lan ")));
        // A proof is of one sentence: the second challenge cannot be answered with the first's.
        let a = mine.proof("lbp1-a:11");
        assert!(mine.proves("lbp1-a:11", &a));
        assert!(!mine.proves("lbp1-b:11", &a));
        assert!(!Cookie::of("another word").proves("lbp1-a:11", &a));
        // 64 hex characters, whatever the cookie's length.
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(format!("{mine:?}"), "Cookie(<18 bytes>)");
    }
}
