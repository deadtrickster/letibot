//! The vocabulary a harness is opened with. A `Harness` renders its stable prefix at open, so it
//! needs the GGUF even where no model is ever asked for a token. The operator's own file wins over
//! the box's default, because a box that is not the one this repository is developed on has none.

use std::path::PathBuf;

pub fn gguf() -> Option<PathBuf> {
    std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(PathBuf::from)
        .or_else(letibot_harnessd::vocab_gguf)
}
