//! The built-in model, pinned: a Hugging Face commit, three files, and a hash for each.
//! `mode = "internal"` with no `model` uses this checkpoint; nothing is compiled in.
//! Commit and hash pins let a box check fetched bytes against reviewed bytes and refuse any other.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::{PinOverride, Settings};
use crate::model::BUILTIN_MODEL;

/// The commit the three files are pinned at. A branch would move under a box that fetched
/// before it moved, so this is a full hash, and every URL below is built under it.
pub const REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

/// The repository the three files come from, and its `resolve` root with `/<revision>/<file>`
/// under it. A section may name another repository with `builtin_repo`.
pub const REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";
pub const BASE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve";

/// The `resolve` root of a repository a section named instead of the shipped one.
pub fn base_for(repo: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve")
}

/// One file of the checkpoint: what it is called, what it must hash to, and what it weighs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file: &'static str,
    pub sha256: String,
    /// What the file weighs, when the pin knows: the sentence before a fetch says how much is
    /// coming only for bytes somebody here measured.
    pub bytes: Option<u64>,
}

/// The three files, and the trust in them. `sha256` is what the bytes must hash to, and
/// `bytes` is how much the one sentence before a fetch promises is coming.
const SHIPPED: [(&str, &str, u64); 3] = [
    (
        "config.json",
        "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
        612,
    ),
    (
        "tokenizer.json",
        "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
        466_247,
    ),
    (
        "model.safetensors",
        "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
        90_868_376,
    ),
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
    /// The table this build ships - the shipped override, which is no override at all.
    pub fn shipped() -> Pin {
        Pin::over(&PinOverride::default())
    }

    /// The pin a `[embeddings]` section asks for: the shipped table with the repository, the
    /// commit and the hashes the section wrote over it. The parser refused a half, so this is
    /// either the shipped pin or a whole other one.
    pub fn of(settings: &Settings) -> Pin {
        Pin::over(&settings.builtin)
    }

    fn over(wrote: &PinOverride) -> Pin {
        let sources = SHIPPED
            .iter()
            .map(|(file, sha256, bytes)| Source {
                file,
                sha256: match wrote.sha256.get(*file) {
                    Some(hash) => hash.clone(),
                    None => (*sha256).to_string(),
                },
                // A hash somebody else chose comes with bytes nobody here measured.
                bytes: wrote.sha256.is_empty().then_some(*bytes),
            })
            .collect();
        Pin {
            base: match &wrote.repo {
                Some(repo) => base_for(repo),
                None => BASE.to_string(),
            },
            revision: wrote
                .revision
                .clone()
                .unwrap_or_else(|| REVISION.to_string()),
            sources,
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

    /// Is the pin a real one? A commit hash and a SHA-256 are fixed lengths, so a string of
    /// the wrong length, one that is not hexadecimal, or the all-zero filler a table is written
    /// with before its values are known is a pin no fetch may run under.
    pub fn is_filled(&self) -> bool {
        pinned(&self.revision, 40) && self.sources.iter().all(|s| pinned(&s.sha256, 64))
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
                "the built-in model is at {url}, which is not an http:// or https:// URL"
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

/// Make the pinned model present in the data directory, fetching it if it is not. Called
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
    let weighed: Option<u64> = pin.sources().iter().map(|s| s.bytes).sum();
    let files = match weighed {
        Some(bytes) => format!("{} files, {}", pin.sources().len(), weigh(bytes)),
        None => format!("{} files", pin.sources().len()),
    };
    format!(
        "the built-in embedding model {} at commit {} is not in {}, so it is being fetched from \
         {} ({files}): this happens once, and every score this box makes is the words alone \
         until it lands",
        crate::model::BUILTIN_MODEL,
        &pin.revision()[..pin.revision().len().min(12)],
        dir.display(),
        pin.base,
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
    let route = Route::of(url)?;
    let temp = dir.join(format!("{}.part", source.file));
    let mut ladder = letibot_http::retry::Ladder::new(policy);
    loop {
        match attempt(&route, &temp, source, url) {
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

/// One request, into the temp name, hashed once it is complete. The rename is the only way a
/// file reaches its real name, and it happens only for bytes that matched the pin.
fn attempt(route: &Route, temp: &Path, source: &Source, url: &str) -> Result<(), Attempt> {
    match route {
        Route::Plain(endpoint, path) => {
            let body = letibot_http::send(endpoint, letibot_http::Request::get(path))
                .map_err(Attempt::Http)?;
            let file = std::fs::File::create(temp).map_err(|e| io(temp, e))?;
            let mut sink = Hashing {
                out: file,
                hash: Sha256::new(),
            };
            body.read_to_writer(&mut sink).map_err(Attempt::Http)?;
            let (got, file) = sink.finish();
            drop(file);
            settle(got, temp, source, url)
        }
        Route::Curl => {
            curl(url, temp)?;
            let got = hash_file(temp).map_err(|e| io(temp, e))?;
            settle(got, temp, source, url)
        }
    }
}

/// Compare against the pin and rename into place.
fn settle(got: String, temp: &Path, source: &Source, url: &str) -> Result<(), Attempt> {
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

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut sink = Hashing {
        out: std::io::sink(),
        hash: Sha256::new(),
    };
    std::io::copy(&mut file, &mut sink)?;
    Ok(sink.finish().0)
}

/// Redirects curl may follow: Hugging Face `resolve` is one hop to a CDN, and a loop ends here.
const MAX_REDIRS: u32 = 5;

/// A whole-transfer ceiling, so a stalled CDN is a failed attempt the ladder retries.
const CURL_MAX_TIME_SECS: u32 = 1800;

/// The curl argv for one pinned file. https only, so a redirect cannot downgrade to http; no
/// shell is involved, and `--globoff` keeps a URL's brackets literal.
fn curl_args(url: &str, temp: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "--silent",
        "--show-error",
        "--location",
        "--globoff",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-redirs",
    ]
    .iter()
    .map(Into::into)
    .collect();
    args.push(MAX_REDIRS.to_string().into());
    args.push("--max-time".into());
    args.push(CURL_MAX_TIME_SECS.to_string().into());
    // The status is read from stdout rather than `--fail`, so the ladder can tell a 503 from a
    // 404.
    args.push("--write-out".into());
    args.push("%{http_code}".into());
    args.push("--output".into());
    args.push(temp.into());
    args.push("--".into());
    args.push(url.into());
    args
}

/// Fetch over https with curl into `temp`. A non-2xx or a transport failure is an
/// [`Attempt::Http`], so the same ladder decides whether to try again.
fn curl(url: &str, temp: &Path) -> Result<(), Attempt> {
    let out = std::process::Command::new("curl")
        .args(curl_args(url, temp))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Attempt::Http(letibot_http::HttpError::Io(std::io::Error::new(
                e.kind(),
                format!("curl could not start (is it on PATH?): {e}"),
            )))
        })?;
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if out.status.success() && (200..300).contains(&code) {
        return Ok(());
    }
    let _ = std::fs::remove_file(temp);
    if code >= 300 {
        return Err(Attempt::Http(letibot_http::HttpError::Status {
            code,
            body: String::new(),
            retry_after: None,
        }));
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(Attempt::Http(letibot_http::HttpError::Io(
        std::io::Error::other(format!("curl {}: {said}", out.status)),
    )))
}

fn io(path: &Path, e: std::io::Error) -> Attempt {
    Attempt::Done(DownloadError::Io {
        path: path.to_path_buf(),
        why: e.to_string(),
    })
}

/// How one URL is fetched: https through curl, which has TLS and redirects, and plain
/// `http://` through the tree's client, which has neither and stays that way on purpose.
enum Route {
    Plain(letibot_http::Endpoint, String),
    Curl,
}

impl Route {
    fn of(url: &str) -> Result<Route, DownloadError> {
        if url.starts_with("https://") && url.len() > "https://".len() {
            return Ok(Route::Curl);
        }
        let (endpoint, path) = split(url)?;
        Ok(Route::Plain(endpoint, path))
    }
}

/// `http://HOST[:PORT][/path]` into the two halves this client takes. A scheme it cannot
/// speak is refused by name rather than attempted.
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

    /// The pin is the whole trust story. A box that fetched whatever a branch held would
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
                is_hex(&source.sha256, 64) && source.sha256.bytes().any(|b| b != b'0'),
                "{} is pinned at {:?}, and a pin is a full 64-hex sha-256",
                source.file,
                source.sha256
            );
            assert!(
                source.bytes.is_some_and(|b| b > 0),
                "{} is weighed {:?} bytes, and the sentence before a fetch says how much is \
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

    /// A pin a section wrote is a pin of its own. Every URL, the directory the files land in
    /// and the hash each file must have follow the section, and the shipped table is what a
    /// section that wrote none of it gets.
    #[test]
    fn a_pin_a_section_wrote_follows_the_section() {
        let rev = "2222222222222222222222222222222222222222";
        let hash = "abc123".to_string() + &"0".repeat(58);
        let section = format!(
            "[embeddings]\nmode = \"internal\"\nbuiltin_repo = \"me/all-MiniLM-L6-v2\"\n\
             builtin_revision = \"{rev}\"\nbuiltin_sha256 = {{ \"config.json\" = \"{hash}\", \
             \"tokenizer.json\" = \"{hash}\", \"model.safetensors\" = \"{hash}\" }}\n"
        );
        let pin = Pin::of(&Settings::from_toml(&section).expect("the section"));
        assert_eq!(pin.revision(), rev);
        assert_eq!(
            pin.url("config.json"),
            format!("https://huggingface.co/me/all-MiniLM-L6-v2/resolve/{rev}/config.json")
        );
        assert_eq!(
            pin.dir(Path::new("/data")),
            Path::new("/data")
                .join("models")
                .join(BUILTIN_MODEL)
                .join(rev)
        );
        for source in pin.sources() {
            assert_eq!(
                source.sha256, hash,
                "{} takes the section's hash",
                source.file
            );
            assert_eq!(
                source.bytes, None,
                "a section's revision comes with bytes nobody here measured"
            );
        }
        assert!(pin.is_filled());

        let shipped = Pin::of(&Settings::from_toml("[embeddings]\nmode = \"internal\"\n").unwrap());
        assert_eq!(shipped.revision(), REVISION);
        assert_eq!(
            shipped.url("config.json"),
            Pin::shipped().url("config.json")
        );
        assert_eq!(shipped.sources(), Pin::shipped().sources());
    }

    /// A changed pin fetches its own files. The directory is keyed by the revision, so a box
    /// that moved the pin never serves another revision's bytes under the new commit's name.
    #[test]
    fn a_changed_pin_fetches_its_own_files_and_not_another_revisions() {
        let files = Checkpoint::new();
        let fake = Fake::new(files.route());
        let data = Dir::new("two-revisions");
        let first = files.pin(&fake.base());
        ensure(&first, data.path(), Policy::none(), &mut |_| {}).expect("the first revision");
        assert_eq!(fake.requests(), 3);

        let second = Pin::new(
            fake.base(),
            "1111111111111111111111111111111111111111",
            first.sources().to_vec(),
        );
        assert_ne!(second.dir(data.path()), first.dir(data.path()));
        assert!(
            !second.present(&second.dir(data.path())),
            "the first revision's files are not the second's"
        );
        ensure(&second, data.path(), Policy::none(), &mut |_| {}).expect("the second revision");
        assert_eq!(fake.requests(), 6, "fresh files, and none of them reused");
        assert!(
            first.present(&first.dir(data.path())),
            "and the first revision is still whole"
        );
    }

    /// The first use fetches the three files and the second start does not. The bytes
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

    #[test]
    fn an_https_url_goes_to_curl_and_an_http_one_stays_on_the_plain_client() {
        assert!(matches!(
            Route::of("https://huggingface.co/x/resolve/abc/f"),
            Ok(Route::Curl)
        ));
        assert!(matches!(
            Route::of("http://127.0.0.1:9/f"),
            Ok(Route::Plain(..))
        ));
        assert!(matches!(Route::of("https://"), Err(DownloadError::Url(_))));
        assert!(matches!(Route::of("ftp://h/f"), Err(DownloadError::Url(_))));
    }

    #[test]
    fn the_curl_argv_is_https_only_bounded_and_ends_at_the_url() {
        let args: Vec<String> = curl_args("https://h/f", Path::new("/d/f.part"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = |flag: &str| args.iter().position(|a| a == flag).expect(flag);
        assert_eq!(args[at("--proto") + 1], "=https");
        assert_eq!(args[at("--proto-redir") + 1], "=https");
        assert_eq!(args[at("--max-redirs") + 1], "5");
        assert_eq!(args[at("--output") + 1], "/d/f.part");
        assert!(args.contains(&"--location".to_string()));
        // Verification stays on with the system CA store: no way to turn it off or swap it.
        for banned in [
            "-k",
            "--insecure",
            "--cacert",
            "--capath",
            "--proxy-insecure",
        ] {
            assert!(!args.contains(&banned.to_string()), "{banned}");
        }
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("-k") || a.contains("insecure"))
        );
        assert_eq!(&args[args.len() - 2..], ["--", "https://h/f"]);
    }
}
