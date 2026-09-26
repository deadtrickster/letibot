//! Keeping the **raw frames** when the stream stops accounting for itself.
//!
//! # Why a whole module for a debug dump
//!
//! T23 cost two of the operator's sessions and produced one line of evidence:
//!
//! ```text
//! a frame advanced tokens_predicted 2477 -> 2479 but carried 1 id(s)
//! ```
//!
//! That line says a frame was wrong. It does not say *which* frame, what it
//! carried, what came before it, or whether the missing id arrived one frame later
//! — and those are the four questions that decide whether the guard or the server
//! is at fault. A `FrameMismatch` is rare, content-dependent and not reproducible
//! on demand from a transcript, so the only moment the evidence exists is the
//! moment it fires. It is caught here rather than reconstructed later.
//!
//! **And that line is now two lines**, which is the change R12's shape asked for: the
//! direction of the mismatch says whether the server withheld ids it counted (`ids <
//! advance` — the UTF-8 gate, i.e. a llama.cpp without `d10f94713`) or sent more than it
//! counted (`ids > advance` — a fault no withholding explains). See
//! [`crate::stream::Mismatch`]. This module is unchanged by that and is what the second
//! case points at: when the reading is *over-sent*, the captured frames are the only
//! evidence there is.
//!
//! # What it keeps, and why the frames *after* matter most
//!
//! The offending frame, the two before it, and the next few. The trailing ones are
//! the load-bearing part: "the id arrives one frame late" and "the id never
//! arrives" produce the same error message and want opposite fixes. Collecting
//! them costs a few more frames off a socket that is about to be closed anyway.
//!
//! The frames are stored **verbatim**, as the payload bytes the server sent. A
//! re-serialised `Chunk` would be this module's own understanding of the frame,
//! which is the thing under suspicion.
//!
//! # This is not a fallback path
//!
//! Nothing here feeds the accumulator, and a capture that fails to write changes
//! no outcome — it downgrades to a warning. The ledger's contents are decided by
//! [`crate::stream`] alone, exactly as before.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// How many frames before the offender are kept.
const LEAD: usize = 2;
/// How many frames after it are read before the socket is closed.
const TRAIL: usize = 3;

/// Where captures go. A field on the engine rather than a global, so a test can
/// point it somewhere and a caller can switch it off.
#[derive(Debug, Clone)]
pub struct FrameCapture {
    dir: Option<PathBuf>,
}

impl Default for FrameCapture {
    /// On by default.
    ///
    /// The asymmetry is the whole argument: a capture that is never read costs a
    /// few kilobytes in the temp directory, and a capture that was switched off
    /// costs another session. `LETIBOT_FRAME_CAPTURE_DIR` moves it; setting that
    /// variable to the empty string switches it off.
    fn default() -> Self {
        match std::env::var("LETIBOT_FRAME_CAPTURE_DIR") {
            Ok(s) if s.is_empty() => FrameCapture { dir: None },
            Ok(s) => FrameCapture {
                dir: Some(PathBuf::from(s)),
            },
            Err(_) => FrameCapture {
                dir: Some(std::env::temp_dir().join("letibot-frames")),
            },
        }
    }
}

impl FrameCapture {
    pub fn to_dir(dir: impl Into<PathBuf>) -> Self {
        FrameCapture {
            dir: Some(dir.into()),
        }
    }

    pub fn disabled() -> Self {
        FrameCapture { dir: None }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Start recording one turn's frames.
    pub fn begin(&self, turn_id: &str) -> CaptureSession {
        CaptureSession {
            dir: self.dir.clone(),
            turn_id: turn_id.to_string(),
            lead: VecDeque::with_capacity(LEAD + 1),
            armed: None,
            index: 0,
        }
    }
}

/// One turn's rolling window.
#[derive(Debug)]
pub struct CaptureSession {
    dir: Option<PathBuf>,
    turn_id: String,
    lead: VecDeque<String>,
    armed: Option<Armed>,
    index: usize,
}

#[derive(Debug)]
struct Armed {
    reason: String,
    at: usize,
    before: Vec<String>,
    frame: String,
    after: Vec<String>,
}

impl CaptureSession {
    /// Feed every raw `data:` payload, in order, before it is classified.
    ///
    /// Called unconditionally: the window has to already hold the frames *before*
    /// the offender by the time the offender is recognised.
    ///
    /// **And that is why the window is a ring that REUSES its strings.** This is called once per
    /// SSE frame — one frame per token on this server — for a window that holds three, and a
    /// capture almost never fires: `push_back(payload.to_string())` paid a fresh `String` for every
    /// token of every turn so that a window nobody reads could hold a copy of it. The entry leaving
    /// the window has exactly the storage the arriving one needs, so it is emptied and refilled
    /// instead of freed and re-allocated. Steady state allocates nothing.
    ///
    /// The window's *contents* are unchanged — same three most recent frames, in order — and that
    /// is the whole risk of a change like this: a ring that reuses storage is a ring that can hand
    /// a caller a buffer that has since been overwritten. Nothing here hands out the storage;
    /// [`CaptureSession::arm`] is the only reader and it copies what it keeps.
    pub fn observe(&mut self, payload: &str) {
        if let Some(a) = &mut self.armed {
            if a.after.len() < TRAIL {
                a.after.push(payload.to_string());
            }
            return;
        }
        // `LEAD + 1`: the newest entry is a candidate offender, and `arm` takes it
        // back out. Sizing the window at `LEAD` would leave one predecessor.
        if self.lead.len() == LEAD + 1 {
            // The evicted slot is the one being refilled, so its buffer is taken, emptied and put
            // back rather than replaced.
            let mut reused = self.lead.pop_front().expect("len is LEAD + 1");
            reused.clear();
            reused.push_str(payload);
            self.lead.push_back(reused);
        } else {
            self.lead.push_back(payload.to_string());
        }
        self.index += 1;
    }

    /// The frame most recently handed to [`CaptureSession::observe`] is the one
    /// that was refused. Everything already in the window becomes its context.
    pub fn arm(&mut self, reason: impl Into<String>) {
        if self.armed.is_some() {
            return;
        }
        let frame = self.lead.pop_back().unwrap_or_default();
        self.armed = Some(Armed {
            reason: reason.into(),
            at: self.index.saturating_sub(1),
            before: self.lead.iter().cloned().collect(),
            frame,
            after: Vec::new(),
        });
    }

    pub fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    /// Whether enough trailing frames have been collected to stop reading.
    pub fn trail_complete(&self) -> bool {
        self.armed.as_ref().is_some_and(|a| a.after.len() >= TRAIL)
    }

    /// Write the capture out. `Ok(None)` when nothing was armed or capture is off.
    ///
    /// The file is JSON built by hand rather than through `serde_json::to_string`
    /// on a struct, because every frame must land in it **verbatim** — a frame that
    /// has been through a parser and back is a frame this crate has already had an
    /// opinion about.
    pub fn write(&self) -> Result<Option<PathBuf>, std::io::Error> {
        let (Some(dir), Some(a)) = (&self.dir, &self.armed) else {
            return Ok(None);
        };
        std::fs::create_dir_all(dir)?;
        let name = format!("frame-mismatch-{}.json", sanitise(&self.turn_id));
        let path = dir.join(name);
        let mut out = String::new();
        out.push_str("{\n  \"reason\": ");
        push_json_str(&mut out, &a.reason);
        out.push_str(",\n  \"turn_id\": ");
        push_json_str(&mut out, &self.turn_id);
        out.push_str(&format!(",\n  \"frame_index\": {}", a.at));
        out.push_str(",\n  \"before\": [\n");
        push_frames(&mut out, &a.before);
        out.push_str("  ],\n  \"frame\": ");
        push_json_str(&mut out, &a.frame);
        out.push_str(",\n  \"after\": [\n");
        push_frames(&mut out, &a.after);
        out.push_str("  ]\n}\n");
        std::fs::write(&path, out)?;
        Ok(Some(path))
    }
}

fn push_frames(out: &mut String, frames: &[String]) {
    for (i, f) in frames.iter().enumerate() {
        out.push_str("    ");
        push_json_str(out, f);
        if i + 1 < frames.len() {
            out.push(',');
        }
        out.push('\n');
    }
}

/// A frame is kept as a JSON *string*, not as embedded JSON: a frame that failed
/// to parse is exactly the frame worth keeping, and embedding it would make the
/// capture file unreadable at the moment it matters most.
fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn sanitise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<String> {
        (0..10).map(|i| format!(r#"{{"n":{i}}}"#)).collect()
    }

    #[test]
    fn the_window_holds_the_offender_its_predecessors_and_its_successors() {
        let dir = std::env::temp_dir().join(format!("letibot-capture-test-{}", std::process::id()));
        let cap = FrameCapture::to_dir(&dir);
        let mut s = cap.begin("turn-7");
        let fs = frames();
        for f in &fs[..5] {
            s.observe(f);
        }
        // fs[4] is the frame that was refused.
        s.arm("a frame advanced tokens_predicted 3 -> 5 but carried 1 id(s)");
        assert!(s.is_armed());
        assert!(!s.trail_complete());
        for f in &fs[5..] {
            s.observe(f);
        }
        assert!(s.trail_complete());
        let path = s.write().unwrap().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["frame"], r#"{"n":4}"#);
        assert_eq!(v["before"], serde_json::json!([r#"{"n":2}"#, r#"{"n":3}"#]));
        // The trailing frames are the ones that say whether the id arrived late.
        assert_eq!(
            v["after"],
            serde_json::json!([r#"{"n":5}"#, r#"{"n":6}"#, r#"{"n":7}"#])
        );
        assert!(v["reason"].as_str().unwrap().contains("carried 1 id(s)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_is_stored_verbatim_even_when_it_is_not_valid_json() {
        let dir = std::env::temp_dir().join(format!("letibot-capture-bad-{}", std::process::id()));
        let mut s = FrameCapture::to_dir(&dir).begin("t");
        s.observe("{\"truncated\": \"oh \\ no\"\n");
        s.arm("protocol");
        let path = s.write().unwrap().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["frame"], "{\"truncated\": \"oh \\ no\"\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_when_no_frame_was_refused() {
        let dir = std::env::temp_dir().join(format!("letibot-capture-none-{}", std::process::id()));
        let mut s = FrameCapture::to_dir(&dir).begin("t");
        s.observe("{}");
        assert_eq!(s.write().unwrap(), None);
        assert!(!dir.exists());
    }

    #[test]
    fn capture_can_be_switched_off_and_then_writes_nothing() {
        let mut s = FrameCapture::disabled().begin("t");
        s.observe("{}");
        s.arm("x");
        assert!(s.is_armed());
        assert_eq!(s.write().unwrap(), None);
    }
}
