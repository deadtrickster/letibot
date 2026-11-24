//! A document in the conversation: the card's header, the page it draws once a render lands, the
//! text it falls back to, and the wheel or page key that turns it.
//!
//! The renderer is a fixture here - `App::pdf_reader` is the seam - so these tests measure the card
//! and the paging, not a rasterizer, and need no pdf tools on the box.

use super::*;

/// The placeholder a kitty terminal replaces with a slice of the page.
const PLACEHOLDER: char = '\u{10EEEE}';

/// A prompt carrying a document, as the daemon stores it: the chip in the operator's words and the
/// bytes beside them as a `Document` part.
fn a_prompt_with_a_pdf() -> (App, Vec<u8>) {
    let bytes = letibot_transcript::testing::pdf("the answer is 42");
    let media = letibot_transcript::media::Media::document("memo.pdf", &bytes).expect("a pdf");
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![
                    UserPart::Text {
                        text: "summarise this [file 1 · memo.pdf · 900 B]".into(),
                    },
                    UserPart::Document {
                        media_type: media.mime.clone(),
                        data_ref: media.data_ref.clone(),
                        filename: "memo.pdf".into(),
                    },
                ],
            }),
        },
    )));
    (a, bytes)
}

/// What the fixture renderer answers, and what it was asked: a page of `pages`, as a PNG the
/// terminal can hold, or as text when the head says it draws no pictures.
#[derive(Default)]
struct Renders {
    asked: std::sync::Mutex<Vec<(u32, bool)>>,
    pages: u32,
}

fn fixture(pages: u32) -> std::sync::Arc<Renders> {
    std::sync::Arc::new(Renders {
        asked: Default::default(),
        pages,
    })
}

fn reader(r: &std::sync::Arc<Renders>) -> crate::app::pdf::PdfReader {
    let r = r.clone();
    std::sync::Arc::new(move |req: &crate::app::pdf::PageRequest| {
        r.asked.lock().unwrap().push((req.page, req.picture));
        let mut answer = crate::app::pdf::Answer {
            pages: Some(r.pages),
            ..Default::default()
        };
        if req.picture {
            // A page as a picture: a real PNG header, which is all the card measures. A SHORT page,
            // so the card fits a 30-row screen and a test can read its header and its rows at once.
            answer.png = Some(page_png());
        } else {
            answer.text = Some(format!("page {} of the memo", req.page + 1));
        }
        answer
    })
}

/// The picture a page renders to in these tests: 612×240, which is ten rows in a half-width box.
fn page_png() -> Vec<u8> {
    letibot_transcript::testing::png(612, 240)
}

/// Collect the page the head asked for, the way the loop does: a frame, then ticks until the answer
/// lands. The frame is what records where the card's rows are, and a turn throws that away - so a
/// render is only ever asked for by the tick after a frame. The fixture is instant, so this is a
/// handful of ticks and not a wait.
fn settle(a: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let _ = a.screen(100, 30);
    a.clock(1);
    while a.pdf_busy() && std::time::Instant::now() < deadline {
        a.clock(1);
        std::thread::yield_now();
    }
    // The frame that collects an answer invalidates the history; one more tick starts the next
    // render if the card still wants one.
    a.clock(1);
}

/// A prompt's document is a card: its header names the file, the page and the size, and the page
/// itself is on its way.
#[test]
fn a_document_in_a_prompt_is_a_card_that_says_which_page_it_is_on() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader = reader(&fixture(3));
    a.set_proto(rano::term::Proto::Kitty);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    let header = card_header(&screen)
        .unwrap_or_else(|| panic!("the card's header is not on the screen:\n{joined}"));
    assert!(header.contains("memo.pdf"), "{header:?}");
    assert!(header.contains("· p 1/… ·"), "{header:?}");
    assert!(
        screen.iter().any(|l| l.contains("rendering p 1")),
        "the page is drawn as nothing rather than as a promise:\n{joined}"
    );
}

/// The card's header row as the frame drew it. Found by its GLYPH and not by the file's name, because
/// the operator's own chip line names the file too - and by no span crossing, because the row is
/// three spans and the escapes between them split any sentence a test might search for.
fn card_header(screen: &[String]) -> Option<&String> {
    screen
        .iter()
        .find(|l| l.contains(crate::ui::transcript::pdf::GLYPH))
}

/// Once the render lands the header states the page count, and the page is the terminal's: the rows
/// are placeholders and the bytes went out under the page's own id.
#[test]
fn the_page_arrives_as_a_picture_under_the_headers_page_count() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader = reader(&fixture(3));
    a.set_proto(rano::term::Proto::Kitty);
    let _ = a.screen(100, 30);
    settle(&mut a);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    let header = card_header(&screen).unwrap_or_else(|| panic!("no card:\n{joined}"));
    assert!(header.contains("p 1/3"), "{header:?}");
    assert!(
        screen.iter().any(|l| l.contains(PLACEHOLDER)),
        "no page was drawn:\n{joined}"
    );
    assert!(!joined.contains("rendering p 1"), "{joined}");
    // And the bytes went out, once, under the id the placeholders name.
    let uploads = a.take_image_uploads();
    let id = crate::app::pdf::page_id("s.0", 1, 0);
    assert_eq!(uploads.len(), 2, "an upload and a placement");
    assert_eq!(
        uploads[0],
        rano::term::graphics::image_upload(
            id,
            &letibot_transcript::media::encode_base64(&page_png())
        ),
        "the upload is the page's bytes under the card's id"
    );
    // A frame that changed nothing sends nothing: the terminal holds the page.
    let _ = a.screen(100, 30);
    assert!(a.take_image_uploads().is_empty(), "the page went twice");
}

/// A wheel notch over the card turns its page - the whole point of the card - and the header follows.
#[test]
fn a_wheel_notch_over_the_card_turns_its_page() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader = reader(&fixture(3));
    a.set_proto(rano::term::Proto::Kitty);
    let _ = a.screen(100, 30);
    settle(&mut a);
    let frame = a.screen(100, 30);
    let y = frame
        .iter()
        .position(|l| l.contains(crate::ui::transcript::pdf::GLYPH))
        .unwrap_or_else(|| panic!("the card's header:\n{}", frame.join("\n")));
    // Aimed at the card's own rows, which is where a wheel over a page lands.
    a.key(Key::Wheel {
        up: false,
        x: 10,
        y: (y + 1) as u16,
    });
    assert_eq!(
        a.pdf.card("s.0", 1).map(|c| c.page),
        Some(1),
        "the notch did not turn the page"
    );
    settle(&mut a);
    let after = a.screen(100, 30);
    let header = card_header(&after).expect("the card");
    assert!(header.contains("p 2/3"), "{header:?}");
}

/// A page key with no pointer turns it too, and at the last page the step goes back to the
/// conversation rather than dead-ending: that is how a reader scrolls past a document.
#[test]
fn the_last_page_hands_the_step_back_to_the_conversation() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader = reader(&fixture(2));
    a.set_proto(rano::term::Proto::Kitty);
    let _ = a.screen(100, 30);
    settle(&mut a);
    let _ = a.screen(100, 30);
    a.key(Key::PageDown);
    assert_eq!(a.pdf.card("s.0", 1).map(|c| c.page), Some(1), "page one");
    settle(&mut a);
    let _ = a.screen(100, 30);
    let before = a.held_line();
    a.key(Key::PageDown);
    assert_eq!(
        a.pdf.card("s.0", 1).map(|c| c.page),
        Some(1),
        "the last page turned past its end"
    );
    assert_ne!(
        a.held_line(),
        before,
        "the conversation did not take the step"
    );
    // And back up: the first page's top hands the step over as well.
    a.key(Key::PageUp);
    assert_eq!(a.pdf.card("s.0", 1).map(|c| c.page), Some(0), "page zero");
}

/// A terminal that draws no pictures gets the page's text, and the same paging: the fixture is asked
/// for text, and the rows under the header are the page's words.
#[test]
fn a_terminal_without_graphics_draws_the_pages_text() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    let renders = fixture(2);
    a.pdf_reader = reader(&renders);
    // No protocol: the head draws no picture, so it asks for the text.
    let _ = a.screen(100, 30);
    settle(&mut a);
    let joined = a.screen(100, 30).join("\n");
    assert!(joined.contains("page 1 of the memo"), "{joined}");
    assert!(!joined.contains(PLACEHOLDER), "{joined}");
    assert_eq!(
        renders.asked.lock().unwrap().as_slice(),
        &[(0, false)],
        "a head with no graphics asked for a picture"
    );
    // Paging works the same: the text of page two is asked for and drawn.
    a.key(Key::PageDown);
    settle(&mut a);
    let after = a.screen(100, 30);
    let header = card_header(&after).expect("the card");
    assert!(header.contains("p 2/2"), "{header:?}");
    let joined = after.join("\n");
    assert!(joined.contains("page 2 of the memo"), "{joined}");
}

/// A render the box cannot do is said, not waited on: the card states why, and the reader can still
/// page past the page that failed.
#[test]
fn a_page_that_cannot_be_rendered_says_so() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader =
        std::sync::Arc::new(
            |req: &crate::app::pdf::PageRequest| crate::app::pdf::Answer {
                pages: Some(2),
                failed: Some(format!(
                    "page {} could not be rasterized here",
                    req.page + 1
                )),
                ..Default::default()
            },
        );
    let _ = a.screen(100, 30);
    settle(&mut a);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    assert!(joined.contains("could not be rasterized here"), "{joined}");
    let header = card_header(&screen).expect("the card");
    assert!(header.contains("p 1/2"), "{header:?}");
}

/// A tool result that read a document draws the same card, named by what the call read.
#[test]
fn a_read_of_a_pdf_draws_the_card_too() {
    let bytes = letibot_transcript::testing::pdf("the answer is 42");
    let media = letibot_transcript::media::Media::document("memo.pdf", &bytes).expect("a pdf");
    let mut a = app();
    a.pdf_reader = reader(&fixture(4));
    a.set_proto(rano::term::Proto::Kitty);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: format!(
                    "/w/docs/memo.pdf — {}\nThe document is attached to this result.",
                    media.summary()
                ),
                edits: Vec::new(),
                origin: None,
                media: Some(media),
                ran_ms: None,
                decision: None,
            }),
        },
    )));
    // A tool row is the model's working: at the terse rung it is drawn, and at `conversation` it is
    // not drawn at all, so this test reads it where tools are shown.
    a.visibility = Visibility::lifted();
    let _ = a.screen(100, 30);
    settle(&mut a);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    let header = card_header(&screen).unwrap_or_else(|| panic!("no card:\n{joined}"));
    assert!(header.contains("p 1/4"), "{header:?}");
    assert!(
        screen.iter().any(|l| l.contains(PLACEHOLDER)),
        "the read's document drew no page:\n{joined}"
    );
}

/// A verbosity that hides the head's working hides the page and keeps the header: the card still
/// says which document and which page, and costs the conversation one row.
#[test]
fn a_hidden_verbosity_shows_the_header_alone() {
    let (mut a, _bytes) = a_prompt_with_a_pdf();
    a.pdf_reader = reader(&fixture(3));
    a.set_proto(rano::term::Proto::Kitty);
    let _ = a.screen(100, 30);
    settle(&mut a);
    a.visibility = Visibility::of(Profile::CONVERSATION);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    let header = card_header(&screen).unwrap_or_else(|| panic!("no card:\n{joined}"));
    assert!(header.contains("p 1/3"), "{header:?}");
    assert!(!joined.contains(PLACEHOLDER), "{joined}");
    assert!(!joined.contains("rendering p"), "{joined}");
    // The card is held, and its page with it: hiding the body hides the drawing, not the fact.
    assert!(
        a.pdf.card("s.0", 1).is_some(),
        "the card is held once it has been drawn"
    );
}

/// The row renderer's own answer for a prompt with a document, measured without a frame: the header,
/// the row that says the page is coming, and nothing else for a card that holds no page yet.
#[test]
fn a_prompt_row_draws_the_header_and_the_row_that_says_a_page_is_coming() {
    let bytes = letibot_transcript::testing::pdf("the answer is 42");
    let media = letibot_transcript::media::Media::document("memo.pdf", &bytes).expect("a pdf");
    let item = TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![
            UserPart::Text {
                text: "summarise this".into(),
            },
            UserPart::Document {
                media_type: media.mime.clone(),
                data_ref: media.data_ref.clone(),
                filename: "memo.pdf".into(),
            },
        ],
    };
    let rows = item_rows(false, item);
    assert_eq!(
        rows.iter()
            .filter(|r| r.contains(crate::ui::transcript::pdf::GLYPH))
            .count(),
        1,
        "one card for one document: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("· p 1/… ·")),
        "the header states the page and leaves the count open: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.trim() == "rendering p 1…"),
        "the card promises the page it is waiting for: {rows:?}"
    );
}
