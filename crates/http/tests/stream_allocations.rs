//! **What a streaming SSE response costs the allocator** — and why this is its own file.
//!
//! The framing tests next door (`chunked_body.rs`) pass on the old code as well, and that is the
//! honest result: they pin the framing's BEHAVIOUR, which was correct and had no coverage at all.
//! What changed in `Body` is how many times the allocator is called getting that behaviour, and
//! only a counter can see it.
//!
//! **One measuring test per binary.** Cargo runs the tests within one file on parallel threads,
//! and the counter is global, so two tests in one file count each other's allocations — measured
//! at 5.72 per frame here when the real figure is under 2, with the number wandering between
//! runs. The same mistake cost the frame-allocation test in `letibot-tui` an afternoon.
//!
//! Counted over a stream of frames rather than one: the chunk buffer and the size line are both
//! per-CHUNK costs, and one frame per chunk is the real shape of an SSE response.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

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
/// A hand-written encoder because `format!` allocates — see the call site for why that matters
/// here in a way it does not in the other test files.
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
        // **Allocation-free, and that is not tidiness — it is the measurement.** This fake server
        // runs IN THE TEST PROCESS, so the counting allocator counts it too: `format!("{:x}\r\n",
        // …)` here is one `String` per chunk, and it was read as one allocation per chunk in the
        // CLIENT. It cost an afternoon of bisecting `next_bytes` for a cost that was never there.
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
    (letibot_http::Endpoint::new("127.0.0.1", addr.port()), handle)
}

// ---------------------------------------------------------------------------------------------
// **And the allocation count**, which is what the change to this file was for.
//
// The six tests above pass on the old code as well, and that is the honest result: they pin the
// framing's BEHAVIOUR, which was correct and untested. What changed here is how many times the
// allocator is called getting that behaviour, and only a counter can see it.
//
// Counted over a stream of frames, because a single frame is too small a sample to tell a reused
// buffer from a fresh one: the chunk buffer and the size line are both per-CHUNK costs, and one
// frame per chunk is the real shape of an SSE response.

/// Every `data:` payload of a chunked SSE body, through the real client.
/// Count the frames, WITHOUT keeping their payloads.
///
/// The first version of this collected `d.clone()` for each payload, which is an allocation per
/// frame spent by the TEST rather than by the client — and it was a third of the measured number.
/// Counting is all this needs; what the payloads SAY is the other file's business.
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

/// **A streaming response does not allocate per chunk** — the changed behaviour, counted.
///
/// The bound is asserted rather than the exact number, so a small change to the frame struct does
/// not fail it. Before the chunk buffer and the size line were reused this was three allocations
/// per chunk — the size `String`, the chunk `Vec`, and the drained frame copy — plus one per
/// `data:` line, which is the only one that has to stay.
#[test]
fn a_streaming_response_does_not_allocate_per_chunk() {
    use std::sync::atomic::Ordering;

    const FRAMES: usize = 400;
    let mut body = Vec::new();
    for i in 0..FRAMES {
        body.extend_from_slice(format!("data: {{\"n\":{i}}}\n\n").as_bytes());
    }
    // **One frame per chunk**, which is the shape a real SSE response has and the shape that makes
    // a per-chunk allocation cost 200 of them. Two lines per chunk, so the chunk is not a frame.
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
    // **What the 2 is, so the bound means something.** The client's steady state per frame is
    // exactly two:
    //
    //   1. the `data:` line's `String` — the frame owns it, and it has to outlive the borrow of
    //      `pending` that it was parsed from, so this one is inherent; and
    //   2. the `Vec<String>` that holds it, built fresh per frame because the frame is moved into
    //      the callback (`SmallVec` would remove it; a dependency would too).
    //
    // Everything else — the chunk buffer, the size line, the drained frame copy — is now zero per
    // chunk, and this bound is tight enough to notice any of them coming back: one per chunk is
    // one per frame at the shape below, so a regression lands at 3.06.
    assert!(
        per_frame < 2.5,
        "{per_frame:.2} allocations per frame over {FRAMES} frames (was 4.05 before the chunk \
         buffer and size line were reused) — the chunk buffer, the size line, or the drained \
         frame copy is being allocated again"
    );
}
