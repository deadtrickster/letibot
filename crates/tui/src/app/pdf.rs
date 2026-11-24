//! A document in the conversation, as a card of pages: which page the reader is on, the pages
//! rendered so far, and the one render in flight.
//!
//! The renderer is rano's, behind `letibot_transcript::pdf`, and it is never called on the frame's
//! thread: a page costs up to ten seconds and the rasterizer's RSS, so a page is asked for by one
//! worker thread and collected by [`App::pump_pdf`] on the next tick. Until it arrives the card says
//! `rendering p N…`, and every later frame reads the cache.

use super::*;
use letibot_transcript::{TranscriptItem, UserPart};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// What names a card: the row that carries the document, and which document of it. A tool result
/// carries one (`0`); a prompt may carry several.
pub(crate) type PdfKey = (String, usize);

/// How many rendered pages one card keeps. A reader pages forward and back, so a couple either way;
/// the bound is what a 20 MB document is allowed to cost the head's memory.
pub(crate) const KEEP_PAGES: usize = 4;

/// The most rows of a page's TEXT the card draws. `pdftotext` on one page is a page's worth of lines;
/// this is the cap that keeps an extractor having a bad day from becoming a wall of rows.
pub(crate) const TEXT_ROWS: usize = 80;

/// The dpi a page is rendered at for a card `cols` cells wide. A printed page is about eight and a
/// half inches across and a cell about twelve pixels, so three dpi a column keeps the picture sharp
/// when the terminal scales it into the box - and under the renderer's own bounds.
pub(crate) fn dpi_for(cols: u32) -> u32 {
    (cols.saturating_mul(3)).clamp(96, 200)
}

/// One rendered page: the pixels it was rendered from, which is the aspect the card's box is cut to,
/// and kept with the page so two frames cannot disagree about it.
#[derive(Debug, Clone)]
pub(crate) struct Shot {
    pub(crate) page: u32,
    pub(crate) px: (u32, u32),
}

/// One document of one row, and what this head holds of it.
#[derive(Debug, Default)]
pub(crate) struct PdfCard {
    /// The document's bytes, decoded once: a row keeps a `data:` reference and the renderer takes a
    /// file, so the decode is done off the frame and kept while the row is.
    pub(crate) bytes: Option<Arc<Vec<u8>>>,
    /// How many pages it has, once a render said so - `None` until then, which the header spells `…`.
    pub(crate) pages: Option<u32>,
    /// The page the reader is on, zero-based.
    pub(crate) page: u32,
    /// The pages rendered so far, oldest first, at most [`KEEP_PAGES`].
    pub(crate) shots: VecDeque<Shot>,
    /// The page's text and which page it is of: the fallback a terminal with no graphics gets, and
    /// what a renderer that cannot rasterize leaves.
    pub(crate) text: Option<(u32, String)>,
    /// Why a page could not be rendered at all, with the page it was.
    pub(crate) failed: Option<(u32, String)>,
    /// The page a render is in flight for.
    pub(crate) busy: Option<u32>,
}

impl PdfCard {
    /// The rendered page, when it is one of the kept ones.
    pub(crate) fn shot(&self, page: u32) -> Option<&Shot> {
        self.shots.iter().find(|s| s.page == page)
    }

    /// The page's text, when the text in hand is that page's.
    pub(crate) fn text_of(&self, page: u32) -> Option<&str> {
        match &self.text {
            Some((of, text)) if *of == page => Some(text),
            _ => None,
        }
    }

    /// The reason that page could not be rendered, when it is that page's.
    pub(crate) fn failure(&self, page: u32) -> Option<&str> {
        match &self.failed {
            Some((of, why)) if *of == page => Some(why),
            _ => None,
        }
    }
}

/// Where a card's rows are, as the last draw of its row put them: the lines of that row before the
/// card, how many the card took, and which page it drew. Relative to the row's first line, because
/// the draw knows what it has written and where the row starts is the walk's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Where {
    pub(crate) at: usize,
    pub(crate) lines: usize,
    pub(crate) page: u32,
}

/// Every document card this head holds, and where the last frame drew each one. The drawing side
/// reads `cards` and writes `drawn` through a `&`, which is why one of them is a `RefCell` - the
/// same bargain `ItemCtx::payload_max` makes with a `Cell`.
#[derive(Debug, Default)]
pub(crate) struct PdfCards {
    pub(crate) cards: BTreeMap<PdfKey, PdfCard>,
    pub(crate) drawn: RefCell<BTreeMap<PdfKey, Where>>,
}

impl PdfCards {
    /// The card, when this head holds one for the row and document.
    pub(crate) fn card(&self, item_id: &str, part: usize) -> Option<&PdfCard> {
        self.cards.get(&(item_id.to_string(), part))
    }

    /// The card, made if this row's document has none yet.
    pub(crate) fn entry(&mut self, item_id: &str, part: usize) -> &mut PdfCard {
        self.cards.entry((item_id.to_string(), part)).or_default()
    }

    /// Record where a draw put the card. Called by the drawing side, which is handed a `&`.
    pub(crate) fn place(&self, item_id: &str, part: usize, at: Where) {
        if let Ok(mut drawn) = self.drawn.try_borrow_mut() {
            drawn.insert((item_id.to_string(), part), at);
        }
    }

    /// Where the last draw put a card, absolute, using the span of the row it belongs to.
    pub(crate) fn absolute(&self, span_at: usize, w: Where) -> std::ops::Range<usize> {
        span_at + w.at..span_at + w.at + w.lines
    }

    /// Forget the cards whose rows left the transcript - the rule `prune_to_the_rows` keeps for every
    /// map keyed by a row's id.
    pub(crate) fn retain_rows(&mut self, held: &std::collections::HashSet<String>) {
        self.cards.retain(|(id, _), _| held.contains(id));
        if let Ok(mut drawn) = self.drawn.try_borrow_mut() {
            drawn.retain(|(id, _), _| held.contains(id));
        }
    }
}

/// What one render is asked for: the document's bytes, the page, the dpi - and the two facts that
/// save a second pass over a 20 MB file.
#[derive(Debug)]
pub(crate) struct PageRequest {
    pub(crate) bytes: Arc<Vec<u8>>,
    /// Zero-based.
    pub(crate) page: u32,
    pub(crate) dpi: u32,
    /// The page count, when a previous render answered it: a count is asked for once a document.
    pub(crate) pages: Option<u32>,
    /// The terminal draws pictures. `false` asks for the page's text alone, which is all a head
    /// without graphics can draw and cheaper than a raster.
    pub(crate) picture: bool,
}

/// What one render answers.
#[derive(Debug, Default, Clone)]
pub(crate) struct Answer {
    pub(crate) pages: Option<u32>,
    pub(crate) png: Option<Vec<u8>>,
    pub(crate) text: Option<String>,
    pub(crate) failed: Option<String>,
}

/// What takes a render: `letibot_transcript::pdf` - rano's rasterizer behind it - in production, and
/// a fixture in a test. Run on a thread of its own, so it must be `Send + Sync`.
pub(crate) type PdfReader = Arc<dyn Fn(&PageRequest) -> Answer + Send + Sync>;

/// A page ready for the terminal: the base64 an upload carries, and the pixels its box is cut from.
type PageUpload = (Arc<str>, (Option<u32>, Option<u32>));

/// The production reader: the page's picture where the terminal draws one, and its text where it does
/// not or where the rasterizer refused.
pub(crate) fn read_page(r: &PageRequest) -> Answer {
    use letibot_transcript::pdf;
    let pages = r.pages.or_else(|| pdf::page_count(&r.bytes));
    let mut answer = Answer {
        pages,
        ..Answer::default()
    };
    if r.picture {
        answer.png = pdf::page_png(&r.bytes, r.page, r.dpi);
    }
    if answer.png.is_none() {
        answer.text = pdf::page_text(&r.bytes, r.page);
    }
    if answer.png.is_none() && answer.text.is_none() {
        answer.failed = Some(match pages {
            Some(n) if r.page >= n => {
                format!("page {} does not exist: the document has {n}", r.page + 1)
            }
            _ => "no renderer here can draw this page".to_string(),
        });
    }
    answer
}

/// A render in flight: which card's page, and the channel its answer arrives on.
pub(crate) struct Job {
    pub(crate) key: PdfKey,
    pub(crate) page: u32,
    pub(crate) rx: std::sync::mpsc::Receiver<Answer>,
}

/// The reader's place on one document's card: the page, and how many of its rows sit above the top of
/// the viewport - which is zero when the whole card is on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) page: u32,
    pub(crate) offset: usize,
}

/// What a scroll step over a card does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The card takes it: show this page, at this offset into it.
    To(Place),
    /// The card has nothing left in that direction, and the conversation takes the step.
    Conversation,
}

/// The one rule a page turns by: from `place`, a step of `by` rows either way, on a document of
/// `pages`, whose current page takes `rows` and whose viewport shows `room`.
///
/// A page taller than the viewport is scrolled first and turned at its last row; a page that fits is
/// whole on the screen already, so the step turns it. At the first page's top and the last page's
/// bottom the document has nothing left, and the step is the conversation's.
pub(crate) fn step(
    place: Place,
    pages: u32,
    rows: usize,
    room: usize,
    down: bool,
    by: usize,
) -> Step {
    let pages = pages.max(1);
    // How far into the page the viewport can scroll before the page's last row is the last one
    // showing. Zero for a page that fits.
    let bottom = rows.saturating_sub(room);
    if down {
        if place.offset < bottom {
            return Step::To(Place {
                page: place.page,
                offset: (place.offset + by).min(bottom),
            });
        }
        if place.page + 1 < pages {
            return Step::To(Place {
                page: place.page + 1,
                offset: 0,
            });
        }
        return Step::Conversation;
    }
    if place.offset > 0 {
        return Step::To(Place {
            page: place.page,
            offset: place.offset.saturating_sub(by),
        });
    }
    if place.page > 0 {
        // Paging up lands at the previous page's BOTTOM: the reader is reading upwards, and the row
        // they came from is the one below the one they now want.
        return Step::To(Place {
            page: place.page - 1,
            offset: bottom,
        });
    }
    Step::Conversation
}

/// A page's image id: the row's and the document's own, with the page - so two pages of one document
/// are two pictures the terminal holds at once, and a later page never overwrites an earlier one.
pub(crate) fn page_id(item_id: &str, part: usize, page: u32) -> u32 {
    rano::term::graphics::image_id(&format!("{item_id}#pdf{part}p{page}"))
}

impl App {
    /// The document's bytes, decoded from the row's `data:` reference the first time a render needs
    /// them. `None` for a row that has lost its document, which a turn then draws as text.
    fn pdf_bytes(&mut self, key: &PdfKey) -> Option<Arc<Vec<u8>>> {
        if let Some(held) = self.pdf.cards.get(key).and_then(|c| c.bytes.clone()) {
            return Some(held);
        }
        let reference = self.document_ref(&key.0, key.1)?;
        let payload = letibot_transcript::media::wire_payload(&reference);
        let bytes = Arc::new(letibot_transcript::media::decode_base64(payload));
        self.pdf.cards.get_mut(key)?.bytes = Some(bytes.clone());
        Some(bytes)
    }

    /// The `data:` reference of one document of one row: a prompt's part, or a tool result's media.
    pub(crate) fn document_ref(&self, item_id: &str, part: usize) -> Option<String> {
        let item = self
            .items
            .iter()
            .find(|i| i.item_id == item_id)?
            .item
            .as_ref()?;
        match item {
            TranscriptItem::User { parts, .. } => match parts.get(part)? {
                UserPart::Document { data_ref, .. } => Some(data_ref.clone()),
                _ => None,
            },
            TranscriptItem::ToolResult { media: Some(m), .. } if part == 0 && m.is_document() => {
                Some(m.data_ref.clone())
            }
            _ => None,
        }
    }

    /// Is any page render in flight? The loop must keep ticking while one is - the placeholder under
    /// the header is replaced by the frame that collects the answer, and nothing else wakes it.
    pub(crate) fn pdf_busy(&self) -> bool {
        self.pdf_job.is_some()
    }

    /// Collect a finished render, then start the next one for a card on the screen that has none of
    /// its page. Called from the clock: a rasterizer is never asked for a page on a frame.
    pub(crate) fn pump_pdf(&mut self) {
        if let Some(job) = self.pdf_job.take() {
            match job.rx.try_recv() {
                Ok(answer) => self.pdf_landed(job.key, job.page, answer),
                // Still rendering. One page at a time: a second thread would be a second rasterizer
                // against the same caps, and the cache would still be read by the same frame.
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.pdf_job = Some(job);
                    return;
                }
                // The worker died without an answer. The card says so rather than waiting for ever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(card) = self.pdf.cards.get_mut(&job.key) {
                        card.busy = None;
                        card.failed = Some((job.page, "the renderer stopped".to_string()));
                    }
                    self.invalidate_history();
                }
            }
        }
        self.start_pdf_render();
    }

    /// The card a page belongs to, once its answer is in: the picture for the terminal to draw, or the
    /// page's text, or the reason there is neither.
    fn pdf_landed(&mut self, key: PdfKey, page: u32, answer: Answer) {
        // What the terminal has to be told about, taken out of the borrow before it is uploaded: the
        // card is borrowed from `self` while it is being filled in.
        let mut picture: Option<PageUpload> = None;
        {
            let Some(card) = self.pdf.cards.get_mut(&key) else {
                return;
            };
            card.busy = None;
            if let Some(n) = answer.pages {
                card.pages = Some(n.max(1));
            }
            match answer.png {
                Some(png) => {
                    let b64: Arc<str> = Arc::from(letibot_transcript::media::encode_base64(&png));
                    let (w, h) = crate::ui::render::picture_size(&png);
                    card.shots.push_back(Shot {
                        page,
                        px: (w.unwrap_or(0), h.unwrap_or(0)),
                    });
                    while card.shots.len() > KEEP_PAGES {
                        card.shots.pop_front();
                    }
                    card.failed = None;
                    picture = Some((b64, (w, h)));
                }
                None => match answer.text {
                    Some(text) => {
                        card.text = Some((page, text));
                        card.failed = None;
                    }
                    // Neither: the card draws the sentence instead of a page, and the reader can still
                    // page past it - a document with one unreadable page is not one with none.
                    None => {
                        card.failed = Some((
                            page,
                            answer
                                .failed
                                .unwrap_or_else(|| "this page could not be rendered".to_string()),
                        ));
                    }
                },
            }
        }
        if let Some((b64, px)) = picture {
            let (item_id, part) = (key.0.clone(), key.1);
            self.upload_page(&item_id, part, page, &b64, px);
        }
        self.invalidate_history();
    }

    /// Send one rendered page to the terminal, once: the same two commands every other picture in the
    /// conversation goes out with, under the page's own id.
    fn upload_page(
        &mut self,
        item_id: &str,
        part: usize,
        page: u32,
        b64: &str,
        px: (Option<u32>, Option<u32>),
    ) {
        let id = page_id(item_id, part, page);
        if self.images_sent.contains_key(&id) {
            return;
        }
        let box_cols = rano::term::graphics::image_box(self.cfg.width);
        let (cols, rows) = rano::term::graphics::image_cells(px.0, px.1, box_cols);
        self.images_sent.insert(id, px);
        let upload = rano::term::graphics::image_upload(id, b64);
        self.image_uploads.push(self.proto.wrap(&upload));
        let place = rano::term::graphics::image_place(id, cols, rows);
        self.image_uploads.push(self.proto.wrap(&place));
    }

    /// Start a render for a card the reader can actually see whose page this head does not hold. Only
    /// the visible ones: a transcript may hold twenty documents, and a page a reader never turns to is
    /// a rasterizer run nobody asked for.
    fn start_pdf_render(&mut self) {
        if self.pdf_job.is_some() {
            return;
        }
        let Some((key, page, dpi)) = self.visible_pdf_want() else {
            return;
        };
        let Some(bytes) = self.pdf_bytes(&key) else {
            return;
        };
        // Read before the card is borrowed: a page is asked for as a picture only where the terminal
        // draws one, and anywhere else its text is the whole of what a card can show.
        let (picture, reader) = (self.cfg.images, self.pdf_reader.clone());
        let Some(card) = self.pdf.cards.get_mut(&key) else {
            return;
        };
        let request = PageRequest {
            bytes,
            page,
            dpi,
            pages: card.pages,
            picture,
        };
        card.busy = Some(page);
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pdf-page".into())
            .spawn(move || {
                let _ = tx.send(reader(&request));
            });
        match spawned {
            Ok(_) => self.pdf_job = Some(Job { key, page, rx }),
            Err(_) => {
                // No thread, no page: said, not waited for.
                if let Some(card) = self.pdf.cards.get_mut(&key) {
                    card.busy = None;
                    card.failed =
                        Some((page, "no thread could be started to render it".to_string()));
                }
                self.invalidate_history();
            }
        }
    }

    /// The card the reader can see that needs its page rendered, with the dpi its width asks for. A row
    /// the walk drew a document for is a card from here on: this is where one is made, because the
    /// drawing side has no `&mut` and a card with no page would otherwise never exist at all.
    fn visible_pdf_want(&mut self) -> Option<(PdfKey, u32, u32)> {
        let top = self.view_top;
        let room = self.view_room.max(1);
        // The card the reader is on wins over one merely at the edge of the window.
        let held = self.held_line();
        let mut on_screen: Vec<(PdfKey, u32, u32, bool)> = Vec::new();
        {
            let drawn = self.pdf.drawn.borrow();
            for ((item_id, part), w) in drawn.iter() {
                // The page the CARD is on, not the one the last frame drew: a turn moves the card and
                // the frame after it is the one that draws the new page.
                let card = self.pdf.cards.get(&(item_id.clone(), *part));
                let page = card.map(|c| c.page).unwrap_or(w.page);
                // Nothing held for this page is a want: `None` here is the card that has just been
                // drawn for the first time.
                if let Some(card) = card {
                    if card.shot(page).is_some() && self.cfg.images {
                        continue;
                    }
                    if card.text_of(page).is_some() || card.failure(page).is_some() {
                        continue;
                    }
                    if card.busy == Some(page) {
                        continue;
                    }
                }
                let Some(span) = self.span_for(item_id) else {
                    continue;
                };
                let rows = self.pdf.absolute(span.at, *w);
                let visible = rows.start < top + room && rows.end > top;
                if !visible {
                    continue;
                }
                let here = held >= rows.start && held < rows.end;
                let cols = (self.cfg.width.max(20) / 2).clamp(20, 80) as u32;
                on_screen.push(((item_id.clone(), *part), page, dpi_for(cols), here));
            }
        }
        let chosen = on_screen
            .iter()
            .find(|(_, _, _, here)| *here)
            .or_else(|| on_screen.first())
            .map(|(key, page, dpi, _)| (key.clone(), *page, *dpi));
        if let Some((key, _, _)) = &chosen {
            self.pdf.entry(&key.0, key.1);
        }
        chosen
    }

    /// A scroll step that lands on a document card: it turns the page when the page has nothing left
    /// in that direction, and hands the step back to the conversation at either end of the document.
    /// `true` when the card took it.
    ///
    /// `at` is the line the step was aimed at - the pointer's row for a wheel notch - and the reader's
    /// own line when a key sent it.
    pub(crate) fn turn_pdf_page(&mut self, up: bool, by: usize, at: Option<usize>) -> bool {
        let Some((key, space, at_row, page)) = self.card_for_step(up, at) else {
            return false;
        };
        let pages = self
            .pdf
            .cards
            .get(&key)
            .and_then(|c| c.pages)
            .unwrap_or(1)
            .max(1);
        // The rows of the page above the top of the viewport: what the page has scrolled by. A card
        // whole on the screen has scrolled none of it, which is the case a notch turns at once.
        let above = self.held_line().saturating_sub(space.start);
        let place = Place {
            page,
            offset: above,
        };
        let rows = space.len().max(1);
        match step(place, pages, rows, self.view_room.max(1), !up, by) {
            // A step inside a page the viewport has not scrolled to its end: the conversation's own
            // scroll IS how a page taller than the screen is read, so it is handed back verbatim.
            Step::To(next) if next.page == page => false,
            Step::To(next) => {
                let Some(card) = self.pdf.cards.get_mut(&key) else {
                    return false;
                };
                card.page = next.page;
                // The page it turned TO is drawn from the place the step named: the whole of it from
                // its first row going down, and its last rows going up.
                let Some(ordinal) = self.items.iter().position(|it| it.item_id == key.0) else {
                    return true;
                };
                self.fill_to_row(ordinal);
                self.anchor = Some(Held {
                    item_id: key.0.clone(),
                    ordinal,
                    into: at_row.saturating_add(next.offset),
                });
                self.invalidate_history();
                true
            }
            Step::Conversation => false,
        }
    }

    /// The card a step is about: the one whose rows the step was aimed at, or - for a key, which aims
    /// at no row - the one at the window's edge in the direction of the step, and failing that one the
    /// window holds whole. A card entirely on the screen is the thing the reader is looking at, so a
    /// step is about it; a card half scrolled reads as the page it is.
    ///
    /// Its key, the rows the last draw placed it at, the card's own offset inside its row, and the page
    /// that draw made.
    fn card_for_step(
        &self,
        up: bool,
        pointed: Option<usize>,
    ) -> Option<(PdfKey, std::ops::Range<usize>, usize, u32)> {
        let drawn = self.pdf.drawn.borrow();
        let placed: Vec<(PdfKey, std::ops::Range<usize>, usize, u32)> = drawn
            .iter()
            .filter_map(|((item_id, part), w)| {
                let span = self.span_for(item_id)?;
                Some((
                    (item_id.clone(), *part),
                    self.pdf.absolute(span.at, *w),
                    w.at,
                    w.page,
                ))
            })
            .collect();
        if let Some(line) = pointed {
            return placed
                .into_iter()
                .find(|(_, rows, _, _)| rows.contains(&line));
        }
        let top = self.view_top;
        let room = self.view_room.max(1);
        let edge = if up { top } else { top + room - 1 };
        if let Some(hit) = placed.iter().find(|(_, rows, _, _)| rows.contains(&edge)) {
            return Some(hit.clone());
        }
        placed
            .into_iter()
            .find(|(_, rows, _, _)| rows.start >= top && rows.end <= top + room)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first page's top: the document has nothing above it, so the step is the conversation's -
    /// this is the hand-off a reader relies on to reach the rows above the card.
    #[test]
    fn a_step_up_from_the_first_page_is_the_conversations() {
        assert_eq!(
            step(Place { page: 0, offset: 0 }, 3, 20, 30, false, 3),
            Step::Conversation
        );
    }

    /// And the last page's bottom is the other end: past it the conversation scrolls again, so a card
    /// in the middle of a transcript can be read through and left.
    #[test]
    fn a_step_down_from_the_last_pages_bottom_is_the_conversations() {
        // A page taller than the viewport, scrolled to its last row.
        let bottom = 40 - 30;
        assert_eq!(
            step(
                Place {
                    page: 2,
                    offset: bottom
                },
                3,
                40,
                30,
                true,
                3
            ),
            Step::Conversation
        );
    }

    /// Inside one page the step is the page's own scroll, and it stops at the page's last row rather
    /// than running past it into the next.
    #[test]
    fn a_step_inside_a_tall_page_scrolls_it_and_stops_at_its_last_row() {
        let to = |offset| step(Place { page: 0, offset }, 2, 40, 30, true, 3);
        assert_eq!(to(0), Step::To(Place { page: 0, offset: 3 }));
        assert_eq!(
            to(8),
            Step::To(Place {
                page: 0,
                offset: 10
            }),
            "the last row of a 40-row page in a 30-row window is offset 10"
        );
        assert_eq!(
            to(10),
            Step::To(Place { page: 1, offset: 0 }),
            "from the bottom the next step turns the page"
        );
    }

    /// A page that fits the screen has scrolled nothing and has nothing to scroll: the first step
    /// down turns it, which is the whole of "a card of pages that scrolls page by page".
    #[test]
    fn a_page_that_fits_the_screen_turns_on_the_first_step() {
        let fits = Place { page: 0, offset: 0 };
        assert_eq!(
            step(fits, 4, 20, 30, true, 3),
            Step::To(Place { page: 1, offset: 0 })
        );
        assert_eq!(
            step(Place { page: 2, offset: 0 }, 4, 20, 30, false, 3),
            Step::To(Place { page: 1, offset: 0 }),
            "and back, to the previous page's first row, because it fits whole"
        );
    }

    /// Paging UP into a page taller than the screen lands at its bottom: the reader is reading
    /// upwards, so the rows they want are the ones under the row they came from.
    #[test]
    fn paging_up_lands_at_the_previous_pages_bottom() {
        assert_eq!(
            step(Place { page: 1, offset: 0 }, 3, 40, 30, false, 3),
            Step::To(Place {
                page: 0,
                offset: 10
            })
        );
        // And a step up inside the page is the page's own scroll.
        assert_eq!(
            step(Place { page: 1, offset: 6 }, 3, 40, 30, false, 3),
            Step::To(Place { page: 1, offset: 3 })
        );
    }

    /// One page is a document that turns in neither direction: both ends are the conversation's.
    #[test]
    fn a_one_page_document_hands_both_directions_to_the_conversation() {
        let only = Place { page: 0, offset: 0 };
        assert_eq!(step(only, 1, 10, 30, true, 3), Step::Conversation);
        assert_eq!(step(only, 1, 10, 30, false, 3), Step::Conversation);
    }

    /// The id two pages of one document get is two ids, and a page's id is stable across frames - the
    /// terminal holds the bytes under it.
    #[test]
    fn every_page_of_a_document_has_its_own_image_id() {
        let a = page_id("s.0", 0, 0);
        let b = page_id("s.0", 0, 1);
        assert_ne!(a, b, "two pages shared one image id");
        assert_eq!(a, page_id("s.0", 0, 0));
        assert_ne!(
            a,
            page_id("s.0", 1, 0),
            "two documents of one row shared an id"
        );
    }

    /// The dpi follows the width the card gets, inside the renderer's own bounds.
    #[test]
    fn the_dpi_follows_the_cards_width() {
        assert_eq!(dpi_for(20), 96, "a narrow card is not rendered below 96");
        assert_eq!(dpi_for(50), 150);
        assert_eq!(dpi_for(120), 200, "and not above what the renderer allows");
    }

    /// The cache is bounded: a reader can turn pages all day and the head keeps the last few.
    #[test]
    fn the_page_cache_keeps_a_bounded_number_of_pages() {
        let mut card = PdfCard::default();
        for page in 0..10u32 {
            card.shots.push_back(Shot { page, px: (2, 2) });
            while card.shots.len() > KEEP_PAGES {
                card.shots.pop_front();
            }
        }
        assert_eq!(card.shots.len(), KEEP_PAGES);
        assert_eq!(card.shots.front().map(|s| s.page), Some(6));
        assert_eq!(card.shots.back().map(|s| s.page), Some(9));
        assert!(card.shot(9).is_some());
        assert!(card.shot(0).is_none(), "an evicted page is gone");
    }
}
