//! The PDF work this tree does not own. A row keeps a document as BYTES, so nothing here can hand a
//! renderer a path of its own: these are the bytes-facing calls, and they own the temporary file so
//! no caller has one to leak.
//!
//! The renderers are `rano::pdf`'s - page count, one page as a PNG, one page as text, the whole
//! document as markdown, with timeouts and output caps already in it. This crate cannot depend on
//! rano (it is the bottom of the graph), so the daemon registers it here once, and a build with
//! nothing registered still reads a document's text through `pdftotext` on PATH: that is what a
//! tool call or a test on a box with no daemon may assume, and nothing more.

use std::path::Path;
use std::sync::{Arc, OnceLock};

/// What a PDF renderer answers. The daemon's implementation is `rano::pdf`; a test's is whatever it
/// can prove without a rasterizer on the box.
pub trait Renderer: Send + Sync {
    /// How many pages this document has.
    fn page_count(&self, path: &Path) -> Result<u32, String>;
    /// One page as text, `page` zero-based.
    fn page_text(&self, path: &Path, page: u32) -> Result<String, String>;
    /// One page as a PNG, `page` zero-based, `dpi` inside the renderer's own bounds.
    fn page_png(&self, path: &Path, page: u32, dpi: u32) -> Result<Vec<u8>, String>;
    /// The whole document as markdown, `title` as its heading.
    fn markdown(&self, path: &Path, title: &str) -> Result<String, String>;
}

/// The most pages a document is turned into pictures for: past ten, a model that cannot take the
/// file is better served by its text than by a first ten pages it cannot know are partial.
pub const MAX_PAGES_AS_IMAGES: u32 = 10;

/// The dpi a page is rendered at when a model is given pictures: legible in a terminal cell, and
/// under the renderer's own high bound.
pub const PAGE_DPI: u32 = 150;

static RENDERER: OnceLock<Arc<dyn Renderer>> = OnceLock::new();

/// Register the renderer. The FIRST registration wins, because a process has one daemon and a
/// second setter would be a second answer to the same question mid-session.
pub fn set_renderer(renderer: Arc<dyn Renderer>) -> bool {
    RENDERER.set(renderer).is_ok()
}

/// The renderer, when one is registered.
pub fn renderer() -> Option<&'static Arc<dyn Renderer>> {
    RENDERER.get()
}

/// Run `f` with the document written to a temporary file. `None` when the box has no writable
/// temporary directory, which is the one failure a caller cannot tell from a renderer's.
fn with_path<T>(bytes: &[u8], f: impl FnOnce(&Path) -> T) -> Option<T> {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("letibot-doc-{}-{}.pdf", std::process::id(), {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }));
    let written = (|| {
        let mut file = std::fs::File::create(&path).ok()?;
        file.write_all(bytes).ok()?;
        file.flush().ok()
    })();
    if written.is_none() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let out = f(&path);
    let _ = std::fs::remove_file(&path);
    Some(out)
}

/// The document as markdown - a heading a page and its laid-out text - or `None` when nothing here
/// can read it. The registered renderer first; without one, `pdftotext` alone, laid out by page.
pub fn markdown(bytes: &[u8], title: &str) -> Option<String> {
    markdown_via(renderer().map(|r| r.as_ref()), bytes, title)
}

/// [`markdown`] with the renderer given rather than looked up: the registration is process-wide, so
/// a test that set it would be a test of every other test's answer too.
fn markdown_via(r: Option<&dyn Renderer>, bytes: &[u8], title: &str) -> Option<String> {
    match r {
        Some(r) => with_path(bytes, |p| r.markdown(p, title)).and_then(Result::ok),
        None => pdftotext_markdown(bytes, title),
    }
}

/// How many pages, or `None` when no renderer is registered: a count is the renderer's, and a
/// number this module guessed from the bytes is one a caller would act on.
pub fn page_count(bytes: &[u8]) -> Option<u32> {
    let r = renderer()?;
    with_path(bytes, |p| r.page_count(p)).and_then(Result::ok)
}

/// One page's text, `page` zero-based, or `None` when no renderer is registered or the renderer
/// refused that page.
pub fn page_text(bytes: &[u8], page: u32) -> Option<String> {
    let r = renderer()?;
    with_path(bytes, |p| r.page_text(p, page)).and_then(Result::ok)
}

/// One page as a PNG, `page` zero-based, at `dpi`, or `None` when no renderer is registered or the
/// renderer refused it. The picture a reader is shown one page at a time - [`pages_as_png`] is the
/// model's ten, and a card that turns pages asks for the one it is about to draw.
pub fn page_png(bytes: &[u8], page: u32, dpi: u32) -> Option<Vec<u8>> {
    let r = renderer()?;
    with_path(bytes, |p| r.page_png(p, page, dpi)).and_then(Result::ok)
}

/// Up to [`MAX_PAGES_AS_IMAGES`] pages as PNGs, in order, with the count of pages the document has.
/// The renderer's, or nothing: a picture this module could not render is not one a provider may
/// describe as sent.
pub fn pages_as_png(bytes: &[u8], dpi: u32) -> Option<(u32, Vec<Vec<u8>>)> {
    pages_via(renderer().map(|r| r.as_ref()), bytes, dpi)
}

fn pages_via(r: Option<&dyn Renderer>, bytes: &[u8], dpi: u32) -> Option<(u32, Vec<Vec<u8>>)> {
    let r = r?;
    with_path(bytes, |path| {
        let pages = r.page_count(path).ok()?;
        let mut shots = Vec::new();
        for page in 0..pages.min(MAX_PAGES_AS_IMAGES) {
            match r.page_png(path, page, dpi) {
                Ok(png) => shots.push(png),
                // One page a rasterizer refuses is where the pictures stop: a gap in the middle
                // would read as a document with pages missing, which is worse than its text.
                Err(_) => break,
            }
        }
        Some((pages, shots))
    })
    .flatten()
}

/// The `pdftotext` fallback, one page a heading, which is the shape a reader of a prompt already
/// knows from `rano::pdf::document_markdown`. `None` when the box has no extractor or it printed
/// nothing.
fn pdftotext_markdown(bytes: &[u8], title: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("pdftotext")
        .args(["-layout", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    // Fed from a thread: a document larger than a pipe buffer blocks the writer, and doing that
    // before reading the extractor's own output deadlocks as soon as that pipe fills too.
    let out = std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(bytes);
        });
        child.wait_with_output().ok()
    })?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    if text.trim().is_empty() {
        return None;
    }
    let mut said = format!("# {title}\n");
    // `pdftotext` ends each page with a form feed, and the file with one.
    for (n, page) in text.split('\u{c}').enumerate() {
        if page.trim().is_empty() {
            continue;
        }
        said.push_str(&format!("\n## Page {}\n\n```text\n", n + 1));
        said.push_str(page.trim_end_matches(['\n', '\r']));
        said.push_str("\n```\n");
    }
    Some(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A renderer that answers from a table, so this module's own half - the temporary file, the
    /// page cap, the fallback - is what the tests measure, on any box. Injected rather than
    /// registered: the registration is process-wide, and two tests setting it would be two tests
    /// measuring each other.
    struct Fake {
        pages: u32,
        seen: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }

    impl Renderer for Fake {
        fn page_count(&self, path: &Path) -> Result<u32, String> {
            self.seen.lock().unwrap().push(path.to_path_buf());
            Ok(self.pages)
        }
        fn page_text(&self, path: &Path, _page: u32) -> Result<String, String> {
            Ok(std::fs::read_to_string(path).unwrap_or_default())
        }
        fn page_png(&self, _path: &Path, page: u32, dpi: u32) -> Result<Vec<u8>, String> {
            Ok(format!("png-{page}-{dpi}").into_bytes())
        }
        fn markdown(&self, path: &Path, title: &str) -> Result<String, String> {
            self.seen.lock().unwrap().push(path.to_path_buf());
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            Ok(format!("# {title}\n{len}"))
        }
    }

    fn fake(pages: u32) -> Fake {
        Fake {
            pages,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The bytes reach the renderer as a FILE that is gone afterwards: a row keeps a document as
    /// bytes, and a temporary file that outlived the call would be one per turn of a session.
    #[test]
    fn the_document_is_a_temporary_file_that_does_not_outlive_the_call() {
        let f = fake(3);
        let bytes = crate::testing::pdf("hello");
        // `markdown` reads the file's own length, which is the renderer's proof that the whole
        // document was written rather than a header of it.
        let said = markdown_via(Some(&f), &bytes, "memo.pdf").expect("the renderer's answer");
        assert!(said.starts_with("# memo.pdf"), "{said}");
        assert!(
            said.ends_with(&format!("{}", bytes.len())),
            "the renderer saw fewer bytes than the document has: {said}"
        );
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "the file was written once");
        assert!(
            !seen[0].exists(),
            "a document's temporary file outlived the call: {:?}",
            seen[0]
        );
    }

    /// The page cap is the module's, not the renderer's: a forty-page document is ten pictures, and
    /// the count of pages comes back beside them so the caller can say what it left out.
    #[test]
    fn a_document_is_at_most_ten_pictures_and_its_page_count_comes_back() {
        let f = fake(40);
        let bytes = crate::testing::pdf("hello");
        let (pages, shots) = pages_via(Some(&f), &bytes, PAGE_DPI).expect("the renderer's answer");
        assert_eq!(pages, 40, "the count is the document's, not the cap's");
        assert_eq!(shots.len(), MAX_PAGES_AS_IMAGES as usize);
        assert_eq!(shots[0], format!("png-0-{PAGE_DPI}").into_bytes());
    }

    /// With no renderer, a document still yields its text through `pdftotext`; on a box without one
    /// the honest answer is nothing, and never a guess at a page count.
    #[test]
    fn without_a_renderer_the_text_still_comes_back_or_the_answer_is_none() {
        let bytes = crate::testing::pdf("the answer is 42");
        match markdown_via(None, &bytes, "memo.pdf") {
            Some(said) => {
                assert!(said.starts_with("# memo.pdf"), "{said}");
                assert!(said.contains("the answer is 42"), "{said}");
            }
            None => assert!(
                pages_via(None, &bytes, PAGE_DPI).is_none(),
                "pictures with no renderer to draw them"
            ),
        }
    }
}
