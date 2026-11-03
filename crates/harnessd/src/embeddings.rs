//! The box's one embedder: `[embeddings]` read off the project file, the model loaded once at
//! daemon start, and every caller scoring through the tiers that come off it.
//!
//! A tier is per caller and holds that caller's corpus's vectors; the embedder underneath it is
//! one per box, so four callers load one model and a text is embedded once. `mode = "off"` - the
//! default, and every box that never wrote the section - is no embedder at all, and then each
//! caller's own lexical floor is the whole score.

use std::path::Path;
use std::sync::Arc;

use letibot_embeddings::{Embedder, Settings, Tier};

/// Build the embedder a `[embeddings]` section asks for, or `None` when it asks for nothing.
/// `Err` is a section the box DID ask for and did not get, in the words of the key that refused
/// it: a typo in `mode`, a model directory that is not there, or a cache that will not open.
pub fn build(file: &Path, data_dir: Option<&Path>) -> Result<Option<Arc<Embedder>>, String> {
    let settings = Settings::load(file).map_err(|e| e.to_string())?;
    embedder(&settings, data_dir)
}

/// [`build`] from a section already read, which is what the daemon has once it has merged the two
/// `leticode.toml` levels: the file named is the one whose section won.
pub fn embedder(
    settings: &Settings,
    data_dir: Option<&Path>,
) -> Result<Option<Arc<Embedder>>, String> {
    if settings.mode.is_off() {
        return Ok(None);
    }
    let dir = data_dir.ok_or_else(|| {
        format!(
            "the box has no data directory, so `{}` cannot be kept",
            letibot_embeddings::db_path(Path::new(""))
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "embeddings.db".to_string())
        )
    })?;
    Embedder::open(settings, dir).map_err(|e| e.to_string())
}

/// One caller's tier over the box's embedder: `None` when the box has none, which is the lexical
/// score exactly. The vectors stay with the caller - a note is never scored against a skill.
pub fn tier(embedder: Option<&Arc<Embedder>>) -> Option<Tier> {
    embedder.map(|e| Tier::over(Arc::clone(e)))
}

/// **The sentence a session says once** when the box's embedder could not answer, and `None` the
/// rest of the session's turns. Asked at the round boundary, by which time every caller has
/// already fallen back to its own words - so this is a disclosure and never a decision.
pub fn fault_note(embedder: Option<&Arc<Embedder>>) -> Option<String> {
    let said = embedder?.take_fault()?;
    Some(format!(
        "the second similarity tier could not answer, so every score this session makes is the \
         words alone, until it answers again: {said}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A directory that removes itself.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!(
                "letibot-embeddings-{tag}-{}-{}",
                std::process::id(),
                letibot_metrics::now_ms()
            ));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Dir(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A file with the section in it, and the data directory its cache lands in.
    fn section(dir: &Dir, body: &str) -> PathBuf {
        let file = dir.path().join("leticode.toml");
        std::fs::write(&file, body).expect("the fixture file");
        file
    }

    /// **A box that never wrote the key has no embedder**, and that is not a failure: it is the
    /// default this whole feature is off behind.
    #[test]
    fn no_section_is_no_embedder() {
        let dir = Dir::new("none");
        let file = section(&dir, "[default]\nmain_model = \"m\"\n");
        assert!(build(&file, Some(dir.path())).expect("no fault").is_none());
        assert!(tier(None).is_none());
    }

    /// A section the box asked for and did not get is a fault with a sentence, not a silence: the
    /// mode, the key or the directory that refused it.
    #[test]
    fn a_section_that_cannot_be_built_says_which_key_refused_it() {
        let dir = Dir::new("bad");
        // `model` under `off` would do nothing, and the config refuses it by name.
        let off = section(
            &dir,
            "[embeddings]\nmode = \"off\"\nmodel = \"/tmp/nowhere\"\n",
        );
        let said = build(&off, Some(dir.path())).expect_err("a fault");
        assert!(said.contains("mode = \"off\""), "{said}");
        // A model directory that is not there names the file it wanted.
        let missing = section(
            &dir,
            &format!(
                "[embeddings]\nmode = \"internal\"\nmodel = \"{}\"\n",
                dir.path().join("no-model").display()
            ),
        );
        let said = build(&missing, Some(dir.path())).expect_err("a fault");
        assert!(said.contains("no-model"), "{said}");
    }

    /// **A tier that cannot answer is said once, and the callers keep their words.** The sentence
    /// names what refused it, the second ask gets nothing, and a box with no embedder never asks.
    #[test]
    fn a_failing_embedder_is_said_once_and_a_box_with_none_is_silent() {
        let dir = Dir::new("fault");
        let file = section(&dir, "[embeddings]\nmode = \"http://127.0.0.1:1\"\n");
        let embedder = build(&file, Some(dir.path()))
            .expect("no fault yet: the endpoint is refused at the first call")
            .expect("an embedder");
        let notes = tier(Some(&embedder)).expect("a tier");
        assert!(notes.query("alpha", 1).is_err());
        let said = fault_note(Some(&embedder)).expect("the first failure is said");
        assert!(said.contains("127.0.0.1:1"), "{said}");
        assert!(said.contains("words alone"), "{said}");
        assert_eq!(fault_note(Some(&embedder)), None, "once, not every round");
        assert_eq!(
            fault_note(None),
            None,
            "a box with no tier has nothing to say"
        );
    }

    /// And a box that asks for the tier gets one: the model it named, over the cache in its data
    /// directory, with a tier per caller above it.
    #[test]
    fn a_box_that_asks_gets_one_embedder_and_a_tier_per_caller() {
        let dir = Dir::new("asked");
        let model = dir.path().join("model");
        letibot_embeddings::testing::tiny_bert(&model).expect("the fixture model");
        let file = section(
            &dir,
            &format!(
                "[embeddings]\nmode = \"internal\"\nmodel = \"{}\"\n",
                model.display()
            ),
        );
        let embedder = build(&file, Some(dir.path()))
            .expect("no fault")
            .expect("an embedder");
        assert_eq!(embedder.model(), model.display().to_string());
        let notes = tier(Some(&embedder)).expect("a tier");
        let skills = tier(Some(&embedder)).expect("a tier");
        // Two callers, one engine: the same vector comes back from either, and neither's
        // documents are in the other's index.
        let a = notes.embedder().vector("alpha beta", 1).expect("a vector");
        let b = skills.embedder().vector("alpha beta", 1).expect("a vector");
        assert_eq!(a, b);
        assert_eq!(skills.cosine("a-note", &a), None);
    }
}
