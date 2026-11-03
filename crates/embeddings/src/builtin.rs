//! The built-in model, pinned: a commit on Hugging Face, three files, and a hash for each.
//!
//! `mode = "internal"` with no `model` is this checkpoint, and none of it is compiled in. The
//! pin is a commit rather than a branch and a hash rather than a length, so the bytes a box
//! fetches can be checked against the bytes somebody reviewed, and any other answer is refused.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::BUILTIN_MODEL;

/// The commit the three files are pinned at. A branch would move under a box that fetched
/// before it moved, so this is a full hash, and every URL below is built under it.
pub const REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

/// The repository's `resolve` root, with `/<revision>/<file>` under it.
pub const BASE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve";

/// One file of the checkpoint: what it is called, what it must hash to, and what it weighs -
/// the last only so the one sentence before a fetch can say how much is coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub file: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// **The three files, and the trust in them.** `sha256` is what the bytes must hash to, and
/// `bytes` is how much the one sentence before a fetch promises is coming.
pub const SOURCES: [Source; 3] = [
    Source {
        file: "config.json",
        sha256: "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
        bytes: 612,
    },
    Source {
        file: "tokenizer.json",
        sha256: "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
        bytes: 466_247,
    },
    Source {
        file: "model.safetensors",
        sha256: "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
        bytes: 90_868_376,
    },
];

/// A pin a fetch runs against: the shipped table, or a test's own bytes, which is the only
/// way to exercise a hash check without the network.
#[derive(Debug, Clone)]
pub struct Pin {
    base: String,
    revision: String,
    sources: Vec<Source>,
}

impl Pin {
    /// The table this build ships.
    pub fn shipped() -> Pin {
        Pin {
            base: BASE.to_string(),
            revision: REVISION.to_string(),
            sources: SOURCES.to_vec(),
        }
    }

    /// A pin over files a caller names, which is what a test serves from its own socket.
    pub fn new(base: impl Into<String>, revision: impl Into<String>, sources: Vec<Source>) -> Pin {
        Pin {
            base: base.into(),
            revision: revision.into(),
            sources,
        }
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Where this pin keeps its files: one directory per revision, so a moved pin is a new
    /// directory and never a half-replaced one.
    pub fn dir(&self, data_dir: &Path) -> PathBuf {
        data_dir
            .join("models")
            .join(BUILTIN_MODEL)
            .join(&self.revision)
    }

    /// The URL one file is served at.
    pub fn url(&self, file: &str) -> String {
        format!("{}/{}/{}", self.base, self.revision, file)
    }

    /// **Is the pin a real one?** A commit hash and a SHA-256 are fixed lengths, so a string of
    /// the wrong length, one that is not hexadecimal, or the all-zero filler a table is written
    /// with before its values are known is a pin no fetch may run under.
    pub fn is_filled(&self) -> bool {
        pinned(&self.revision, 40) && self.sources.iter().all(|s| pinned(s.sha256, 64))
    }

    /// Are all three files already here? Checked before anything is fetched, so a box that
    /// paid for the model once starts without reaching the network at all.
    pub fn present(&self, dir: &Path) -> bool {
        self.sources.iter().all(|s| dir.join(s.file).is_file())
    }
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A pin's own spelling: `len` hexadecimal digits, and not the filler a table holds before its
/// values are known.
fn pinned(text: &str, len: usize) -> bool {
    is_hex(text, len) && text.bytes().any(|b| b != b'0')
}

/// Why the built-in model is not in the data directory, in the words the one sentence a box
/// says about it uses. Every variant names what to act on: a URL, a file, or a path.
#[derive(Debug)]
pub enum DownloadError {
    /// The pin is not a commit and a hash per file, so this build does not know which bytes to
    /// trust.
    Unpinned,
    /// A URL this build's client cannot fetch.
    Url(String),
    /// No answer, or an answer that was refused, after the ladder.
    Http {
        file: &'static str,
        url: String,
        why: String,
    },
    /// The bytes are not the ones the pin names, so they were not renamed into place.
    Hash {
        file: &'static str,
        url: String,
        want: String,
        got: String,
    },
    /// The data directory would not take a file.
    Io { path: PathBuf, why: String },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Unpinned => write!(
                f,
                "the built-in model's pin is not a full commit and a sha-256 for every file, so \
                 nothing was fetched"
            ),
            DownloadError::Url(url) => write!(
                f,
                "the built-in model is at {url}, and this build's HTTP client cannot fetch it: \
                 the client speaks http:// only, with no TLS"
            ),
            DownloadError::Http { file, url, why } => {
                write!(f, "fetching {file} from {url}: {why}")
            }
            DownloadError::Hash {
                file,
                url,
                want,
                got,
            } => write!(
                f,
                "{url} served {file} with sha-256 {got} and the pin says {want}, so it was not \
                 renamed into place and the built-in model is still absent"
            ),
            DownloadError::Io { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

impl std::error::Error for DownloadError {}

/// **Make the pinned model present in the data directory, fetching it if it is not.** Called
/// where the embedder is opened, which is daemon start, so a fetch never runs inside a turn.
/// `say` is handed one sentence and only when a fetch is about to happen.
pub fn ensure(
    pin: &Pin,
    data_dir: &Path,
    policy: letibot_http::retry::Policy,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, DownloadError> {
    let dir = pin.dir(data_dir);
    if pin.present(&dir) {
        return Ok(dir);
    }
    if !pin.is_filled() {
        return Err(DownloadError::Unpinned);
    }
    say(&sentence(pin, &dir));
    std::fs::create_dir_all(&dir).map_err(|e| DownloadError::Io {
        path: dir.clone(),
        why: e.to_string(),
    })?;
    for source in pin.sources() {
        // One file in place is one file that hashed right: a rename is the last act, so a
        // fetch that stopped midway is resumed and not repeated.
        if dir.join(source.file).is_file() {
            continue;
        }
        fetch(source, &pin.url(source.file), &dir, policy)?;
    }
    Ok(dir)
}

/// The one sentence a box says before it fetches: what, from where, how much, where it is
/// kept, and the fact that it happens once.
fn sentence(pin: &Pin, dir: &Path) -> String {
    let bytes: u64 = pin.sources().iter().map(|s| s.bytes).sum();
    format!(
        "the built-in embedding model {} at commit {} is not in {}, so it is being fetched from \
         {} ({} files, {}): this happens once, and every score this box makes is the words alone \
         until it lands",
        crate::model::BUILTIN_MODEL,
        &pin.revision()[..pin.revision().len().min(12)],
        dir.display(),
        pin.base,
        pin.sources().len(),
        weigh(bytes)
    )
}

fn weigh(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

/// One file, over the ladder: a transport failure or a *not now* is tried again after the
/// wait the tree's rule names, and a refusal, a hash mismatch or a disk error ends it at once.
fn fetch(
    source: &Source,
    url: &str,
    dir: &Path,
    policy: letibot_http::retry::Policy,
) -> Result<(), DownloadError> {
    let (endpoint, path) = split(url)?;
    let temp = dir.join(format!("{}.part", source.file));
    let mut ladder = letibot_http::retry::Ladder::new(policy);
    loop {
        match attempt(&endpoint, &path, &temp, source, url) {
            Ok(()) => return Ok(()),
            Err(Attempt::Http(e)) => {
                let Some(wait) = ladder.failed(&e) else {
                    let _ = std::fs::remove_file(&temp);
                    return Err(DownloadError::Http {
                        file: source.file,
                        url: url.to_string(),
                        why: ladder.ending(&e),
                    });
                };
                std::thread::sleep(wait);
            }
            Err(Attempt::Done(e)) => {
                let _ = std::fs::remove_file(&temp);
                return Err(e);
            }
        }
    }
}

/// A failure of one attempt: one the ladder may take again, and one it may not.
enum Attempt {
    Http(letibot_http::HttpError),
    Done(DownloadError),
}

/// One request, into the temp name, hashed on the way in. The rename is the only way a file
/// reaches its real name, and it happens only for bytes that matched the pin.
fn attempt(
    endpoint: &letibot_http::Endpoint,
    path: &str,
    temp: &Path,
    source: &Source,
    url: &str,
) -> Result<(), Attempt> {
    let body =
        letibot_http::send(endpoint, letibot_http::Request::get(path)).map_err(Attempt::Http)?;
    let file = std::fs::File::create(temp).map_err(|e| io(temp, e))?;
    let mut sink = Hashing {
        out: file,
        hash: Sha256::new(),
    };
    body.read_to_writer(&mut sink).map_err(Attempt::Http)?;
    let (got, file) = sink.finish();
    drop(file);
    if got != source.sha256 {
        return Err(Attempt::Done(DownloadError::Hash {
            file: source.file,
            url: url.to_string(),
            want: source.sha256.to_string(),
            got,
        }));
    }
    let into = temp.with_file_name(source.file);
    std::fs::rename(temp, &into).map_err(|e| io(&into, e))
}

fn io(path: &Path, e: std::io::Error) -> Attempt {
    Attempt::Done(DownloadError::Io {
        path: path.to_path_buf(),
        why: e.to_string(),
    })
}

/// `http://HOST[:PORT][/path]` into the two halves this client takes. A scheme it cannot
/// speak is refused by name rather than attempted: it is HTTP/1.1 with no TLS.
fn split(url: &str) -> Result<(letibot_http::Endpoint, String), DownloadError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| DownloadError::Url(url.to_string()))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(DownloadError::Url(url.to_string()));
    }
    let endpoint = if authority.contains(':') {
        letibot_http::Endpoint::parse(authority)
            .map_err(|e| DownloadError::Url(format!("{url}: {e}")))?
    } else {
        letibot_http::Endpoint::new(authority, 80)
    };
    Ok((endpoint, path.to_string()))
}

/// A writer that hashes what it writes. The pin is checked on the bytes as they land, so a
/// hundred megabytes is never held in memory to be hashed afterwards.
struct Hashing<W> {
    out: W,
    hash: Sha256,
}

impl<W> Hashing<W> {
    fn finish(self) -> (String, W) {
        (format!("{:x}", self.hash.finalize()), self.out)
    }
}

impl<W: std::io::Write> std::io::Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.out.write(buf)?;
        self.hash.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{Checkpoint, Dir, Fake, answer};
    use crate::testing as fixture;

    use letibot_http::retry::Policy;

    /// **The pin is the whole trust story.** A box that fetched whatever a branch held would
    /// embed with bytes nobody reviewed, so the shipped table is a full commit and a full
    /// sha-256 per file, and none of the four is the all-zero filler.
    #[test]
    fn the_builtin_pin_names_a_commit_and_a_hash_for_every_file() {
        let pin = Pin::shipped();
        assert!(
            is_hex(pin.revision(), 40) && pin.revision().bytes().any(|b| b != b'0'),
            "the built-in revision is {:?}, and a pin is a full 40-hex commit",
            pin.revision()
        );
        for source in pin.sources() {
            assert!(
                is_hex(source.sha256, 64) && source.sha256.bytes().any(|b| b != b'0'),
                "{} is pinned at {:?}, and a pin is a full 64-hex sha-256",
                source.file,
                source.sha256
            );
            assert!(
                source.bytes > 0,
                "{} is weighed {} bytes, and the sentence before a fetch says how much is \
                 coming",
                source.file,
                source.bytes
            );
        }
        assert!(pin.is_filled(), "the shipped pin is a real one");
    }

    /// One directory per revision, so a moved pin fetches into a new place instead of mixing
    /// its files with the ones already there.
    #[test]
    fn the_builtin_model_is_one_directory_per_revision() {
        let dir = Pin::shipped().dir(Path::new("/data"));
        assert_eq!(
            dir,
            Path::new("/data")
                .join("models")
                .join(BUILTIN_MODEL)
                .join(REVISION)
        );
        assert!(
            Pin::shipped()
                .url("config.json")
                .ends_with(&format!("/{REVISION}/config.json"))
        );
    }

    /// **The first use fetches the three files and the second start does not.** The bytes
    /// served are a checkpoint this build opens, so the assertion is not that files appeared
    /// but that the model underneath them works.
    #[test]
    fn the_builtin_model_is_fetched_once_and_then_found_on_disk() {
        let files = Checkpoint::new();
        let fake = Fake::new(files.route());
        let pin = files.pin(&fake.base());
        let data = Dir::new("fetched");

        let mut said: Vec<String> = Vec::new();
        let dir = ensure(&pin, data.path(), Policy::none(), &mut |line| {
            said.push(line.to_string())
        })
        .expect("the fetch");
        assert!(pin.present(&dir), "{} is not whole", dir.display());
        assert_eq!(fake.requests(), 3, "one request per file");

        // The sentence before the fetch: what, from where, how much, and that it is once.
        let sentence = said.join("\n");
        assert_eq!(said.len(), 1, "{sentence}");
        assert!(sentence.contains(crate::model::BUILTIN_MODEL), "{sentence}");
        assert!(sentence.contains(&fake.base()), "{sentence}");
        assert!(sentence.contains(&REVISION[..12]), "{sentence}");
        assert!(sentence.contains("once"), "{sentence}");
        assert!(sentence.contains(&dir.display().to_string()), "{sentence}");

        // What was fetched opens as the model it is, from the shipped revision's directory.
        let model = crate::model::Model::open_builtin(data.path()).expect("the fetched model");
        assert_eq!(model.dim(), fixture::HIDDEN);

        // A second start finds it: no request at all, and nothing is said.
        let mut again: Vec<String> = Vec::new();
        let second = ensure(&pin, data.path(), Policy::none(), &mut |line| {
            again.push(line.to_string())
        })
        .expect("the cache");
        assert_eq!(second, dir);
        assert_eq!(
            fake.requests(),
            3,
            "a model already on disk is not fetched again"
        );
        assert!(again.is_empty(), "nothing is fetched, so nothing is said");
    }

    /// A file whose bytes are not the pinned ones is refused and never renamed into place:
    /// the directory is not left holding a checkpoint made of one good file and one bad.
    #[test]
    fn a_file_that_does_not_hash_to_the_pin_is_not_renamed_into_place() {
        let files = Checkpoint::new();
        let served = files.files.clone();
        let fake = Fake::new(move |path| {
            let file = path.rsplit('/').next().unwrap_or("");
            match served.get(file) {
                // One byte changed: the length is right and the hash is not.
                Some(bytes) if file == "config.json" => {
                    let mut altered = bytes.clone();
                    altered[0] ^= 0xff;
                    answer(&altered)
                }
                Some(bytes) => answer(bytes),
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            }
        });
        let pin = files.pin(&fake.base());
        let data = Dir::new("hash");
        let err = ensure(&pin, data.path(), Policy::none(), &mut |_| {})
            .expect_err("a hash mismatch is a refusal");
        assert!(
            matches!(
                err,
                DownloadError::Hash {
                    file: "config.json",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("sha-256"), "{err}");
        assert!(err.to_string().contains(&pin.url("config.json")), "{err}");
        let dir = pin.dir(data.path());
        assert!(!dir.join("config.json").is_file(), "it was renamed anyway");
        assert!(!dir.join("config.json.part").exists(), "the temp was left");
        assert!(!pin.present(&dir));
    }

    /// A body that stops early is a failure and not a short file: the pin is what says so,
    /// and the temp name is what keeps it out of the model.
    #[test]
    fn a_cut_off_download_leaves_nothing_behind() {
        let fake = Fake::new(|path| {
            let file = path.rsplit('/').next().unwrap_or("");
            // A length the connection then does not honour, which is what a dropped
            // connection looks like to the reader.
            let mut out = b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n".to_vec();
            out.extend_from_slice(file.as_bytes());
            out
        });
        let files = Checkpoint::new();
        let pin = files.pin(&fake.base());
        let data = Dir::new("cut");
        let err = ensure(&pin, data.path(), Policy::none(), &mut |_| {})
            .expect_err("a cut-off body is a refusal");
        assert!(
            matches!(
                err,
                DownloadError::Http {
                    file: "config.json",
                    ..
                }
            ),
            "{err}"
        );
        let said = err.to_string();
        assert!(said.contains("config.json"), "{said}");
        assert!(said.contains(&pin.url("config.json")), "{said}");
        let dir = pin.dir(data.path());
        assert!(
            !dir.join("config.json").is_file(),
            "a short file was renamed"
        );
        assert!(!dir.join("config.json.part").exists(), "the temp was left");
    }

    /// A pin that is not filled cannot fetch anything, and it must not reach the server to find
    /// that out: a box must never embed with bytes nobody pinned.
    #[test]
    fn an_unfilled_pin_is_refused_before_anything_is_fetched() {
        let fake = Fake::new(|_| b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n".to_vec());
        let data = Dir::new("unpinned");
        let filler = Pin::new(BASE, "0".repeat(40), Pin::shipped().sources().to_vec());
        let err = ensure(&filler, data.path(), Policy::none(), &mut |_| {
            panic!("nothing is announced when nothing is fetched")
        })
        .expect_err("an unfilled pin is a refusal");
        assert!(matches!(err, DownloadError::Unpinned), "{err}");
        assert_eq!(fake.requests(), 0);
    }
}
