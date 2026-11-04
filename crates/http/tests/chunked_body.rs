//! The chunked-transfer framing against a server that actually chunks.
//! Real llama.cpp endpoints use it and it carries the SSE frames. Cases: a frame split across two
//! chunks, two frames in one chunk, and a frame boundary on a chunk boundary.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

/// A one-shot HTTP server that answers with the given chunked body, in the given
/// chunks. The endpoint comes back as `Endpoint`, not a URL, because `Endpoint::parse`
/// takes `HOST:PORT` and an `http://` string is a hostname it will try to resolve.
fn chunked_server(chunks: Vec<Vec<u8>>) -> (letibot_http::Endpoint, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let Ok((conn, _)) = listener.accept() else {
            return;
        };
        serve(conn, chunks);
    });
    (
        letibot_http::Endpoint::new("127.0.0.1", addr.port()),
        handle,
    )
}

fn serve(mut conn: TcpStream, chunks: Vec<Vec<u8>>) {
    // Read the request head, so the client is not writing into a closed socket.
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim_end().is_empty() {
            break;
        }
    }
    let _ = conn.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                              Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    );
    for (i, c) in chunks.iter().enumerate() {
        // Every other chunk carries an extension after the size, because a real server sends them
        // (`;` parameters are legal and the parser must ignore them rather than fail the parse).
        let head = if i % 2 == 0 {
            format!("{:x}\r\n", c.len())
        } else {
            format!("{:x};ext=1\r\n", c.len())
        };
        let _ = conn.write_all(head.as_bytes());
        let _ = conn.write_all(c);
        let _ = conn.write_all(b"\r\n");
    }
    // The terminating zero-length chunk.
    let _ = conn.write_all(b"0\r\n\r\n");
    let _ = conn.flush();
}

/// Every `data:` payload of an SSE body, in order, through the real client.
fn payloads(endpoint: &letibot_http::Endpoint) -> Vec<String> {
    let body = letibot_http::get(endpoint, "/v1/chat/completions").expect("the request");
    let mut out = Vec::new();
    body.for_each_frame(|frame| {
        for d in &frame.data {
            out.push(d.clone());
        }
        Ok(letibot_http::Flow::Continue)
    })
    .expect("the body parses");
    out
}

const ONE: &str = "data: {\"n\":1}\n\n";
const TWO: &str = "data: {\"n\":2}\n\n";
const THREE: &str = "data: {\"n\":3}\n\n";

#[test]
fn a_frame_split_across_two_chunks_is_one_frame() {
    // The first frame's bytes end mid-JSON, so the reader must hold the partial line.
    let (all, head) = (format!("{ONE}{TWO}"), 12);
    let (endpoint, server) = chunked_server(vec![
        all.as_bytes()[..head].to_vec(),
        all.as_bytes()[head..].to_vec(),
    ]);
    assert_eq!(payloads(&endpoint), vec!["{\"n\":1}", "{\"n\":2}"]);
    server.join().unwrap();
}

#[test]
fn two_frames_inside_one_chunk_are_two_frames() {
    let (endpoint, server) = chunked_server(vec![
        format!("{ONE}{TWO}").into_bytes(),
        THREE.as_bytes().to_vec(),
    ]);
    assert_eq!(
        payloads(&endpoint),
        vec!["{\"n\":1}", "{\"n\":2}", "{\"n\":3}"]
    );
    server.join().unwrap();
}

#[test]
fn a_frame_boundary_exactly_on_a_chunk_boundary_is_two_frames() {
    // The `\n\n` is the last thing in the first chunk, which is the off-by-two case: a reader
    // that looked for the boundary only within the newest chunk would find nothing here and then
    // find it again one chunk later, having already consumed the delimiter.
    let (endpoint, server) = chunked_server(vec![ONE.as_bytes().to_vec(), TWO.as_bytes().to_vec()]);
    assert_eq!(payloads(&endpoint), vec!["{\"n\":1}", "{\"n\":2}"]);
    server.join().unwrap();
}

#[test]
fn many_small_chunks_reassemble_into_the_same_frames() {
    // One byte per chunk: the reader's chunk buffer is resized on every read and its capacity has
    // to survive that, and the frame assembler sees the body arrive a byte at a time.
    let all = format!("{ONE}{TWO}");
    let chunks: Vec<Vec<u8>> = all.as_bytes().iter().map(|b| vec![*b]).collect();
    let (endpoint, server) = chunked_server(chunks);
    assert_eq!(payloads(&endpoint), vec!["{\"n\":1}", "{\"n\":2}"]);
    server.join().unwrap();
}

#[test]
fn a_chunk_size_line_carrying_an_extension_still_names_the_size() {
    // `1a;ext=1` - the semicolon and what follows are not part of the hex.
    let (endpoint, server) = chunked_server(vec![ONE.as_bytes().to_vec()]);
    assert_eq!(payloads(&endpoint), vec!["{\"n\":1}"]);
    server.join().unwrap();
}

#[test]
fn a_body_with_no_frames_at_all_is_empty_rather_than_an_error() {
    // A heartbeat-only stream: `:` comment lines and no data. `for_each_frame` must run to the
    // terminating chunk and return cleanly.
    let (endpoint, server) = chunked_server(vec![b": ping\n\n".to_vec()]);
    assert_eq!(payloads(&endpoint), Vec::<String>::new());
    server.join().unwrap();
}
