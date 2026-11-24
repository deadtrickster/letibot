//! rano's PDF renderers, behind `letibot_transcript::pdf`'s trait: a document's page count, one page
//! as a PNG, one page as text, and the whole document as markdown.
//!
//! `letibot-transcript` is the bottom of the graph and keeps a document as BYTES, so it cannot name a
//! renderer; a host registers one here, once, and this crate is where both hosts get the same one -
//! the daemon renders a document for a model, and the head renders a page for a reader. The work is
//! rano's (`pdfinfo` or a page-tree parse, `pdftoppm`/`gs`, `pdftotext`), with the timeouts and
//! output caps already in it, which is the whole reason not to write a second one.

use std::path::Path;
use std::sync::Arc;

use letibot_transcript::pdf::Renderer;

/// The trait over `rano::pdf`, one method each.
struct RanoPdf;

impl Renderer for RanoPdf {
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

/// Register the renderer with the crate that keeps a document as bytes. Returns whether this call was
/// the one that set it - the first wins, because a process has one answer to one question.
pub fn install() -> bool {
    letibot_transcript::pdf::set_renderer(Arc::new(RanoPdf))
}
