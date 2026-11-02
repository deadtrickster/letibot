//! The user's own checkpoint, run in this process by candle.
//!
//! `model = "<dir>"` is a directory with `config.json`, `tokenizer.json` and
//! `model.safetensors`, and `mode = "internal"` with no `model` is the built-in one in
//! the data directory. Both go through the same code, because they are the same thing:
//! a BERT-family encoder and a tokenizer beside it.
//!
//! An architecture this build does not run, and a directory that is missing a file, are
//! refused at open by name - a config that only fails at the first round is a config
//! that fails while somebody is working.

use std::path::{Path, PathBuf};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

/// The model the entry decided on: six layers, 384 wide, English, and small enough to
/// run beside a turn rather than after it.
pub const BUILTIN_MODEL: &str = "all-MiniLM-L6-v2";

/// The three files a checkpoint directory must have, in the order a refusal lists them.
pub const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// How many tokens one text may be. MiniLM's own limit is 512, and a note is a paragraph:
/// the tail of anything longer is dropped rather than paid for on every round.
pub const MAX_TOKENS: usize = 256;

/// The architectures this build runs: BERT and the fork of it `nomic-embed-text` ships.
const BERT_FAMILY: [&str; 2] = ["bert", "nomic_bert"];

/// Where the built-in model lives once somebody has put it there.
pub fn builtin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(BUILTIN_MODEL)
}

/// Why a checkpoint could not be opened or run. Every variant says the directory, because
/// the directory is what the operator wrote in `leticode.toml`.
#[derive(Debug)]
pub enum ModelError {
    /// A file the directory does not have.
    Missing {
        dir: PathBuf,
        missing: Vec<&'static str>,
    },
    /// A `config.json` this build cannot read.
    Config { dir: PathBuf, why: String },
    /// An architecture candle here does not run, named.
    Architecture { dir: PathBuf, model_type: String },
    /// A `tokenizer.json` that will not load.
    Tokenizer { dir: PathBuf, why: String },
    /// Weights that do not match the config beside them.
    Weights { dir: PathBuf, why: String },
    /// The forward pass itself.
    Forward(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Missing { dir, missing } => write!(
                f,
                "{} is not a model directory: it has no {}",
                dir.display(),
                missing.join(", no ")
            ),
            ModelError::Config { dir, why } => {
                write!(f, "{}/config.json: {why}", dir.display())
            }
            ModelError::Architecture { dir, model_type } => write!(
                f,
                "{}/config.json says model_type = \"{model_type}\", which this build does not \
                 run; it runs a BERT-family encoder ({})",
                dir.display(),
                BERT_FAMILY.join(", ")
            ),
            ModelError::Tokenizer { dir, why } => {
                write!(f, "{}/tokenizer.json: {why}", dir.display())
            }
            ModelError::Weights { dir, why } => {
                write!(f, "{}/model.safetensors: {why}", dir.display())
            }
            ModelError::Forward(why) => write!(f, "the embedding forward pass: {why}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// One loaded checkpoint: the tokenizer, the weights, and the name a cache key is built
/// from.
pub struct Model {
    tokenizer: Tokenizer,
    bert: BertModel,
    device: Device,
    name: String,
    dim: usize,
}

impl Model {
    /// Open the directory the config named.
    pub fn open(dir: &Path) -> Result<Model, ModelError> {
        Model::load(dir, dir.display().to_string())
    }

    /// Open the built-in model out of the data directory. A box that has never been given
    /// one is refused with the directory's name, which is the line to act on.
    pub fn open_builtin(data_dir: &Path) -> Result<Model, ModelError> {
        Model::load(&builtin_dir(data_dir), BUILTIN_MODEL.to_string())
    }

    fn load(dir: &Path, name: String) -> Result<Model, ModelError> {
        let missing: Vec<&'static str> = FILES
            .iter()
            .copied()
            .filter(|f| !dir.join(f).is_file())
            .collect();
        if !missing.is_empty() {
            return Err(ModelError::Missing {
                dir: dir.to_path_buf(),
                missing,
            });
        }
        let raw =
            std::fs::read_to_string(dir.join("config.json")).map_err(|e| ModelError::Config {
                dir: dir.to_path_buf(),
                why: e.to_string(),
            })?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| ModelError::Config {
                dir: dir.to_path_buf(),
                why: e.to_string(),
            })?;
        let model_type = value
            .get("model_type")
            .and_then(|m| m.as_str())
            .unwrap_or("bert");
        if !BERT_FAMILY.contains(&model_type) {
            return Err(ModelError::Architecture {
                dir: dir.to_path_buf(),
                model_type: model_type.to_string(),
            });
        }
        let config: BertConfig = serde_json::from_value(value).map_err(|e| ModelError::Config {
            dir: dir.to_path_buf(),
            why: format!("this build reads a BERT config: {e}"),
        })?;
        let dim = config.hidden_size;

        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| {
            ModelError::Tokenizer {
                dir: dir.to_path_buf(),
                why: e.to_string(),
            }
        })?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| ModelError::Tokenizer {
                dir: dir.to_path_buf(),
                why: format!("truncation: {e}"),
            })?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                DType::F32,
                &device,
            )
        }
        .map_err(|e| ModelError::Weights {
            dir: dir.to_path_buf(),
            why: e.to_string(),
        })?;
        let bert = BertModel::load(vb, &config).map_err(|e| ModelError::Weights {
            dir: dir.to_path_buf(),
            why: e.to_string(),
        })?;
        Ok(Model {
            tokenizer,
            bert,
            device,
            name,
            dim,
        })
    }

    /// The name a cache key is built from: the built-in model's pinned name, or the
    /// directory the user pointed at - a change of either is a change of key.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How wide a vector this checkpoint makes.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// One vector per text: mean-pooled over the tokens and unit length, which is what a
    /// cosine wants.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        texts.iter().map(|text| self.embed_one(text)).collect()
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, ModelError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| ModelError::Tokenizer {
                dir: PathBuf::from(&self.name),
                why: e.to_string(),
            })?;
        let ids = encoding.get_ids();
        if ids.is_empty() {
            return Ok(vec![0.0; self.dim]);
        }
        let input = Tensor::new(ids, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        let types = input
            .zeros_like()
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        let mask = input
            .ones_like()
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        let hidden = self
            .bert
            .forward(&input, &types, Some(&mask))
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        // Mean pooling over every token: no padding is used, so the mask is all ones and
        // the mean is over the sentence's own length.
        let pooled = hidden
            .sum(1)
            .and_then(|t| t / (ids.len() as f64))
            .and_then(|t| t.squeeze(0))
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        let mut out: Vec<f32> = pooled
            .to_vec1()
            .map_err(|e| ModelError::Forward(e.to_string()))?;
        let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            for x in &mut out {
                *x /= norm;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosine::cosine;
    use crate::testing as fixture;

    /// A directory that removes itself, so a fixture model never outlives its test.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "letibot-model-{tag}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    /// A refusal, without asking `Model` to be `Debug`: a candle model holds a graph and
    /// deriving `Debug` on it would print megabytes.
    fn refused(result: Result<Model, ModelError>) -> ModelError {
        match result {
            Ok(_) => panic!("expected a refusal"),
            Err(e) => e,
        }
    }

    fn tiny() -> (Dir, Model) {
        let dir = Dir::new("tiny");
        fixture::tiny_bert(dir.path()).expect("write the fixture");
        let model = Model::open(dir.path()).expect("open the fixture");
        (dir, model)
    }

    /// A directory with nothing in it is refused BY NAME, listing every file it wants -
    /// which is the line an operator acts on.
    #[test]
    fn a_directory_without_the_three_files_is_refused_by_name() {
        let dir = Dir::new("empty");
        let err = refused(Model::open(dir.path()));
        match &err {
            ModelError::Missing { missing, .. } => {
                assert_eq!(missing, &FILES.to_vec());
            }
            other => panic!("expected Missing, got {other}"),
        }
        let said = err.to_string();
        for f in FILES {
            assert!(said.contains(f), "{said} does not name {f}");
        }
    }

    /// An architecture this build does not run is refused at open, not at the first
    /// round: the operator is told the word in their own config.
    #[test]
    fn an_architecture_this_build_does_not_run_is_refused_by_name() {
        let dir = Dir::new("arch");
        fixture::tiny_bert(dir.path()).expect("write the fixture");
        let config = std::fs::read_to_string(dir.path().join("config.json"))
            .unwrap()
            .replace("\"bert\"", "\"gemma3\"");
        std::fs::write(dir.path().join("config.json"), config).unwrap();
        let err = refused(Model::open(dir.path()));
        assert!(
            matches!(&err, ModelError::Architecture { model_type, .. } if model_type == "gemma3"),
            "{err}"
        );
        assert!(err.to_string().contains("gemma3"), "{err}");
    }

    /// A box that was never given the built-in model says where it looked, rather than
    /// embedding nothing.
    #[test]
    fn the_builtin_model_is_a_directory_in_the_data_directory_and_is_named_when_absent() {
        let dir = Dir::new("builtin");
        let path = builtin_dir(dir.path());
        assert!(path.ends_with(format!("models/{BUILTIN_MODEL}")));
        let err = refused(Model::open_builtin(dir.path()));
        assert!(err.to_string().contains(BUILTIN_MODEL), "{err}");
    }

    /// The built-in name is the cache's model key for it, and a user's directory is its
    /// own key, so two checkpoints never share a row.
    #[test]
    fn a_model_is_named_for_the_cache_by_what_it_is() {
        let (dir, model) = tiny();
        assert_eq!(model.name(), dir.path().display().to_string());
        assert_eq!(model.dim(), fixture::HIDDEN);
    }

    /// One text, one unit vector, the same one every time: a score that moved between two
    /// calls would make every floor a coin toss.
    #[test]
    fn embedding_is_deterministic_and_of_unit_length() {
        let (_dir, model) = tiny();
        let once = model.embed(&["alpha beta gamma"]).unwrap();
        let twice = model.embed(&["alpha beta gamma"]).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once[0].len(), fixture::HIDDEN);
        let norm = once[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm {norm}");
    }

    /// The case the whole entry is about: a line that shares its words is closer than a
    /// line that shares none, through a real BERT forward pass.
    #[test]
    fn a_longer_paraphrase_is_closer_than_an_unrelated_line() {
        let (_dir, model) = tiny();
        let v = model
            .embed(&[
                "alpha beta gamma",
                "alpha beta gamma delta",
                "kappa lambda mu nu",
            ])
            .unwrap();
        let paraphrase = cosine(&v[0], &v[1]);
        let unrelated = cosine(&v[0], &v[2]);
        assert!(
            paraphrase > unrelated,
            "a paraphrase scored {paraphrase}, an unrelated line {unrelated}"
        );
    }

    /// A text whose words the tokenizer does not know is still a vector rather than a
    /// panic: the tier's answer for it is a real one, and the caller's floor decides.
    #[test]
    fn a_text_of_unknown_words_is_still_a_vector() {
        let (_dir, model) = tiny();
        let v = model.embed(&["qqq zzz"]).unwrap();
        assert_eq!(v[0].len(), fixture::HIDDEN);
        let norm = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm {norm}");
    }

    /// A batch is one vector per text, in the order they were handed over.
    #[test]
    fn a_batch_keeps_its_order_and_its_length() {
        let (_dir, model) = tiny();
        let batch = model.embed(&["alpha", "beta", "alpha"]).unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0], batch[2]);
        assert_ne!(batch[0], batch[1]);
    }
}
