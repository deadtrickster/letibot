//! The 20,000 most frequent words of wordfreq's `large_en`, which skill offers use to tell a
//! description's distinctive words from plain English.
//!
//! ```toml
//! [wordfreq]
//! mode = "internal"   # fetch the list once into <data dir>/wordfreq, and the default
//! mode = "off"        # no list: offers take names and keywords as evidence
//! download = false    # ... use the list on disk, never fetch it
//! ```
//!
//! The data is CC BY-SA 4.0, so nothing is compiled in: the source is fetched at a pinned commit
//! and sha-256, and the derived list is pinned too.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::builtin::{DownloadError, Source};

/// wordfreq's repository at the commit tagged `v3.2`, whose `large_en` is the one the 3.1.1
/// wheel on PyPI ships.
pub const COMMIT: &str = "42233e6c36ce792031bcccfa17cdd0cec9af5fa7";
pub const URL: &str = "https://raw.githubusercontent.com/rspeer/wordfreq/42233e6c36ce792031bcccfa17cdd0cec9af5fa7/wordfreq/data/large_en.msgpack.gz";
pub const SHA256: &str = "dffae8066b78dce0a6667cf5f58e567054f902674667090a7ac8a8a44628b05c";
const BYTES: u64 = 1_494_836;
/// The derived list's own hash: the derivation is deterministic, so a different list is a bug.
pub const LIST_SHA256: &str = "90ece52b900d4dad43025f22ab68164e3b1105cef3df39483135236c0f471fd1";

/// How many words the list keeps.
pub const TOP: usize = 20_000;
const SOURCE: &str = "large_en.msgpack.gz";
pub const LIST: &str = "english-20k.txt";
pub const NOTICE: &str = "NOTICE";

/// The `[wordfreq]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub on: bool,
    pub download: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            on: true,
            download: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    mode: Option<String>,
    download: Option<bool>,
}

impl Settings {
    /// A whole `leticode.toml`. `Ok(None)` is a file with no `[wordfreq]` table.
    pub fn from_toml(text: &str) -> Result<Option<Settings>, String> {
        let doc: toml::Table = toml::from_str(text).map_err(|e| format!("[wordfreq]: {e}"))?;
        let Some(value) = doc.get("wordfreq") else {
            return Ok(None);
        };
        let raw: Raw = value.clone().try_into().map_err(|e: toml::de::Error| {
            format!("[wordfreq]: {e}; it reads `mode` (internal, off) and `download`")
        })?;
        let on = match raw.mode.as_deref() {
            None | Some("internal") => true,
            Some("off") => false,
            Some(other) => {
                return Err(format!(
                    "[wordfreq] mode = \"{other}\" is not `internal` or `off`"
                ));
            }
        };
        Ok(Some(Settings {
            on,
            download: raw.download.unwrap_or(true),
        }))
    }

    /// The section in a file; a missing file has none.
    pub fn load(path: &Path) -> Result<Option<Settings>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// Where the source file comes from and what it and the derived list must hash to.
#[derive(Debug, Clone)]
pub struct Pin {
    url: String,
    sha256: String,
    bytes: Option<u64>,
    list_sha256: Option<String>,
}

impl Pin {
    pub fn shipped() -> Pin {
        Pin {
            url: URL.to_string(),
            sha256: SHA256.to_string(),
            bytes: Some(BYTES),
            list_sha256: Some(LIST_SHA256.to_string()),
        }
    }

    /// A test's own source: its URL and hash, and no pin on the list it derives.
    pub fn new(url: impl Into<String>, sha256: impl Into<String>) -> Pin {
        Pin {
            url: url.into(),
            sha256: sha256.into(),
            bytes: None,
            list_sha256: None,
        }
    }
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("wordfreq")
}

/// The common words, lowercase.
#[derive(Debug, Clone, Default)]
pub struct Words(HashSet<String>);

impl Words {
    pub fn of<I: IntoIterator<Item = S>, S: Into<String>>(words: I) -> Words {
        Words(words.into_iter().map(Into::into).collect())
    }

    pub fn load(path: &Path) -> std::io::Result<Words> {
        let text = std::fs::read_to_string(path)?;
        Ok(Words::of(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#')),
        ))
    }

    pub fn contains(&self, word: &str) -> bool {
        self.0.contains(word)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The list in the data dir, fetched and derived first when it is absent and `download` allows.
/// `say` gets one sentence, only when a fetch is about to happen.
pub fn open(
    settings: &Settings,
    data_dir: &Path,
    pin: &Pin,
    policy: letibot_http::retry::Policy,
    say: &mut dyn FnMut(&str),
) -> Result<Words, String> {
    let list = dir(data_dir).join(LIST);
    if !list.is_file() {
        if !settings.download {
            return Err(format!(
                "{} is absent and [wordfreq] download = false",
                list.display()
            ));
        }
        ensure(pin, data_dir, policy, say).map_err(|e| e.to_string())?;
    }
    Words::load(&list).map_err(|e| format!("{}: {e}", list.display()))
}

/// Fetch the pinned source, derive the list and write it with its NOTICE.
pub fn ensure(
    pin: &Pin,
    data_dir: &Path,
    policy: letibot_http::retry::Policy,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, DownloadError> {
    let dir = dir(data_dir);
    let list = dir.join(LIST);
    if list.is_file() {
        return Ok(list);
    }
    if pin.sha256.len() != 64 || !pin.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DownloadError::Unpinned);
    }
    say(&format!(
        "the English word list is not in {}, so wordfreq's large_en ({}) is being fetched from {}: \
         this happens once, and until it lands skill offers take names and keywords as evidence",
        dir.display(),
        pin.bytes
            .map(|b| format!("{} KiB", b.div_ceil(1024)))
            .unwrap_or_else(|| "size unknown".into()),
        pin.url
    ));
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let source = Source {
        file: SOURCE,
        sha256: pin.sha256.clone(),
        bytes: pin.bytes,
    };
    crate::builtin::fetch(&source, &pin.url, &dir, policy)?;
    let gz = dir.join(SOURCE);
    let words =
        derive(&std::fs::read(&gz).map_err(|e| io(&gz, e))?).map_err(|why| DownloadError::Io {
            path: gz.clone(),
            why,
        })?;
    let mut text = words.join("\n");
    text.push('\n');
    let got = format!("{:x}", Sha256::digest(text.as_bytes()));
    if let Some(want) = &pin.list_sha256
        && &got != want
    {
        return Err(DownloadError::Hash {
            file: LIST,
            url: pin.url.clone(),
            want: want.clone(),
            got,
        });
    }
    std::fs::write(dir.join(NOTICE), notice(pin, &got)).map_err(|e| io(&dir.join(NOTICE), e))?;
    let part = dir.join(format!("{LIST}.part"));
    std::fs::write(&part, text).map_err(|e| io(&part, e))?;
    std::fs::rename(&part, &list).map_err(|e| io(&list, e))?;
    let _ = std::fs::remove_file(&gz);
    Ok(list)
}

fn notice(pin: &Pin, list_sha256: &str) -> String {
    format!(
        "english-20k.txt: the {TOP} most frequent English words of three or more ASCII letters,\n\
         in wordfreq's frequency order, derived on this box from wordfreq's large_en.\n\
         \n\
         Source: {url}\n\
         Source sha256: {sha}\n\
         List sha256: {list_sha256}\n\
         wordfreq: https://github.com/rspeer/wordfreq (commit {COMMIT}, tag v3.2; the same file as the 3.1.1 wheel)\n\
         \n\
         The wordfreq data is (c) Robyn Speer and contributors, licensed CC BY-SA 4.0\n\
         (https://creativecommons.org/licenses/by-sa/4.0/). This list is a derived extract under the\n\
         same licence.\n",
        url = pin.url,
        sha = pin.sha256,
    )
}

fn io(path: &Path, e: std::io::Error) -> DownloadError {
    DownloadError::Io {
        path: path.to_path_buf(),
        why: e.to_string(),
    }
}

/// The top [`TOP`] words of a gzipped cBpack: `[header, bucket, bucket, ...]`, each bucket a
/// list of words, most frequent bucket first. Kept: lowercase ASCII letters, three or more.
pub fn derive(gz: &[u8]) -> Result<Vec<String>, String> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .map_err(|e| format!("gunzip: {e}"))?;
    let mut r = Reader { b: &raw, at: 0 };
    let buckets = r.array()?;
    if buckets == 0 {
        return Err("an empty cBpack".into());
    }
    r.skip()?;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(TOP);
    for _ in 1..buckets {
        for _ in 0..r.array()? {
            let word = r.str()?;
            if out.len() < TOP
                && word.len() >= 3
                && word.bytes().all(|b| b.is_ascii_lowercase())
                && seen.insert(word)
            {
                out.push(word.to_string());
            }
        }
        if out.len() == TOP {
            break;
        }
    }
    Ok(out)
}

/// The msgpack subset a cBpack uses: arrays, a header map, strings, small ints and bools.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.b.get(self.at).ok_or("msgpack: truncated")?;
        self.at += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("msgpack: truncated")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn uint(&mut self, n: usize) -> Result<usize, String> {
        Ok(self
            .take(n)?
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize))
    }

    fn array(&mut self) -> Result<usize, String> {
        match self.byte()? {
            t @ 0x90..=0x9f => Ok((t & 0x0f) as usize),
            0xdc => self.uint(2),
            0xdd => self.uint(4),
            t => Err(format!("msgpack: 0x{t:02x} where a list was expected")),
        }
    }

    fn str(&mut self) -> Result<&'a str, String> {
        let n = match self.byte()? {
            t @ 0xa0..=0xbf => (t & 0x1f) as usize,
            0xd9 => self.uint(1)?,
            0xda => self.uint(2)?,
            0xdb => self.uint(4)?,
            t => return Err(format!("msgpack: 0x{t:02x} where a word was expected")),
        };
        std::str::from_utf8(self.take(n)?).map_err(|e| format!("msgpack: {e}"))
    }

    fn skip(&mut self) -> Result<(), String> {
        let t = self.byte()?;
        let (items, bytes) = match t {
            0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => (0, 0),
            0x80..=0x8f => (2 * (t & 0x0f) as usize, 0),
            0x90..=0x9f => ((t & 0x0f) as usize, 0),
            0xa0..=0xbf => (0, (t & 0x1f) as usize),
            0xcc | 0xd0 => (0, 1),
            0xcd | 0xd1 => (0, 2),
            0xca | 0xce | 0xd2 => (0, 4),
            0xcb | 0xcf | 0xd3 => (0, 8),
            0xd9 => (0, self.uint(1)?),
            0xda => (0, self.uint(2)?),
            0xdb => (0, self.uint(4)?),
            0xdc => (self.uint(2)?, 0),
            0xdd => (self.uint(4)?, 0),
            0xde => (2 * self.uint(2)?, 0),
            0xdf => (2 * self.uint(4)?, 0),
            t => return Err(format!("msgpack: 0x{t:02x} is not read here")),
        };
        self.take(bytes)?;
        for _ in 0..items {
            self.skip()?;
        }
        Ok(())
    }
}

/// A cBpack over these buckets, gzipped, which is what a test serves.
#[cfg(any(test, feature = "testing"))]
pub fn fixture(buckets: &[&[&str]]) -> Vec<u8> {
    use std::io::Write;
    fn str(out: &mut Vec<u8>, s: &str) {
        assert!(s.len() < 32);
        out.push(0xa0 | s.len() as u8);
        out.extend_from_slice(s.as_bytes());
    }
    fn array(out: &mut Vec<u8>, n: usize) {
        out.push(0xdc);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    }
    let mut out = Vec::new();
    array(&mut out, buckets.len() + 1);
    out.push(0x82);
    for s in ["format", "cB", "version"] {
        str(&mut out, s);
    }
    out.push(0x01);
    for bucket in buckets {
        array(&mut out, bucket.len());
        for w in *bucket {
            str(&mut out, w);
        }
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&out).expect("gzip");
    gz.finish().expect("gzip")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{Dir, Fake, answer, owned_hash};
    use letibot_http::retry::Policy;

    fn served(body: Vec<u8>) -> Fake {
        Fake::new(move |path| match path.ends_with(SOURCE) {
            true => answer(&body),
            false => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        })
    }

    #[test]
    fn the_shipped_pin_is_whole() {
        let pin = Pin::shipped();
        assert!(pin.url.starts_with("https://") && pin.url.contains(COMMIT));
        assert_eq!(pin.sha256.len(), 64);
        assert_eq!(pin.list_sha256.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn the_list_keeps_frequency_order_and_drops_short_and_non_ascii_words() {
        let gz = fixture(&[
            &["the", "of", "and"],
            &["Café", "naïve", "work", "the"],
            &["x1y"],
        ]);
        assert_eq!(derive(&gz).unwrap(), vec!["the", "and", "work"]);
    }

    #[test]
    fn a_fetched_list_lands_with_its_notice_and_is_said_once() {
        let gz = fixture(&[&["the", "work", "caller"]]);
        let fake = served(gz.clone());
        let data = Dir::new("wordfreq-fetch");
        let pin = Pin::new(format!("{}/w/{SOURCE}", fake.base()), owned_hash(&gz));
        let mut said = Vec::new();
        let words = open(
            &Settings::default(),
            data.path(),
            &pin,
            Policy::none(),
            &mut |l| said.push(l.to_string()),
        )
        .expect("fetched");
        assert!(words.contains("caller") && !words.contains("sccache"));
        assert_eq!(said.len(), 1, "{said:?}");
        let notice = std::fs::read_to_string(dir(data.path()).join(NOTICE)).unwrap();
        assert!(notice.contains("CC BY-SA 4.0") && notice.contains(&owned_hash(&gz)));
        assert!(!dir(data.path()).join(SOURCE).exists());
        let again = open(
            &Settings::default(),
            data.path(),
            &pin,
            Policy::none(),
            &mut |l| said.push(l.to_string()),
        )
        .expect("on disk");
        assert_eq!(again.len(), 3);
        assert_eq!((said.len(), fake.requests()), (1, 1));
    }

    #[test]
    fn a_wrong_sha256_is_refused_and_nothing_lands() {
        let gz = fixture(&[&["the", "work"]]);
        let fake = served(gz);
        let data = Dir::new("wordfreq-hash");
        let pin = Pin::new(format!("{}/w/{SOURCE}", fake.base()), "ab".repeat(32));
        let err = open(
            &Settings::default(),
            data.path(),
            &pin,
            Policy::none(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.contains("sha-256"), "{err}");
        assert!(!dir(data.path()).join(LIST).exists());
        assert!(!dir(data.path()).join(SOURCE).exists());
    }

    #[test]
    fn a_derived_list_with_the_wrong_hash_is_refused() {
        let gz = fixture(&[&["the", "work"]]);
        let fake = served(gz.clone());
        let data = Dir::new("wordfreq-list-hash");
        let mut pin = Pin::new(format!("{}/w/{SOURCE}", fake.base()), owned_hash(&gz));
        pin.list_sha256 = Some("cd".repeat(32));
        let err = open(
            &Settings::default(),
            data.path(),
            &pin,
            Policy::none(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.contains(LIST), "{err}");
        assert!(!dir(data.path()).join(LIST).exists());
    }

    #[test]
    fn download_off_never_fetches() {
        let fake = served(Vec::new());
        let data = Dir::new("wordfreq-nodl");
        let pin = Pin::new(format!("{}/w/{SOURCE}", fake.base()), "ab".repeat(32));
        let off = Settings {
            on: true,
            download: false,
        };
        let err = open(&off, data.path(), &pin, Policy::none(), &mut |_| {}).unwrap_err();
        assert!(err.contains("download = false"), "{err}");
        assert_eq!(fake.requests(), 0);
    }

    #[test]
    fn the_section_reads_mode_and_download() {
        assert_eq!(Settings::from_toml("").unwrap(), None);
        assert_eq!(
            Settings::from_toml("[wordfreq]\n").unwrap(),
            Some(Settings::default())
        );
        let off = Settings::from_toml("[wordfreq]\nmode = \"off\"\n").unwrap();
        assert_eq!(off.map(|s| s.on), Some(false));
        let nodl = Settings::from_toml("[wordfreq]\ndownload = false\n").unwrap();
        assert_eq!(nodl.map(|s| s.download), Some(false));
        assert!(Settings::from_toml("[wordfreq]\nmode = \"on\"\n").is_err());
        assert!(Settings::from_toml("[wordfreq]\nmod = \"off\"\n").is_err());
    }

    /// Opt-in: `LETIBOT_WORDFREQ_SOURCE=<large_en.msgpack.gz>` checks the shipped pins against a
    /// real copy of the source.
    #[test]
    fn the_shipped_source_derives_the_pinned_list() {
        let Ok(path) = std::env::var("LETIBOT_WORDFREQ_SOURCE") else {
            return;
        };
        let gz = std::fs::read(path).expect("source");
        assert_eq!(owned_hash(&gz), SHA256);
        let mut text = derive(&gz).unwrap().join("\n");
        text.push('\n');
        assert_eq!(owned_hash(text.as_bytes()), LIST_SHA256);
    }
}

#[cfg(test)]
mod reference_tests {
    use letibot_config_ref::{files::LETICODE, probe};

    use super::*;

    #[test]
    fn the_wordfreq_table_is_listed() {
        let fields: std::collections::BTreeSet<&str> = probe::serde_fields::<Raw>()
            .unwrap()
            .iter()
            .copied()
            .collect();
        assert_eq!(fields, probe::keys_of(LETICODE.table("wordfreq").unwrap()));
    }
}
