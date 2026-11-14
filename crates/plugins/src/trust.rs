//! Who wrote a layer: only builtins run freely, anything placed later needs a pin.
//! A pin is the SHA-256 of a layer's module and `plugin.toml`, so a rebuild or edit un-pins it.
//! Unpinned layers' calls are adjudicated without a card. Provenance is recorded and never consulted.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use letibot_metrics::now_ms;
use serde_json::{Value, json};

use crate::{MANIFEST, Manifest, WASM, sha256_file, sha256_hex};

/// One person's statement about one layer's bytes: *this module and this manifest are mine*.
/// The name and the directory are for a reader - `/plugin trust` lists them, a refusal quotes the
/// name. Neither decides: `Trust::standing` compares the two hashes, which un-pins a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The layer's manifest name at the moment it was pinned.
    pub name: String,
    /// The layer directory, as the stack spells it. Where a person pinned it, for the audit.
    pub dir: String,
    /// The SHA-256 of the layer's module file, named by its manifest (empty for a layer with no
    /// module, or one whose manifest will not parse).
    pub module: String,
    /// The SHA-256 of `plugin.toml`.
    pub manifest: String,
    /// The capabilities `plugin.toml` declared when the pin was taken: what the person approved. A
    /// rebuild that declares the same ones keeps the pin ([`Trust::rebuilt`]); `None` is a pin
    /// written before this row existed, and nothing recorded what was approved then.
    pub capabilities: Option<Vec<String>>,
    pub at_ms: u64,
}

/// What a rebuild did to a layer's pin. A pin is a statement about the manifest's capabilities as
/// much as about the bytes: a rebuild that declares nothing new is the same layer, and one that
/// declares a new power is a new review the person has not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebuilt {
    /// The pin was taken again over the new bytes, with the capabilities that were pinned.
    Kept(Pin),
    /// The pin is gone. `new` is what its manifest declares now that was not declared when the
    /// person pinned it, and `was` is what the pin recorded - `None` for a pin taken before this
    /// book recorded capabilities at all, which nothing can be judged against.
    Dropped {
        new: Vec<String>,
        was: Option<Vec<String>>,
    },
}

/// One write of a plugin artifact by a tool - provenance, recorded where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub sha256: String,
    pub at_ms: u64,
    /// The session whose tool wrote it. A person's own edit has no record at all, which is the
    /// difference this whole field is for.
    pub session: String,
}

/// What the trust book says about one layer now: may it run freely, and which pin is the
/// nearest thing to an answer about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// True when the layer's bytes are pinned (or it is a built-in, which never asks this).
    pub free: bool,
    /// The pin for this layer's directory - the one it matches, or the one it has broken. `None`
    /// for a layer nothing was ever pinned for.
    pub pin: Option<Pin>,
}

impl Standing {
    /// The sentence an unpinned or re-written layer is refused with - the layer by name, and both
    /// hashes when there is a pin to compare against. Written here rather than at each of the three
    /// refusal sites, because three spellings of *not pinned* would be three things to recognise.
    pub fn sentence(&self, layer: &str, module: &str, manifest: &str) -> String {
        match &self.pin {
            Some(pin) if pin.module != module || pin.manifest != manifest => format!(
                "the layer `{layer}` is not pinned any more: its bytes changed after it was \
                 pinned. What was pinned is module sha256 {} and manifest sha256 {}; what is on \
                 disk now is module sha256 {} and manifest sha256 {}. `/plugin trust {layer}` \
                 pins what is there now",
                sha_or_dash(&pin.module),
                sha_or_dash(&pin.manifest),
                sha_or_dash(module),
                sha_or_dash(manifest),
            ),
            _ => format!(
                "the layer `{layer}` is not pinned: nothing has recorded that these bytes are \
                 yours, so it is adjudicated call by call rather than run freely. Its module is \
                 sha256 {} and its manifest is sha256 {}. `/plugin trust {layer}` pins them",
                sha_or_dash(module),
                sha_or_dash(manifest),
            ),
        }
    }
}

/// A hash as a sentence quotes it: the whole one. A reader comparing two hashes needs all 64
/// characters, and a truncated hash in a refusal is a hash nobody can check.
fn sha_or_dash(sha: &str) -> String {
    if sha.is_empty() {
        "— (no module)".to_string()
    } else {
        sha.to_string()
    }
}

/// A revocation, as a statement with a time on it: trust is listed and revocable, so taking a pin
/// back is a row and not an absence. Without the row *not pinned* would be indistinguishable from
/// *never mentioned*, and a peer's stale copy would put back a pin a person had just taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Revoked {
    dir: String,
    at_ms: u64,
}

/// The book: the pins, the revocations, and the writes that were recorded.
/// One row per layer directory for each of the first two (the newest wins), one row per path for a
/// write. `merge` is the only way two books are combined, and it is by `at_ms`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Book {
    pins: Vec<Pin>,
    revoked: Vec<Revoked>,
    writes: Vec<Written>,
}

impl Book {
    fn parse(text: &str) -> Book {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            // A file that will not parse pins nothing. See the module doc: the fail-closed
            // direction is *unpinned*, so this is a cost and not a hole.
            return Book::default();
        };
        let mut book = Book::default();
        for pin in value
            .get("pins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = |k: &str| pin.get(k).and_then(Value::as_str).unwrap_or_default();
            if text("dir").is_empty() {
                continue;
            }
            book.pins.push(Pin {
                name: text("name").to_string(),
                dir: text("dir").to_string(),
                module: text("module").to_string(),
                manifest: text("manifest").to_string(),
                // A book written before capabilities were recorded answers `None`, and a rebuild of
                // that layer drops the pin: nothing says what the person approved.
                capabilities: pin
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .map(|caps| {
                        caps.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    }),
                at_ms: pin.get("at_ms").and_then(Value::as_u64).unwrap_or(0),
            });
        }
        for r in value
            .get("revoked")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let dir = r.get("dir").and_then(Value::as_str).unwrap_or_default();
            if dir.is_empty() {
                continue;
            }
            book.revoked.push(Revoked {
                dir: dir.to_string(),
                at_ms: r.get("at_ms").and_then(Value::as_u64).unwrap_or(0),
            });
        }
        for w in value
            .get("writes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = |k: &str| w.get(k).and_then(Value::as_str).unwrap_or_default();
            if text("path").is_empty() {
                continue;
            }
            book.writes.push(Written {
                path: text("path").to_string(),
                sha256: text("sha256").to_string(),
                at_ms: w.get("at_ms").and_then(Value::as_u64).unwrap_or(0),
                session: text("session").to_string(),
            });
        }
        book
    }

    /// Two books as one, by time: the newest statement about a layer directory wins, whichever
    /// process it came from. That is the whole of the cross-process safety - a stale copy's pin
    /// loses to the revocation or re-pin a person just took. A tie is broken towards `b`.
    fn merge(a: Book, b: Book) -> Book {
        let mut out = Book::default();
        for pin in a.pins.iter().chain(b.pins.iter()) {
            match out.pins.iter().position(|p| p.dir == pin.dir) {
                Some(at) if out.pins[at].at_ms > pin.at_ms => {}
                Some(at) => out.pins[at] = pin.clone(),
                None => out.pins.push(pin.clone()),
            }
        }
        for r in a.revoked.iter().chain(b.revoked.iter()) {
            match out.revoked.iter().position(|x| x.dir == r.dir) {
                Some(at) if out.revoked[at].at_ms > r.at_ms => {}
                Some(at) => out.revoked[at] = r.clone(),
                None => out.revoked.push(r.clone()),
            }
        }
        for written in a.writes.iter().chain(b.writes.iter()) {
            match out.writes.iter().position(|w| w.path == written.path) {
                Some(at) if out.writes[at].at_ms > written.at_ms => {}
                Some(at) => out.writes[at] = written.clone(),
                None => out.writes.push(written.clone()),
            }
        }
        // A pin a later revocation supersedes does not go in the file at all: the row would say
        // *pinned* about a layer nobody may trust, and a reader would have to compare timestamps -
        // a book of statements must not read as the opposite of what it means.
        out.pins.retain(|p| {
            let revoked = out
                .revoked
                .iter()
                .find(|r| r.dir == p.dir)
                .map(|r| r.at_ms)
                .unwrap_or(0);
            p.at_ms > revoked
        });
        out
    }

    /// The time this book's next statement carries - strictly later than every statement in it,
    /// so two acts in one millisecond are still ordered. A clock that goes backwards cannot make a
    /// new pin look older than the revocation it replaces.
    fn next_ms(&self) -> u64 {
        let last = self
            .pins
            .iter()
            .map(|p| p.at_ms)
            .chain(self.revoked.iter().map(|r| r.at_ms))
            .chain(self.writes.iter().map(|w| w.at_ms))
            .max()
            .unwrap_or(0);
        now_ms().max(last.saturating_add(1))
    }

    /// The revocation standing against a layer directory, if any.
    fn revoked_at(&self, dir: &str) -> u64 {
        self.revoked
            .iter()
            .find(|r| r.dir == dir)
            .map(|r| r.at_ms)
            .unwrap_or(0)
    }

    /// The pin in force for a layer directory: the row, when it is newer than any revocation.
    fn in_force(&self, dir: &str) -> Option<&Pin> {
        let pin = self.pins.iter().find(|p| p.dir == dir)?;
        (pin.at_ms > self.revoked_at(dir)).then_some(pin)
    }

    fn to_json(&self) -> String {
        let pins: Vec<Value> = self
            .pins
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "dir": p.dir,
                    "module": p.module,
                    "manifest": p.manifest,
                    "capabilities": p.capabilities,
                    "at_ms": p.at_ms,
                })
            })
            .collect();
        let revoked: Vec<Value> = self
            .revoked
            .iter()
            .map(|r| json!({ "dir": r.dir, "at_ms": r.at_ms }))
            .collect();
        let writes: Vec<Value> = self
            .writes
            .iter()
            .map(|w| {
                json!({
                    "path": w.path,
                    "sha256": w.sha256,
                    "at_ms": w.at_ms,
                    "session": w.session,
                })
            })
            .collect();
        serde_json::to_string_pretty(&json!({
            "pins": pins,
            "revoked": revoked,
            "writes": writes,
        }))
        .unwrap_or_else(|_| "{}".to_string())
    }
}

/// The trust book, as both processes read it.
/// Held behind an `Arc` by a [`crate::Stack`]: the stack is shared and a call takes `&self`, so the
/// book is a `Mutex` inside and every method takes `&self`.
pub struct Trust {
    /// `None` for a book that lives only in memory - a test, or a box with no config directory.
    /// Nothing is written anywhere and nothing survives the process.
    file: Option<PathBuf>,
    /// What the file said when it was last read, so a pin taken in another process is noticed
    /// without re-reading it on every hook call.
    stamp: Mutex<Option<(u64, u64)>>,
    book: Mutex<Book>,
}

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trust")
            .field("file", &self.file)
            .field("pins", &self.pins().len())
            .finish()
    }
}

impl Trust {
    /// A book that is never written - for a test, and for a host that could not resolve a
    /// config directory. Nothing is pinned, so every placed layer is adjudicated.
    pub fn in_memory() -> Arc<Trust> {
        Arc::new(Trust {
            file: None,
            stamp: Mutex::new(None),
            book: Mutex::new(Book::default()),
        })
    }

    /// The book at `path`, read now. A missing file is an empty book.
    pub fn open(path: impl Into<PathBuf>) -> Arc<Trust> {
        let file = path.into();
        let trust = Trust {
            file: Some(file.clone()),
            stamp: Mutex::new(None),
            book: Mutex::new(Book::default()),
        };
        trust.reload();
        Arc::new(trust)
    }

    /// This box's book - `$XDG_CONFIG_HOME/letibot/plugin-trust.json`, else
    /// `~/.config/letibot/plugin-trust.json`, and an in-memory one when neither resolves.
    pub fn open_default() -> Arc<Trust> {
        match Trust::default_path() {
            Some(path) => Trust::open(path),
            None => Trust::in_memory(),
        }
    }

    /// Where this box's book is, before it is opened - and the one door an override goes through.
    /// `LETIBOT_PLUGIN_TRUST` names a book explicitly, and an empty value means *no book at all*.
    /// It exists so a test that drives the real desk never writes the operator's own config file.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(named) = std::env::var_os("LETIBOT_PLUGIN_TRUST") {
            return (!named.is_empty()).then(|| PathBuf::from(named));
        }
        crate::paths::trust_book()
    }

    /// Where this book is, for a sentence that tells a person what to look at.
    pub fn path(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reload(&self) {
        let Some(file) = self.file.as_deref() else {
            return;
        };
        let stamp = stamp_of(file);
        let book = match std::fs::read_to_string(file) {
            Ok(text) => Book::parse(&text),
            Err(_) => Book::default(),
        };
        *self.lock() = book;
        *self.stamp.lock().unwrap_or_else(|e| e.into_inner()) = Some(stamp);
    }

    /// Re-read the file when it moved on disk - a pin taken by the head, or by another daemon,
    /// reaching this process. `true` when the book changed, which is the caller's cue to re-decide
    /// every layer's [`Standing`].
    pub fn refresh(&self) -> bool {
        let Some(file) = self.file.as_deref() else {
            return false;
        };
        let stamp = stamp_of(file);
        let seen = *self.stamp.lock().unwrap_or_else(|e| e.into_inner());
        if seen == Some(stamp) {
            return false;
        }
        let before = self.lock().clone();
        self.reload();
        *self.lock() != before
    }

    /// The file every writer locks, beside the book: `plugin-trust.lock`. A sidecar and not the
    /// book itself, because the book is written by `rename` - a lock held on the old inode says
    /// nothing about the new one. `None` degrades to a save that may lose a concurrent statement.
    fn lock_file(&self) -> Option<std::fs::File> {
        let file = self.file.as_deref()?;
        let path = file.with_extension("lock");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .ok()
    }

    /// Write the book, under a lock, merging with whatever is on disk at that instant. The merge by
    /// time means a peer's stale copy can undo neither a revocation nor a re-pin, and the exclusive
    /// lock makes the read, the merge and the write one act - without it the second rename wins.
    fn save(&self) {
        let Some(file) = self.file.as_deref() else {
            return;
        };
        let held = self.lock_file();
        if let Some(held) = held.as_ref() {
            let _ = held.lock();
        }
        let on_disk = std::fs::read_to_string(file)
            .map(|text| Book::parse(&text))
            .unwrap_or_default();
        let merged = Book::merge(on_disk, self.lock().clone());
        *self.lock() = merged;
        // The window a second writer could land in - a test stands in it, and only a test.
        #[cfg(test)]
        test_seam::run();
        let text = self.lock().to_json();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Written beside and renamed on: a reader in the other process must never see half a
        // book, and a half book is an unpinned layer.
        let tmp = file.with_extension(format!("tmp-{}", std::process::id()));
        if std::fs::write(&tmp, text.as_bytes()).is_ok() && std::fs::rename(&tmp, file).is_ok() {
            *self.stamp.lock().unwrap_or_else(|e| e.into_inner()) = Some(stamp_of(file));
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        if let Some(held) = held {
            let _ = held.unlock();
        }
    }

    /// What the book says about these bytes. The comparison is the two hashes and nothing
    /// else - a pin is a statement about bytes, so a rebuild is a different layer.
    pub fn standing(&self, dir: &Path, module: &str, manifest: &str) -> Standing {
        let key = key_of(dir);
        let book = self.lock();
        // A revoked pin is not a pin at all. The sentence a revoked layer is refused with is
        // *not pinned*, not *not pinned any more* - a person took it back, so there is nothing to
        // compare the bytes with and nothing for them to have changed away from.
        let Some(pin) = book.in_force(&key) else {
            return Standing {
                free: false,
                pin: None,
            };
        };
        if pin.module == module && pin.manifest == manifest {
            Standing {
                free: true,
                pin: Some(pin.clone()),
            }
        } else {
            Standing {
                free: false,
                pin: Some(pin.clone()),
            }
        }
    }

    /// What a rebuild of this layer did to its pin: a rebuild whose `plugin.toml` asks for the same
    /// powers keeps the operator's pin over the new bytes, and one that asks for a new power drops
    /// it. `None` when this layer was never pinned, which is the ordinary case.
    pub fn rebuilt(&self, name: &str, dir: &Path) -> Result<Option<Rebuilt>, String> {
        let key = key_of(dir);
        let before = {
            let book = self.lock();
            book.in_force(&key).cloned()
        };
        let Some(pin) = before else {
            return Ok(None);
        };
        let now = capabilities_of(dir).unwrap_or_default();
        let Some(approved) = pin.capabilities.clone() else {
            return Ok(Some(Rebuilt::Dropped {
                new: now,
                was: None,
            }));
        };
        let added: Vec<String> = now
            .iter()
            .filter(|c| !approved.contains(c))
            .cloned()
            .collect();
        let kept = added.is_empty() && approved.iter().all(|c| now.contains(c));
        if kept {
            return self.pin(name, dir).map(|pin| Some(Rebuilt::Kept(pin)));
        }
        Ok(Some(Rebuilt::Dropped {
            new: added,
            was: Some(approved),
        }))
    }

    /// Pin a layer directory's bytes as they are now, and answer what was pinned.
    /// The hashes are read from the files rather than taken from a caller: a caller that passed a
    /// hash it remembered would be pinning the layer it meant to write.
    pub fn pin(&self, name: &str, dir: &Path) -> Result<Pin, String> {
        let manifest = sha256_file(&dir.join(MANIFEST))
            .map_err(|why| format!("`{}` has no manifest to pin: {why}", dir.display()))?;
        let module = sha256_file(&dir.join(Manifest::module_name(dir))).unwrap_or_default();
        // The time is taken from the book, not from the clock alone: a pin has to be newer
        // than the revocation it replaces even when the two happen inside one millisecond.
        let at_ms = self.lock().next_ms();
        let pin = Pin {
            name: name.to_string(),
            dir: key_of(dir),
            module,
            manifest,
            // The powers on the disk now, read back rather than taken from the caller: they are
            // half of what this statement is about, and a rebuild is judged against them.
            capabilities: capabilities_of(dir),
            at_ms,
        };
        {
            let mut book = self.lock();
            book.pins.retain(|p| p.dir != pin.dir);
            // A re-pin supersedes the revocation it answers: keeping the tombstone would be a row
            // every later merge has to compare against, for a statement already answered.
            book.revoked.retain(|r| r.dir != pin.dir);
            book.pins.push(pin.clone());
        }
        self.save();
        Ok(pin)
    }

    /// Take a pin back - by the layer's directory, or by the name it was pinned under. The
    /// pin that was removed is answered so the caller can say which one it was.
    pub fn unpin(&self, what: &str) -> Option<Pin> {
        let key = key_of(Path::new(what));
        let taken = {
            let mut book = self.lock();
            let at = book
                .pins
                .iter()
                .position(|p| p.dir == key || p.name == what)?;
            let taken = book.pins.remove(at);
            // A tombstone, not an absence. This row is what stops another process's stale copy
            // from putting the pin back: it carries a time, and a merge keeps the newer statement.
            let at_ms = book.next_ms();
            book.revoked.retain(|r| r.dir != taken.dir);
            book.revoked.push(Revoked {
                dir: taken.dir.clone(),
                at_ms,
            });
            taken
        };
        self.save();
        Some(taken)
    }

    /// Every pin in force, newest last - a revoked one is not listed, because the question this
    /// answers is *what is pinned*.
    pub fn pins(&self) -> Vec<Pin> {
        let book = self.lock();
        book.pins
            .iter()
            .filter(|p| book.in_force(&p.dir).is_some())
            .cloned()
            .collect()
    }

    /// Record that a tool wrote these bytes, when they are a plugin artifact.
    /// `None` when the path is not one - a write of `README.md` is not provenance about a layer,
    /// and a file of every write on the box would bury the ones that matter.
    pub fn note_write(&self, path: &Path, bytes: &[u8], session: &str) -> Option<Written> {
        if !is_plugin_artifact(path) {
            return None;
        }
        // A write is a statement too, so it carries a time from the book as well: a peer's
        // older record of the same path must not overwrite a newer one at the next merge.
        let at_ms = self.lock().next_ms();
        let written = Written {
            path: key_of(path),
            sha256: sha256_hex(bytes),
            at_ms,
            session: session.to_string(),
        };
        {
            let mut book = self.lock();
            book.writes.retain(|w| w.path != written.path);
            book.writes.push(written.clone());
        }
        self.save();
        Some(written)
    }

    /// Every write record, newest last.
    pub fn writes(&self) -> Vec<Written> {
        self.lock().writes.clone()
    }

    /// Who wrote the bytes in this layer's directory - the records for anything under it: its
    /// module, its manifest, and the source they were built from. What `/plugin trust` reports
    /// beside the hash it pinned.
    pub fn writes_for(&self, dir: &Path) -> Vec<Written> {
        let under = key_of(dir);
        let book = self.lock();
        book.writes
            .iter()
            .filter(|w| Path::new(&w.path).starts_with(&under))
            .cloned()
            .collect()
    }
}

/// The one window this file has, made reachable for a test - `save` runs the installed hook
/// between its read and its write, which is exactly where a second process's write would land.
/// A seam with no production caller, `#[cfg(test)]` all the way through.
#[cfg(test)]
pub(crate) mod test_seam {
    use std::sync::Mutex;
    use std::thread::ThreadId;

    pub type Hook = Box<dyn FnOnce() + Send>;

    /// The installing thread, not just the hook. Libtest runs a file's tests in parallel, so a
    /// process-wide hook would fire inside somebody else's save and fail a second test. A seam that
    /// only its own thread can trip is a seam that cannot.
    static HOOK: Mutex<Option<(ThreadId, Hook)>> = Mutex::new(None);

    /// Stand in the window on this thread's next save, once.
    pub fn install(hook: Hook) {
        *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::thread::current().id(), hook));
    }

    pub fn run() {
        let mine = std::thread::current().id();
        let hook = {
            let mut slot = HOOK.lock().unwrap_or_else(|e| e.into_inner());
            match slot.as_ref() {
                Some((id, _)) if *id == mine => slot.take().map(|(_, hook)| hook),
                _ => None,
            }
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// The capabilities a layer directory's `plugin.toml` declares now, as the manifest spells them.
/// `None` when there is no manifest to read, which is a layer to be re-pinned rather than judged.
fn capabilities_of(dir: &Path) -> Option<Vec<String>> {
    Manifest::read(dir)
        .ok()
        .map(|m| m.capabilities.iter().map(|c| c.to_string()).collect())
}

/// A path as the book spells it: absolute where it can be, so a pin taken in the head and a layer
/// discovered by the daemon are the same key.
fn key_of(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(path))
                    .unwrap_or_else(|_| path.to_path_buf())
            }
        })
        .display()
        .to_string()
}

/// `(mtime, len)` - cheap enough to take on every poll, and enough to notice a rewrite of the same
/// length.
fn stamp_of(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (secs, m.len())
        }
        Err(_) => (0, 0),
    }
}

/// Is this write about a layer? - a module, a manifest, or a file beside one.
/// A layer's source is a plugin artifact too: a `.rs`/`.js` in a directory with a `plugin.toml` is
/// what the module is built from, and a record of it is what a reviewer reads.
fn is_plugin_artifact(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name == MANIFEST || name == WASM {
        return true;
    }
    // A layer's source is an artifact too: `src/lib.rs` sits one level down from the manifest, a
    // test one level further. Three ancestors is the whole of what a layer directory holds.
    let mut at = path.parent();
    for _ in 0..3 {
        let Some(dir) = at else { break };
        if dir.join(MANIFEST).is_file() {
            return true;
        }
        at = dir.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "letibot-trust-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a temp dir");
        p
    }

    fn layer(root: &Path, name: &str, module: &[u8]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("a layer dir");
        std::fs::write(
            dir.join(MANIFEST),
            format!("name = \"{name}\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = []\ncontract = 1\n"),
        )
        .expect("a manifest");
        std::fs::write(dir.join(WASM), module).expect("a module");
        dir
    }

    #[test]
    fn nothing_pinned_is_nothing_free_and_a_pin_frees_the_bytes_it_was_taken_on() {
        let root = tempdir("pin");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();

        let before = trust.standing(
            &dir,
            &sha256_hex(b"module v1"),
            &sha256_hex(&std::fs::read(dir.join(MANIFEST)).unwrap()),
        );
        assert!(!before.free, "an unpinned layer runs nothing freely");
        assert!(before.pin.is_none());
        let said = before.sentence("one", &sha256_hex(b"module v1"), "");
        assert!(said.contains("not pinned"), "{said}");
        assert!(said.contains("/plugin trust one"), "{said}");

        let pin = trust.pin("one", &dir).expect("a pin");
        assert_eq!(pin.module, sha256_hex(b"module v1"));
        let after = trust.standing(&dir, &pin.module, &pin.manifest);
        assert!(after.free, "a pinned layer runs freely");
        assert_eq!(after.pin.as_ref().map(|p| p.name.as_str()), Some("one"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_byte_change_after_the_pin_un_pins_it_and_the_sentence_names_both_hashes() {
        let root = tempdir("changed");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();
        let pin = trust.pin("one", &dir).expect("a pin");

        // The module is rebuilt, which is the shape this rule exists for.
        std::fs::write(dir.join(WASM), b"module v2").expect("a rebuilt module");
        let now = sha256_hex(b"module v2");
        let standing = trust.standing(&dir, &now, &pin.manifest);
        assert!(!standing.free, "the pin was about the old bytes");
        let said = standing.sentence("one", &now, &pin.manifest);
        assert!(said.contains(&pin.module), "the pinned hash: {said}");
        assert!(said.contains(&now), "and what is there now: {said}");
        assert!(said.contains("not pinned any more"), "{said}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rebuild_keeps_the_pin_while_the_manifest_asks_for_the_same_capabilities() {
        let root = tempdir("rebuilt");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();
        let pin = trust.pin("one", &dir).expect("a pin");
        assert_eq!(pin.capabilities, Some(Vec::new()));

        std::fs::write(dir.join(WASM), b"module v2").expect("a rebuilt module");
        let kept = trust.rebuilt("one", &dir).expect("a decision");
        let Some(Rebuilt::Kept(pin)) = kept else {
            panic!("a rebuild with no new capability keeps the pin: {kept:?}");
        };
        assert_eq!(pin.module, sha256_hex(b"module v2"), "over the new bytes");
        assert!(
            trust
                .standing(
                    &dir,
                    &sha256_hex(b"module v2"),
                    &sha256_hex(&std::fs::read(dir.join(MANIFEST)).unwrap())
                )
                .free,
            "so the layer runs freely again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rebuild_that_declares_a_new_capability_drops_the_pin_and_names_it() {
        let root = tempdir("widened");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();
        trust.pin("one", &dir).expect("a pin");

        // The one thing a person is actually approving: what the layer may reach.
        std::fs::write(
            dir.join(MANIFEST),
            "name = \"one\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = [\"read\"]\n\
             contract = 1\n",
        )
        .expect("a wider manifest");
        std::fs::write(dir.join(WASM), b"module v2").expect("a rebuilt module");
        let dropped = trust.rebuilt("one", &dir).expect("a decision");
        let Some(Rebuilt::Dropped { new, was }) = dropped else {
            panic!("a new capability drops the pin: {dropped:?}");
        };
        assert_eq!(new, vec!["read".to_string()], "the power that is new");
        assert_eq!(was, Some(Vec::new()), "and what was approved");
        assert!(
            !trust
                .standing(
                    &dir,
                    &sha256_hex(b"module v2"),
                    &sha256_hex(&std::fs::read(dir.join(MANIFEST)).unwrap())
                )
                .free,
            "the layer is adjudicated call by call until it is pinned again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_layer_nobody_pinned_has_nothing_to_decide() {
        let root = tempdir("unpinned");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();
        assert_eq!(trust.rebuilt("one", &dir), Ok(None));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_book_is_a_file_so_a_pin_taken_elsewhere_is_noticed_by_its_next_poll() {
        let root = tempdir("file");
        let dir = layer(&root, "one", b"module v1");
        let file = root.join("plugin-trust.json");
        let daemon = Trust::open(&file);
        assert!(!daemon.refresh(), "nothing has changed yet");
        assert!(
            !daemon
                .standing(
                    &dir,
                    &sha256_hex(b"module v1"),
                    &sha256_hex(&std::fs::read(dir.join(MANIFEST)).unwrap())
                )
                .free
        );

        // The head, in another process, pins it.
        let head = Trust::open(&file);
        let pin = head.pin("one", &dir).expect("a pin");

        assert!(daemon.refresh(), "the daemon's next poll sees it");
        assert!(daemon.standing(&dir, &pin.module, &pin.manifest).free);
        assert_eq!(daemon.pins().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pin_hashes_the_module_the_manifest_names_and_editing_it_un_pins_the_layer() {
        let root = tempdir("named-module");
        let dir = root.join("one");
        std::fs::create_dir_all(&dir).expect("a layer dir");
        std::fs::write(
            dir.join(MANIFEST),
            "name = \"one\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = []\ncontract = 1\nmodule = \"one.js\"\n",
        )
        .expect("a manifest");
        std::fs::write(dir.join("one.js"), b"a script v1").expect("the named module");
        // A wasm file beside it is not this layer's module, so it is not what the pin is about.
        std::fs::write(dir.join(WASM), b"a stale build")
            .expect("a wasm the manifest does not name");

        let trust = Trust::in_memory();
        let pin = trust.pin("one", &dir).expect("a pin");
        assert_eq!(
            pin.module,
            sha256_hex(b"a script v1"),
            "the pin is about the file the manifest names"
        );
        let manifest = sha256_hex(&std::fs::read(dir.join(MANIFEST)).unwrap());
        assert!(trust.standing(&dir, &pin.module, &manifest).free);

        // Editing the script is a different layer, which is the whole point of pinning it.
        std::fs::write(dir.join("one.js"), b"a script v2").expect("an edited module");
        let now = sha256_hex(b"a script v2");
        let standing = trust.standing(&dir, &now, &manifest);
        assert!(!standing.free, "the pin was about the old text");
        let said = standing.sentence("one", &now, &manifest);
        assert!(said.contains(&pin.module), "the pinned hash: {said}");
        assert!(said.contains("not pinned any more"), "{said}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_of_a_plugin_artifact_is_recorded_and_a_write_of_anything_else_is_not() {
        let root = tempdir("writes");
        let dir = layer(&root, "one", b"module v1");
        let trust = Trust::in_memory();
        let manifest = dir.join(MANIFEST);
        let bytes = std::fs::read(&manifest).unwrap();
        let record = trust
            .note_write(&manifest, &bytes, "s-1")
            .expect("a manifest a tool wrote is provenance");
        assert_eq!(record.sha256, sha256_hex(&bytes));
        assert_eq!(record.session, "s-1");
        // The source beside a manifest is an artifact too.
        assert!(
            trust
                .note_write(&dir.join("src/lib.rs"), b"fn view() {}", "s-1")
                .is_some()
        );
        assert_eq!(trust.writes_for(&dir).len(), 2);
        // And a README is not.
        assert!(
            trust
                .note_write(&root.join("README.md"), b"# x", "s-1")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pin_taken_in_one_process_and_a_write_recorded_in_another_both_survive() {
        let root = tempdir("merge");
        let dir = layer(&root, "one", b"module v1");
        let file = root.join("plugin-trust.json");
        // Two books over one file: the head takes pins, the daemon records writes, and each holds
        // its own copy until its next poll.
        let head = Trust::open(&file);
        let daemon = Trust::open(&file);
        let pin = head.pin("one", &dir).expect("a pin");
        daemon.note_write(&dir.join(MANIFEST), b"a manifest", "s-1");
        // Neither rewrite lost the other's news.
        let seen = Trust::open(&file);
        assert_eq!(seen.pins(), vec![pin.clone()], "the pin survived");
        assert_eq!(seen.writes().len(), 1, "and so did the write record");

        // And a revocation is a statement, not an absence: a merge must not resurrect the pin
        // from a copy on disk that still has it.
        daemon.unpin("one").expect("the pin, revoked");
        let seen = Trust::open(&file);
        assert!(seen.pins().is_empty(), "a revoked pin stays revoked");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The direction that matters: a stale peer must not undo the operator's own act.
    /// The operator pins and then revokes in the head, and a daemon whose book still holds the pin
    /// saves a write record afterwards. It is the timestamps that stop it putting the pin back.
    #[test]
    fn a_stale_book_cannot_put_back_a_pin_a_person_took_away() {
        let root = tempdir("stale-revoke");
        let dir = layer(&root, "one", b"module v1");
        let file = root.join("plugin-trust.json");
        let head = Trust::open(&file);
        let pin = head.pin("one", &dir).expect("a pin");
        assert!(
            Trust::open(&file)
                .standing(&dir, &pin.module, &pin.manifest)
                .free
        );
        // The daemon's copy is taken after the pin and never refreshed again - it holds P1 and
        // knows nothing of the revocation, which is the worst case and the reviewer's exact one.
        let daemon = Trust::open(&file);

        head.unpin("one").expect("the operator took it back");
        assert!(
            !Trust::open(&file)
                .standing(&dir, &pin.module, &pin.manifest)
                .free,
            "the revocation is in force"
        );

        // And now the stale daemon saves, for a reason of its own: a model wrote a source file
        // inside the layer's directory.
        daemon.note_write(&dir.join("src/lib.rs"), b"fn view() {}", "s-1");

        let seen = Trust::open(&file);
        assert!(
            seen.pins().is_empty(),
            "a model's write must not restore a pin the person took back: {:?}",
            seen.pins()
        );
        assert!(
            !seen.standing(&dir, &pin.module, &pin.manifest).free,
            "and the layer is still not free"
        );
        // And the file says so too: the stale writer must not even put the row back, or a
        // person reading their own book would see a pin that is not in force.
        let on_disk = std::fs::read_to_string(&file).expect("the book");
        assert!(
            !on_disk.contains(&pin.module),
            "the revoked pin's row is gone from the file: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The interleaving the merge alone cannot survive: a second writer lands inside `save`'s own
    /// window, between this process's read of the book and its rename over it. The first is paused
    /// by [`test_seam`] there, and the peer then pins a different layer, which must survive.
    #[test]
    fn a_write_that_lands_between_a_read_and_its_rename_is_not_lost() {
        use std::sync::mpsc;
        use std::time::Duration;

        let root = tempdir("interleave");
        let a_dir = layer(&root, "a", b"module a");
        let b_dir = layer(&root, "b", b"module b");
        let file = root.join("plugin-trust.json");
        let one = Trust::open(&file);

        let (hand_back, handed) = mpsc::channel::<std::thread::JoinHandle<Option<Pin>>>();
        let (started, begun) = mpsc::channel::<()>();
        let peer_file = file.clone();
        let peer_dir = b_dir.clone();
        test_seam::install(Box::new(move || {
            let peer = std::thread::spawn(move || {
                let other = Trust::open(&peer_file);
                started.send(()).ok();
                other.pin("b", &peer_dir).ok()
            });
            // Wait until it is inside `pin`, then give it long enough to have reached the lock.
            let _ = begun.recv_timeout(Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(250));
            assert!(
                !peer.is_finished(),
                "the second writer must WAIT for the first to finish writing: without the lock it \
                 reads a book that is about to be replaced and its statement is discarded"
            );
            let _ = hand_back.send(peer);
        }));

        let mine = one.pin("a", &a_dir).expect("the first pin");
        let peer = handed
            .recv_timeout(Duration::from_secs(10))
            .expect("the seam hands its thread back");
        let theirs = peer
            .join()
            .expect("the second writer finished")
            .expect("the second pin landed");

        let seen = Trust::open(&file);
        let pinned: Vec<String> = seen.pins().iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            pinned.len(),
            2,
            "neither writer's statement may be lost: {pinned:?} (first {mine:?}, second {theirs:?})"
        );
        assert!(pinned.contains(&"a".to_string()) && pinned.contains(&"b".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The other direction: a stale peer must not undo a re-pin either. After a byte change the
    /// operator pins what is on disk now; a peer still holding the older pin saves and must not
    /// overwrite it, or `/plugin trust` would report a pin that is not in force.
    #[test]
    fn a_stale_book_cannot_put_back_the_pin_that_a_byte_change_replaced() {
        let root = tempdir("stale-repin");
        let dir = layer(&root, "one", b"module v1");
        let file = root.join("plugin-trust.json");
        let head = Trust::open(&file);
        let first = head.pin("one", &dir).expect("the first pin");
        // The daemon's copy holds P1 from here on.
        let daemon = Trust::open(&file);

        std::fs::write(dir.join(WASM), b"module v2").expect("a rebuilt module");
        let second = head.pin("one", &dir).expect("the re-pin");
        assert_ne!(
            second.module, first.module,
            "different bytes, different pin"
        );

        // The stale peer saves a write record - its own news, about the old pin's world.
        daemon.note_write(&dir.join(WASM), b"module v2", "s-1");

        let seen = Trust::open(&file);
        assert_eq!(seen.pins().len(), 1);
        assert_eq!(
            seen.pins()[0].module,
            second.module,
            "the re-pin is what stands, not the pin the peer remembered"
        );
        assert!(
            seen.standing(&dir, &second.module, &second.manifest).free,
            "the layer is free on the bytes a person actually pinned"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_book_that_will_not_parse_pins_nothing_rather_than_everything() {
        let root = tempdir("corrupt");
        let file = root.join("plugin-trust.json");
        std::fs::write(&file, "{ this is not json").expect("a corrupt book");
        let trust = Trust::open(&file);
        assert!(trust.pins().is_empty());
        let dir = layer(&root, "one", b"m");
        assert!(!trust.standing(&dir, &sha256_hex(b"m"), "").free);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pin_can_be_taken_back_by_name_or_by_directory() {
        let root = tempdir("unpin");
        let dir = layer(&root, "one", b"m");
        let trust = Trust::in_memory();
        let pin = trust.pin("one", &dir).expect("a pin");
        assert!(trust.standing(&dir, &pin.module, &pin.manifest).free);
        let taken = trust.unpin("one").expect("the pin by name");
        assert_eq!(taken.dir, pin.dir);
        assert!(trust.pins().is_empty());
        assert!(!trust.standing(&dir, &pin.module, &pin.manifest).free);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `XDG_CONFIG_HOME=` is a person saying it is unset: the book is then the one in `~/.config`,
    /// which is the file the daemon opens. Deriving it from the empty string gave a relative path.
    #[test]
    fn an_empty_xdg_config_home_is_the_boxes_book() {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir("empty-xdg");
        let before: Vec<(&str, Option<std::ffi::OsString>)> =
            ["XDG_CONFIG_HOME", "HOME", "LETIBOT_PLUGIN_TRUST"]
                .into_iter()
                .map(|k| (k, std::env::var_os(k)))
                .collect();
        // SAFETY: this module's only test that touches the environment, under the lock above.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "");
            std::env::set_var("HOME", &home);
            std::env::remove_var("LETIBOT_PLUGIN_TRUST");
        }
        let seen = Trust::default_path();
        for (key, value) in before {
            // SAFETY: the same lock, restored before anything else reads the environment.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        assert_eq!(
            seen,
            Some(
                home.join(".config")
                    .join("letibot")
                    .join("plugin-trust.json")
            ),
            "the book sits beside the user scope, not under a relative `letibot/`"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod reference_tests {
    use letibot_config_ref::files::PLUGIN_TRUST;

    use super::*;

    /// A book holding every listed field reads and writes back exactly those fields.
    #[test]
    fn the_book_is_the_listed_fields() {
        let mut doc = serde_json::Map::new();
        for t in PLUGIN_TRUST.tables {
            let mut entry = serde_json::Map::new();
            for k in t.keys {
                entry.insert(k.key.into(), serde_json::from_str(k.example).unwrap());
            }
            doc.insert(
                t.path.trim_end_matches("[]").into(),
                serde_json::Value::Array(vec![entry.into()]),
            );
        }
        let text = serde_json::Value::Object(doc.clone()).to_string();
        let back: serde_json::Value = serde_json::from_str(&Book::parse(&text).to_json()).unwrap();
        assert_eq!(back, serde_json::Value::Object(doc));
    }
}
