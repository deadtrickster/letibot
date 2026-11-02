//! A tiny BERT, built by the test: three files, no download, no binary in the tree.
//!
//! The weights are chosen so the model's answer is readable rather than arbitrary: each
//! word is its own direction, the encoder's projections are identities and its MLP is
//! zero, so a sentence's vector is a function of the words it is made of. That is what
//! makes "a paraphrase scores above an unrelated line" a fact of the fixture and not a
//! lucky draw.
//!
//! The file shapes are the real ones - `config.json`, `tokenizer.json`, a
//! `model.safetensors` with `bert.`-prefixed keys - so the internal path a user's own
//! checkpoint takes is the path this exercises.

use std::collections::BTreeMap;
use std::path::Path;

/// The fixture's hidden width, which is also its vocabulary: one word, one direction.
pub const HIDDEN: usize = 24;
/// The fixture's vocabulary: five specials and the words below.
pub const VOCAB: usize = 24;
/// Positional slots, enough for a sentence the tests write.
pub const POSITIONS: usize = 64;

/// The words the fixture's tokenizer knows. A word outside this list is `[UNK]`, which
/// is why the tests are written in these.
pub const WORDS: [&str; 8] = [
    "alpha", "beta", "gamma", "delta", "kappa", "lambda", "mu", "nu",
];

const SPECIALS: [&str; 5] = ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]"];

/// Write every file a `model = "<dir>"` needs, into `dir`.
pub fn tiny_bert(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("config.json"), config_json())?;
    std::fs::write(dir.join("tokenizer.json"), tokenizer_json())?;
    std::fs::write(
        dir.join("model.safetensors"),
        weights().map_err(std::io::Error::other)?,
    )
    .map_err(std::io::Error::other)?;
    Ok(())
}

fn config_json() -> String {
    format!(
        r#"{{
  "model_type": "bert",
  "vocab_size": {VOCAB},
  "hidden_size": {HIDDEN},
  "num_hidden_layers": 1,
  "num_attention_heads": 2,
  "intermediate_size": 32,
  "hidden_act": "gelu",
  "hidden_dropout_prob": 0.0,
  "max_position_embeddings": {POSITIONS},
  "type_vocab_size": 2,
  "initializer_range": 0.02,
  "layer_norm_eps": 1e-12,
  "pad_token_id": 0
}}"#
    )
}

fn tokenizer_json() -> String {
    let mut vocab: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, word) in SPECIALS.iter().chain(WORDS.iter()).enumerate() {
        vocab.insert(word, i);
    }
    format!(
        r##"{{
  "version": "1.0",
  "truncation": null,
  "padding": null,
  "added_tokens": [],
  "normalizer": {{"type": "BertNormalizer", "clean_text": true,
                   "handle_chinese_chars": false, "strip_accents": null,
                   "lowercase": true}},
  "pre_tokenizer": {{"type": "BertPreTokenizer"}},
  "post_processor": {{"type": "BertProcessing",
                      "sep": ["[SEP]", 3], "cls": ["[CLS]", 2]}},
  "decoder": null,
  "model": {{"type": "WordPiece", "unk_token": "[UNK]",
              "continuing_subword_prefix": "#",
              "max_input_chars_per_word": 100,
              "vocab": {vocab:?}}}
}}"##
    )
}

/// The checkpoint: `(name, shape, values)` in the container `safetensors` defines.
fn weights() -> Result<Vec<u8>, String> {
    let mut tensors: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::new();

    // One direction per word: the `i`-th row of the embedding is one-hot at `i`.
    let mut word = vec![0f32; VOCAB * HIDDEN];
    for i in 0..VOCAB {
        word[i * HIDDEN + i] = 1.0;
    }
    tensors.push((
        "bert.embeddings.word_embeddings.weight".into(),
        vec![VOCAB, HIDDEN],
        word,
    ));
    tensors.push((
        "bert.embeddings.position_embeddings.weight".into(),
        vec![POSITIONS, HIDDEN],
        vec![0.0; POSITIONS * HIDDEN],
    ));
    tensors.push((
        "bert.embeddings.token_type_embeddings.weight".into(),
        vec![2, HIDDEN],
        vec![0.0; 2 * HIDDEN],
    ));
    tensors.push((
        "bert.embeddings.LayerNorm.weight".into(),
        vec![HIDDEN],
        vec![1.0; HIDDEN],
    ));
    tensors.push((
        "bert.embeddings.LayerNorm.bias".into(),
        vec![HIDDEN],
        vec![0.0; HIDDEN],
    ));

    let identity = {
        let mut m = vec![0f32; HIDDEN * HIDDEN];
        for i in 0..HIDDEN {
            m[i * HIDDEN + i] = 1.0;
        }
        m
    };
    let layer = "bert.encoder.layer.0";
    for part in ["query", "key", "value"] {
        tensors.push((
            format!("{layer}.attention.self.{part}.weight"),
            vec![HIDDEN, HIDDEN],
            identity.clone(),
        ));
        tensors.push((
            format!("{layer}.attention.self.{part}.bias"),
            vec![HIDDEN],
            vec![0.0; HIDDEN],
        ));
    }
    for part in ["attention.output", "output"] {
        tensors.push((
            format!("{layer}.{part}.LayerNorm.weight"),
            vec![HIDDEN],
            vec![1.0; HIDDEN],
        ));
        tensors.push((
            format!("{layer}.{part}.LayerNorm.bias"),
            vec![HIDDEN],
            vec![0.0; HIDDEN],
        ));
    }
    tensors.push((
        format!("{layer}.attention.output.dense.weight"),
        vec![HIDDEN, HIDDEN],
        identity,
    ));
    tensors.push((
        format!("{layer}.attention.output.dense.bias"),
        vec![HIDDEN],
        vec![0.0; HIDDEN],
    ));
    // The MLP is zero, so a layer is its attention plus its residual: what a word means
    // does not get scrambled by a matrix nobody trained.
    tensors.push((
        format!("{layer}.intermediate.dense.weight"),
        vec![32, HIDDEN],
        vec![0.0; 32 * HIDDEN],
    ));
    tensors.push((
        format!("{layer}.intermediate.dense.bias"),
        vec![32],
        vec![0.0; 32],
    ));
    tensors.push((
        format!("{layer}.output.dense.weight"),
        vec![HIDDEN, 32],
        vec![0.0; HIDDEN * 32],
    ));
    tensors.push((
        format!("{layer}.output.dense.bias"),
        vec![HIDDEN],
        vec![0.0; HIDDEN],
    ));

    safetensors_bytes(&tensors)
}

/// The safetensors container, written by hand: `u64` little-endian header length, the
/// JSON header, then every tensor's bytes in the order the header lists them.
fn safetensors_bytes(tensors: &[(String, Vec<usize>, Vec<f32>)]) -> Result<Vec<u8>, String> {
    let mut header = serde_json::Map::new();
    let mut data: Vec<u8> = Vec::new();
    for (name, shape, values) in tensors {
        if shape.iter().product::<usize>() != values.len() {
            return Err(format!(
                "{name}: shape {shape:?} does not hold {} values",
                values.len()
            ));
        }
        let begin = data.len();
        for v in values {
            data.extend_from_slice(&v.to_le_bytes());
        }
        header.insert(
            name.clone(),
            serde_json::json!({
                "dtype": "F32",
                "shape": shape,
                "data_offsets": [begin, data.len()],
            }),
        );
    }
    let mut json = serde_json::to_vec(&serde_json::Value::Object(header))
        .map_err(|e| format!("header: {e}"))?;
    // The container's header is padded so the tensors start on an eight-byte boundary.
    while (8 + json.len()) % 8 != 0 {
        json.push(b' ');
    }
    let mut out = Vec::with_capacity(8 + json.len() + data.len());
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&data);
    Ok(out)
}
