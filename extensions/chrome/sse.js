// The one SSE reader, from oracle's chrome-capture. Hand-written because the protocol is two
// fields split on a blank line, and the bridge emits single-line JSON in every `data:`.

/** Parse ONE SSE event block (the text between blank lines) -> {event, data} | null. */
export function parseSSE(chunk) {
  let event = "message", data = "";
  for (const line of chunk.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  if (!data) return null;
  try { return { event, data: JSON.parse(data) }; } catch (_) { return null; }
}

// Events are buffered across reads: a TCP chunk can cut one in half. Returns how many bytes
// could not be read (malformed events and a trailing partial), so the caller can say so.
export async function pumpSSE(stream, send) {
  const reader = stream.getReader();
  const dec = new TextDecoder();
  const len = (s) => new TextEncoder().encode(s).length;
  let buf = "", unreadable = 0;
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf("\n\n")) >= 0) {
      const block = buf.slice(0, i);
      buf = buf.slice(i + 2);
      const ev = parseSSE(block);
      if (ev) send(ev);
      else if (block.trim() && !block.startsWith(":")) unreadable += len(block);
    }
  }
  buf += dec.decode();
  if (buf.trim()) unreadable += len(buf);
  return unreadable;
}
