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

/// The book: the pins, and the writes that were recorded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Book {
    pins: Vec<Pin>,
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
        serde_json::to_string_pretty(&json!({ "pins": pins, "writes": writes }))
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
        match config_dir() {
            Some(dir) => Trust::open(dir.join(FILE)),
            None => Trust::in_memory(),
        }
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

    fn save(&self) {
        let Some(file) = self.file.as_deref() else {
            return;
        };
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
    }

    /// **What the book says about these bytes.** The comparison is the two hashes and nothing
    /// else — a pin is a statement about bytes, so a rebuild is a different layer.
    pub fn standing(&self, dir: &Path, module: &str, manifest: &str) -> Standing {
        let key = key_of(dir);
        let book = self.lock();
        match book.pins.iter().find(|p| p.dir == key) {
            Some(pin) if pin.module == module && pin.manifest == manifest => Standing {
                free: true,
                pin: Some(pin.clone()),
            },
            Some(pin) => Standing {
                free: false,
                pin: Some(pin.clone()),
            },
            None => Standing {
                free: false,
                pin: None,
            },
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
        let pin = Pin {
            name: name.to_string(),
            dir: key_of(dir),
            module,
            manifest,
            at_ms: now_ms(),
        };
        {
            let mut book = self.lock();
            book.pins.retain(|p| p.dir != pin.dir);
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
            book.pins.remove(at)
        };
        self.save();
        Some(taken)
    }

    /// Every pin, newest last.
    pub fn pins(&self) -> Vec<Pin> {
        self.lock().pins.clone()
    }

    /// **Record that a tool wrote these bytes**, when they are a plugin artifact.
    ///
    /// `None` when the path is not one — a write of `README.md` is not provenance about a layer,
    /// and a file of every write on the box would bury the ones that matter.
    pub fn note_write(&self, path: &Path, bytes: &[u8], session: &str) -> Option<Written> {
        if !is_plugin_artifact(path) {
            return None;
        }
        let written = Written {
            path: key_of(path),
            sha256: sha256_hex(bytes),
            at_ms: now_ms(),
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
