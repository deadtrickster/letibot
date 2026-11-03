//! What the tests of this crate need to measure a fetch without the network: a scratch
//! directory, the tiny BERT's three files, and a loopback server that serves them.
//!
//! The client under test is the real one. Only the socket is local, so nothing here reaches
//! out of the box and no test needs a filled-in pin.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::builtin::{Pin, REVISION, Source};

/// A scratch directory that removes itself, so a fixture never outlives its test.
pub struct Dir(PathBuf);

impl Dir {
    pub fn new(tag: &str) -> Dir {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-builtin-{tag}-{}-{}-{}",
            std::process::id(),
            letibot_metrics::now_ms(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Dir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The three files a fetch has to find: the tiny BERT's, so the bytes served are the bytes of a
/// checkpoint this build opens and a test measures the download end to end.
pub struct Checkpoint {
    pub files: BTreeMap<&'static str, Vec<u8>>,
}

impl Checkpoint {
    pub fn new() -> Checkpoint {
        let dir = Dir::new("bytes");
        crate::testing::tiny_bert(dir.path()).expect("write the fixture");
        let files = crate::model::FILES
            .iter()
            .map(|file| {
                let bytes = std::fs::read(dir.path().join(file)).expect("read the fixture");
                (*file, bytes)
            })
            .collect();
        Checkpoint { files }
    }

    /// The pin over these bytes served from `base`: the shipped revision, a test's own hashes,
    /// so the directory the model lands in is the one the shipped pin names.
    pub fn pin(&self, base: &str) -> Pin {
        Pin::new(
            base,
            REVISION,
            crate::model::FILES
                .iter()
                .map(|file| {
                    let bytes = &self.files[file];
                    Source {
                        file,
                        sha256: owned_hash(bytes),
                        bytes: Some(bytes.len() as u64),
                    }
                })
                .collect(),
        )
    }

    /// A route that serves these bytes, and a 404 for anything else.
    pub fn route(&self) -> impl Fn(&str) -> Vec<u8> + Send + Sync + 'static {
        let files = self.files.clone();
        move |path| match files.get(path.rsplit('/').next().unwrap_or("")) {
            Some(bytes) => answer(bytes),
            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        }
    }
}

/// A hash a test computed, as the `String` a `Source` names: the shipped table's are constants
/// and a test's are the bytes it just wrote.
pub fn owned_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A loopback HTTP server that answers each request with the bytes a test names, and counts
/// them. `Drop` ends its accept loop.
pub struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Fake {
    pub fn new(route: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, flag, route) = (seen.clone(), stop.clone(), Arc::new(route));
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut path = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if path.is_empty() {
                        path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    }
                }
                log.lock().expect("log").push(path.clone());
                let mut out = stream;
                let _ = out.write_all(&route(&path));
                let _ = out.flush();
            }
        });
        Fake { port, seen, stop }
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// How many requests arrived, under any path.
    pub fn requests(&self) -> usize {
        self.seen.lock().expect("log").len()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One whole answer, with its length, so the client reads it as a finished body.
pub fn answer(body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}
