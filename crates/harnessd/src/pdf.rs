//! rano's PDF renderers, registered with the crate that keeps a document as bytes.
//!
//! A row keeps a PDF inline, so nothing above this crate can hand a renderer a path: the transcript
//! crate writes the temporary file and calls these. rano owns the work - `pdfinfo`/a page-tree parse
//! for the count, `pdftoppm`/`gs` for a page picture, `pdftotext` for the text - with the timeouts
//! and output caps already in it, which is the whole reason not to write a second one here.

use std::path::Path;
use std::sync::Arc;

use letibot_transcript::pdf::Renderer;

/// `rano::pdf`, behind the trait, so `letibot-transcript` and `letibot-provider` need not depend on
/// rano to read a document.
struct Rano;

impl Renderer for Rano {
    fn page_count(&self, path: &Path) -> Result<u32, String> {
        rano::pdf::page_count(path)
    }

    fn page_text(&self, path: &Path, page: u32) -> Result<String, String> {
        rano::pdf::page_text(path, page)
    }

    fn page_png(&self, path: &Path, page: u32, dpi: u32) -> Result<Vec<u8>, String> {
        rano::pdf::page_png(path, page, dpi)
    }

    fn markdown(&self, path: &Path, title: &str) -> Result<String, String> {
        rano::pdf::document_markdown(path, title)
    }
}

/// Register the renderers once per process. Returns whether this call was the one that set them,
/// which is the same contract every other daemon-wide registration here has.
pub fn install() -> bool {
    letibot_transcript::pdf::set_renderer(Arc::new(Rano))
}
