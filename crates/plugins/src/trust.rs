//! **Who wrote a layer, on disk** — DECISIONS.md D17, as the operator restated it on 2026-10-31:
//! *"Only the builtins run freely; anything I place later needs a pin."*
//!
//! # The ladder, in three arms
//!
//! 1. **A built-in runs freely**, `exec` included. It is the binary's own bytes — nobody placed
//!    them at run time — and [`crate::Stack::register_builtin_shared`] marks it free by
//!    definition, not by anything in this file.
//! 2. **A placed layer needs a pin.** A pin is the SHA-256 of the layer's module and of its
//!    `plugin.toml`, recorded by a person saying *these bytes are mine*. A layer whose bytes match
//!    a pin is free. One that matches nothing is **unpinned**, and every call it makes is
//!    adjudicated without a card (`Gate::admit_unasked`) rather than run.
//! 3. **A byte change after the pin un-pins it.** The pin is not a permission granted to a
//!    directory; it is a statement about bytes. So a layer rebuilt or edited after it was pinned
//!    is unpinned again, and [`Standing::sentence`] names the layer and BOTH hashes — what the
//!    person pinned and what is on disk now.
//!
//! # Provenance is recorded, not inferred
//!
//! [`Trust::note_write`] records the SHA-256 of a plugin module or manifest at the moment a tool
//! writes it, so *who wrote these bytes* is a fact on disk rather than a guess from a path — a
//! layer in a project's `.letibot/plugins/` is not the operator's because of where it sits.
//!
//! **The ladder does not consult provenance**, and that is deliberate: an unpinned layer is
//! adjudicated whatever wrote it, so the weaker fact (a guess about authorship) can never widen
//! what runs. The record is what a person reads when deciding whether to pin — `/plugin trust`
//! reports it — and what a later audit answers *who put this here* with.
//!
//! # The file, and why it is one
//!
//! Pins and write records live in one JSON file in the letibot config directory
//! (`$XDG_CONFIG_HOME/letibot/plugin-trust.json`), not in a layer's own directory: a pin written
//! beside the bytes it pins would be a pin a model can write, which is the one thing the ladder
//! cannot survive. It is a file rather than a store table because two processes read it — the
//! daemon's stack, which answers tools, and the head's, which draws panes — and a pin a person
//! takes in one must be in force in the other within its next poll.
//!
//! A missing file is *nothing is pinned*, and so is one that will not parse: the ladder's
//! fail-closed direction is the unpinned one, so a corrupt file costs a card-free adjudication and
//! never a free run.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::{MANIFEST, WASM, sha256_file, sha256_hex};

/// **One person's statement about one layer's bytes**: *this module and this manifest are mine*.
///
/// The name and the directory are for a reader — `/plugin trust` lists them, and the sentence a
/// refusal carries quotes the name. Neither is what the pin decides: [`Trust::standing`] compares
/// the two hashes and nothing else, which is what makes a rebuild un-pin the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The layer's manifest name at the moment it was pinned.
    pub name: String,
    /// The layer directory, as the stack spells it. Where a person pinned it, for the audit.
    pub dir: String,
    /// The SHA-256 of `plugin.wasm`, or empty for a layer with no module (a declaration).
    pub module: String,
    /// The SHA-256 of `plugin.toml`.
    pub manifest: String,
    pub at_ms: u64,
}

/// **One write of a plugin artifact by a tool** — provenance, recorded where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub sha256: String,
    pub at_ms: u64,
    /// The session whose tool wrote it. A person's own edit has no record at all, which is the
    /// difference this whole field is for.
    pub session: String,
}

/// **What the trust book says about one layer now**: may it run freely, and which pin is the
/// nearest thing to an answer about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// True when the layer's bytes are pinned (or it is a built-in, which never asks this).
    pub free: bool,
    /// The pin for this layer's directory — the one it matches, or the one it has broken. `None`
    /// for a layer nothing was ever pinned for.
    pub pin: Option<Pin>,
}

impl Standing {
    /// **The sentence an unpinned or re-written layer is refused with** — the layer by name, and
    /// both hashes when there is a pin to compare against.
    ///
    /// Written here rather than at each refusal site because three callers say it (a tool call, an
    /// `exec` look, the stack's own audit notice) and three spellings of *this layer is not
    /// pinned* would be three things a person has to recognise.
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

/// A hash as a sentence quotes it: **the whole one**. A reader comparing two hashes needs all 64
/// characters, and a truncated hash in a refusal is a hash nobody can check.
fn sha_or_dash(sha: &str) -> String {
    if sha.is_empty() {
        "— (no module)".to_string()
    } else {
        sha.to_string()
    }
}

/// **A revocation, as a statement with a time on it** — DECISIONS.md D17's *"trust is listed and
/// revocable"*, and the reason it is a row rather than an absence.
///
/// The book has two writers in two processes (the daemon records a write, the head takes or takes
/// back a pin) and neither holds the other's copy. So **every statement carries its time**, and a
/// merge is decided by that time and nothing else: a pin only stands if it is newer than the
/// revocation of the same layer. Without the row, *not pinned* would be indistinguishable from
/// *never mentioned*, and a peer's stale copy would put back a pin a person had just taken back —
/// with a model's ordinary `write` into the layer directory as the thing that triggered the save.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Revoked {
    dir: String,
    at_ms: u64,
}

/// The book: the pins, the revocations, and the writes that were recorded.
///
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

    /// **Two books as one**, by time: the newest statement about a layer directory wins, whichever
    /// process it came from.
    ///
    /// This is the whole of the cross-process safety. A stale copy's pin is older than the
    /// revocation a person just took, so the merge keeps the revocation; a stale copy's pin is
    /// older than a re-pin taken after a byte change, so the merge keeps the newer pin. A tie — two
    /// processes writing the same layer in the same millisecond — is broken towards `b`, the book
    /// being merged in, which is the one whose statement the caller is making now.
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
        // **A pin a later revocation supersedes does not go in the file at all.** Keeping the row
        // would be a file that says *pinned* about a layer nobody may trust — a person reading it
        // (or a future reader of this code) would have to know to compare two timestamps, and the
        // one thing a book of statements must not do is read as the opposite of what it means.
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

    /// **The time this book's next statement carries** — strictly later than every statement in it,
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

    /// **The pin in force** for a layer directory: the row, when it is newer than any revocation.
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

/// **The trust book, as both processes read it** — see the module doc for where it lives and why.
///
/// Held behind an `Arc` by a [`crate::Stack`]: the stack is shared and a call takes `&self`, so the
/// book is a `Mutex` inside and every method takes `&self`.
pub struct Trust {
    /// `None` for a book that lives only in memory — a test, or a box with no config directory.
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
    /// **A book that is never written** — for a test, and for a host that could not resolve a
    /// config directory. Nothing is pinned, so every placed layer is adjudicated.
    pub fn in_memory() -> Arc<Trust> {
        Arc::new(Trust {
            file: None,
            stamp: Mutex::new(None),
            book: Mutex::new(Book::default()),
        })
    }

    /// **The book at `path`**, read now. A missing file is an empty book.
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

    /// **This box's book** — `$XDG_CONFIG_HOME/letibot/plugin-trust.json`, else
    /// `~/.config/letibot/plugin-trust.json`, and an in-memory one when neither resolves.
    pub fn open_default() -> Arc<Trust> {
        match Trust::default_path() {
            Some(path) => Trust::open(path),
            None => Trust::in_memory(),
        }
    }

    /// **Where this box's book is**, before it is opened — and the one door an override goes
    /// through.
    ///
    /// `LETIBOT_PLUGIN_TRUST` names a book explicitly, and an EMPTY value means *no book at all*
    /// (nothing is pinned and nothing is recorded). It exists for two real cases and not for
    /// tidiness: a test binary that drives the real desk must not write the operator's own
    /// `~/.config/letibot/plugin-trust.json` (`crates/harnessd/tests/plugins_tool.rs` sets it for
    /// exactly that reason), and a box with one home and two configurations needs to say which
    /// book a daemon reads. The same shape `LETIBOT_HOSTS` and `LETIBOT_STORE` take.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(named) = std::env::var_os("LETIBOT_PLUGIN_TRUST") {
            return (!named.is_empty()).then(|| PathBuf::from(named));
        }
        config_dir().map(|dir| dir.join(FILE))
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

    /// **Re-read the file when it moved on disk** — a pin taken by the HEAD, or by another daemon,
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

    /// **The file every writer locks**, beside the book: `plugin-trust.lock`.
    ///
    /// A sidecar and not the book itself, because the book is written by `rename` — a lock held on
    /// the old inode says nothing about the new one. `None` when there is no book on disk (an
    /// in-memory one) or when the file cannot be opened, and a save without it is a save that may
    /// lose a concurrent writer's statement: the degraded path, said out loud rather than hidden.
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

    /// **Write the book, under a lock, merging with whatever is on disk at that instant.**
    ///
    /// Two things are needed and they are not the same thing:
    ///
    /// * **A merge by time** ([`Book::merge`]), so a peer's stale copy can neither undo a
    ///   revocation nor undo a re-pin — its statement is older than the one in the file.
    /// * **The read, the merge and the write as ONE act**, which is what the exclusive lock on
    ///   `<book>.lock` buys. Without it two processes interleave inside `save` itself: each reads
    ///   a book without the other's statement, each computes a merge, and the second's rename
    ///   discards the first's. A revocation then survives only if the two never overlapped, which
    ///   is not a guarantee — it is luck with a narrow window.
    ///
    /// The lock is released when the `File` drops (or by `unlock`), including on a panic. A file
    /// whose lock cannot be taken — an exotic filesystem, a read-only directory — degrades to the
    /// merge alone rather than to no save at all: losing a concurrent statement is bad, losing the
    /// operator's own pin outright is worse.
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
        // The window a second writer could land in — a test stands in it, and only a test.
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

    /// **What the book says about these bytes.** The comparison is the two hashes and nothing
    /// else — a pin is a statement about bytes, so a rebuild is a different layer.
    pub fn standing(&self, dir: &Path, module: &str, manifest: &str) -> Standing {
        let key = key_of(dir);
        let book = self.lock();
        // **A revoked pin is not a pin at all.** The sentence a revoked layer is refused with is
        // *not pinned*, not *not pinned any more* — a person took it back, so there is nothing to
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

    /// **Pin a layer directory's bytes as they are now**, and answer what was pinned.
    ///
    /// The hashes are read from the files rather than taken from a caller: the pin has to be about
    /// what is on disk, and a caller that passed a hash it remembered would be pinning the layer
    /// it meant to write.
    pub fn pin(&self, name: &str, dir: &Path) -> Result<Pin, String> {
        let manifest = sha256_file(&dir.join(MANIFEST))
            .map_err(|why| format!("`{}` has no manifest to pin: {why}", dir.display()))?;
        let module = sha256_file(&dir.join(WASM)).unwrap_or_default();
        // **The time is taken from the book, not from the clock alone**: a pin has to be newer
        // than the revocation it replaces even when the two happen inside one millisecond.
        let at_ms = self.lock().next_ms();
        let pin = Pin {
            name: name.to_string(),
            dir: key_of(dir),
            module,
            manifest,
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

    /// **Take a pin back** — by the layer's directory, or by the name it was pinned under. The
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
            // **A tombstone, not an absence.** This row is what stops another process's stale copy
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

    /// **Every pin IN FORCE**, newest last — a revoked one is not listed, because the question this
    /// answers is *what is pinned*.
    pub fn pins(&self) -> Vec<Pin> {
        let book = self.lock();
        book.pins
            .iter()
            .filter(|p| book.in_force(&p.dir).is_some())
            .cloned()
            .collect()
    }

    /// **Record that a tool wrote these bytes**, when they are a plugin artifact.
    ///
    /// `None` when the path is not one — a write of `README.md` is not provenance about a layer,
    /// and a file of every write on the box would bury the ones that matter.
    pub fn note_write(&self, path: &Path, bytes: &[u8], session: &str) -> Option<Written> {
        if !is_plugin_artifact(path) {
            return None;
        }
        // **A write is a statement too**, so it carries a time from the book as well: a peer's
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

    /// **Who wrote the bytes in this layer's directory** — the records for anything under it: its
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

/// **The one window this file has, made reachable for a test** — `save` runs the installed hook
/// between its read and its write, which is exactly where a second process's write would land.
///
/// The same shape the job watcher's `hold_settlements_for_test` takes: a seam with no production
/// caller, `#[cfg(test)]` all the way through, so the shipped binary has no such hook to trip.
#[cfg(test)]
pub(crate) mod test_seam {
    use std::sync::Mutex;
    use std::thread::ThreadId;

    pub type Hook = Box<dyn FnOnce() + Send>;

    /// **The installing THREAD, not just the hook.** Libtest runs a file's tests in parallel, so a
    /// process-wide hook would fire inside somebody else's save — MEASURED: it did, and took a
    /// second test down with it. A seam that only its own thread can trip is a seam that cannot.
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

/// `(mtime, len)` — cheap enough to take on every poll, and enough to notice a rewrite of the same
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **Is this write about a layer?** — a module, a manifest, or a file beside one.
///
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

/// The book's file name in the config directory.
const FILE: &str = "plugin-trust.json";

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` — the same directory the user plugin scope
/// is read from (`crates/harnessd/src/plugins.rs`), so a person finds the book beside the layers
/// it is about.
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("letibot"))
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

        // **And a revocation is a statement, not an absence**: a merge must not resurrect the pin
        // from a copy on disk that still has it.
        daemon.unpin("one").expect("the pin, revoked");
        let seen = Trust::open(&file);
        assert!(seen.pins().is_empty(), "a revoked pin stays revoked");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The direction that matters: a STALE peer must not undo the operator's own act.**
    ///
    /// The reviewer's case, exactly: the operator pins, the operator revokes in the head, and a
    /// daemon whose book still holds the pin saves a write record afterwards — a model's ordinary
    /// `write` into the layer directory is enough to trigger it. A merge that let the in-memory
    /// entry win, or that treated *absent from my copy* as *never pinned*, put the pin back and the
    /// layer ran free again. It is the timestamps that stop it, and this test holds a book that
    /// never saw the revocation for the whole sequence.
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
        // **The daemon's copy is taken after the pin and never refreshed again** — it holds P1 and
        // knows nothing of the revocation, which is the worst case and the reviewer's exact one.
        let daemon = Trust::open(&file);

        head.unpin("one").expect("the operator took it back");
        assert!(
            !Trust::open(&file)
                .standing(&dir, &pin.module, &pin.manifest)
                .free,
            "the revocation is in force"
        );

        // **And now the stale daemon saves**, for a reason of its own: a model wrote a source file
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
        // **And the file says so too**: the stale writer must not even put the row back, or a
        // person reading their own book would see a pin that is not in force.
        let on_disk = std::fs::read_to_string(&file).expect("the book");
        assert!(
            !on_disk.contains(&pin.module),
            "the revoked pin's row is gone from the file: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The interleaving the merge alone cannot survive**: a second writer lands INSIDE `save`'s
    /// own window — between this process's read of the book and its rename over it.
    ///
    /// This is the reviewer's finding, made deterministic. Two books over one file; the first is
    /// paused by [`test_seam`] exactly where a peer's write would land, and the peer then pins a
    /// DIFFERENT layer. Without the exclusive lock the peer reads a book that does not have the
    /// first's statement, merges, writes, and the first's rename then discards it — so the operator's
    /// pin is silently gone. With the lock the peer waits, and both statements are in the file.
    ///
    /// Three assertions, and the last one is the one that cannot be passed by luck: the second
    /// writer is still waiting, its pin succeeded, and the book holds both.
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

    /// **The other direction: a stale peer must not undo a RE-PIN either.** After a byte change the
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

        // The stale peer saves a write record — its own news, about the old pin's world.
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
}
