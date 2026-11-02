//! What a streaming SSE response costs the allocator, which only a counter can see.
//!
//! The framing tests next door pin the framing's behaviour, which was always correct. What this
//! file measures is how many times the allocator is called getting that behaviour.
//!
//! One measuring test per binary: cargo runs the tests within one file on parallel threads and the
//! counter is global, so two tests in one file count each other's allocations.
//!
//! Counted over a stream of frames rather than one, because the chunk buffer and the size line are
//! per-chunk costs and one frame per chunk is the real shape of an SSE response.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

mod alloc_stats {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static ALLOCS: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.realloc(p, l, new) }
        }
    }
}

#[global_allocator]
static COUNTING: alloc_stats::Counting = alloc_stats::Counting;

/// `n` as lowercase hex plus `\r\n`, written into `out`. Returns how many bytes were used.
///
/// Hand-written because `format!` allocates, and this file counts allocations.
fn hex_into(out: &mut [u8], mut n: usize) -> usize {
    let mut digits = [0u8; 16];
    let mut len = 0;
    loop {
        digits[len] = b"0123456789abcdef"[n & 0xf];
        len += 1;
        n >>= 4;
        if n == 0 {
            break;
        }
    }
    for i in 0..len {
        out[i] = digits[len - 1 - i];
    }
    out[len] = b'\r';
    out[len + 1] = b'\n';
    len + 2
}

fn chunked_server(chunks: Vec<Vec<u8>>) -> (letibot_http::Endpoint, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let Ok((mut conn, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                break;
            }
        }
        let _ = conn.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
              Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        );
        // Allocation-free, because this fake server runs in the test process and the counting
        // allocator counts it too: a `format!` here reads as an allocation per chunk in the client.
        // The size line is written by hand from a fixed buffer instead.
        let mut head = [0u8; 24];
        for c in &chunks {
            let n = hex_into(&mut head, c.len());
            let _ = conn.write_all(&head[..n]);
            let _ = conn.write_all(c);
            let _ = conn.write_all(b"\r\n");
        }
        let _ = conn.write_all(b"0\r\n\r\n");
        let _ = conn.flush();
    });
    (
        letibot_http::Endpoint::new("127.0.0.1", addr.port()),
        handle,
    )
}

// The allocation count, which is what this file exists for. Counted over a stream of frames
// because a single frame is too small a sample to tell a reused buffer from a fresh one.

/// Every `data:` payload of a chunked SSE body, counted and not kept.
///
/// Collecting the payloads would spend an allocation per frame in the TEST rather than in the
/// client, and it was a third of the measured number. What the payloads say is the other file's
/// business.
fn count_frames(endpoint: &letibot_http::Endpoint) -> usize {
    let body = letibot_http::get(endpoint, "/v1/chat/completions").expect("the request");
    let mut n = 0usize;
    body.for_each_frame(|frame| {
        n += frame.data.len();
        Ok(letibot_http::Flow::Continue)
    })
    .expect("the body parses");
    n
}

/// A streaming response does not allocate per chunk: the changed behaviour, counted.
///
/// A bound rather than an exact number, so a small change to the frame struct does not fail it.
#[test]
fn a_streaming_response_does_not_allocate_per_chunk() {
    use std::sync::atomic::Ordering;

    const FRAMES: usize = 400;
    let mut body = Vec::new();
    for i in 0..FRAMES {
        body.extend_from_slice(format!("data: {{\"n\":{i}}}\n\n").as_bytes());
    }
    // **One frame per chunk**, which is the shape a real SSE response has and the shape that
    // makes a per-chunk allocation cost 200 of them. Two lines per chunk, so a chunk is a frame.
    let chunks: Vec<Vec<u8>> = body
        .split_inclusive(|b| *b == b'\n')
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|c| c.concat())
        .collect();
    let frames_per_chunk = FRAMES as f64 / chunks.len() as f64;
    let (endpoint, server) = chunked_server(chunks);

    let before = alloc_stats::ALLOCS.load(Ordering::Relaxed);
    let got = count_frames(&endpoint);
    let allocs = alloc_stats::ALLOCS.load(Ordering::Relaxed) - before;
    server.join().unwrap();

    assert_eq!(got, FRAMES, "every frame arrived");
    let per_frame = allocs as f64 / FRAMES as f64;
    println!(
        "HTTP_ALLOCS frames={FRAMES} per_chunk={frames_per_chunk:.1} total={allocs} \
         per_frame={per_frame:.2}"
    );
    // What the 2 is, so the bound means something. The client's steady state per frame is exactly
    // two: the `data:` line's `String`, which the frame owns and which must outlive the borrow of
    // `pending`, and the `Vec<String>` holding it, built fresh because the frame moves into the
    // callback. The chunk buffer, the size line and the drained frame copy are all zero per chunk,
    // and this bound is tight enough to notice any of them coming back.
    assert!(
        per_frame < 2.5,
        "{per_frame:.2} allocations per frame over {FRAMES} frames (was 4.05 before the chunk \
         buffer and size line were reused) — the chunk buffer, the size line, or the drained \
         frame copy is being allocated again"
    );
}
