//! A Claude Code session's own title: the name `/rename` gives it, which Claude Code appends to the
//! session's transcript and writes nowhere else a bridge can read. Naming a seat after the session
//! is what makes two sessions in one folder two labels rather than one label and a suffix.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The environment variable that moves Claude Code's own config directory.
pub const CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

/// How much of an existing transcript the FIRST read is given: a session hours old has megabytes
/// behind its last title, and a label is not worth reading all of them. What a rename appends
/// after that is read as it arrives.
pub const TAIL_BYTES: u64 = 2 << 20;

/// The two rows a `/rename` writes, and the field each names the title in. Both are written for one
/// rename, and a later rename appends another pair, so the last line of either kind is the title.
const TITLE_ROWS: [(&str, &str); 2] =
    [("custom-title", "customTitle"), ("agent-name", "agentName")];

/// Claude Code's config directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// The directory Claude Code keeps one working folder's transcripts in: the folder's own path with
/// every `/` written as `-`, under `projects` (`/home/dead/Projects/x` -> `-home-dead-Projects-x`).
pub fn project_dir(config: &Path, cwd: &Path) -> PathBuf {
    let slug: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    config.join("projects").join(slug)
}

/// The transcript of the Claude Code session `id`: looked for in `cwd`'s project directory and then
/// in its ancestors', because the server a plugin starts may begin in a subdirectory of the folder
/// `claude` was run in and the transcript is filed under that folder.
pub fn transcript_for(config: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    // A session id names a file, not a path: anything with a separator in it is not one.
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let file = format!("{id}.jsonl");
    cwd.ancestors()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| project_dir(config, d).join(&file))
        .find(|p| p.is_file())
}

/// The one transcript in `project` written since `since`, when exactly one was. A bridge whose
/// environment carries no session id can still be the session it was started for - but only when
/// nothing else in that folder could be, which is what one candidate means and two do not.
pub fn only_since(project: &Path, since: SystemTime) -> Option<PathBuf> {
    let mut found = std::fs::read_dir(project)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= since)
        });
    let (first, second) = (found.next()?, found.next());
    second.is_none().then_some(first)
}

/// This process's start, so *written since this bridge started* has something to compare against.
/// `None` where `/proc` is not this shape (a Mac): the id-less recovery then does not happen, and
/// the folder's name is what the seat is called - which is where it was before the recovery existed.
pub fn process_start() -> Option<SystemTime> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // `comm` (field 2) is parenthesised and may itself hold spaces and parentheses, so the fields
    // are counted from after the closing one: 22 (`starttime`) is the 20th of those.
    let rest = &stat[stat.rfind(')')? + 1..];
    let ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|hz| *hz > 0)?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let age = Duration::from_secs_f64((uptime - ticks as f64 / hz as f64).max(0.0));
    SystemTime::now().checked_sub(age)
}

/// The title one transcript row names, if it names one.
fn title_of(line: &str) -> Option<String> {
    // A transcript is mostly rows that name no title, and a JSON parse each is the whole cost of
    // reading one: the two words a title row must carry are what it is asked first.
    if !TITLE_ROWS.iter().any(|(kind, _)| line.contains(kind)) {
        return None;
    }
    let row: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = row["type"].as_str()?;
    let (_, field) = TITLE_ROWS.iter().find(|(k, _)| *k == kind)?;
    let named = row[field].as_str()?.trim();
    (!named.is_empty()).then(|| named.to_string())
}

/// One transcript's last title, read incrementally: a session's transcript grows every turn, so a
/// file that has not moved costs one `stat` and a file that has costs only what was appended.
#[derive(Debug, Default)]
pub struct Title {
    path: Option<PathBuf>,
    /// The byte the next read starts at: just past the last whole line read.
    at: u64,
    /// This read begins mid-file, so its first line is not a whole one and names nothing.
    fresh: bool,
    /// The file's length and modification time when it was last read.
    seen: Option<(Option<SystemTime>, u64)>,
    title: Option<String>,
    /// When a title this watcher had not read before was found, or `None` while the file has only
    /// ever said what it said at the first read. It is what tells a `/rename` that happened while
    /// the bridge was running - the one that settles which of two names is the newer.
    moved_at: Option<SystemTime>,
}

impl Title {
    /// A watcher over one transcript. A path that is not there is a session with no title yet: the
    /// file is looked for again on the next call, and a rename into existence is picked up then.
    pub fn watching(path: PathBuf) -> Title {
        Title {
            path: Some(path),
            ..Title::default()
        }
    }

    /// The transcript this watcher reads.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// When this session's title last CHANGED under this watcher: a `/rename` typed since the
    /// bridge started, and `None` for the title the file already had. A name settled before this
    /// process started is not newer than anything, so it cannot outrank a nick file.
    pub fn moved_at(&self) -> Option<SystemTime> {
        self.moved_at
    }

    /// The last title the transcript names, reading only what it gained since the last call.
    pub fn current(&mut self) -> Option<&str> {
        let path = self.path.clone()?;
        let Ok(meta) = std::fs::metadata(&path) else {
            // A transcript that is gone names nothing: the folder's own name is what is left.
            self.title = None;
            self.seen = None;
            return None;
        };
        let (len, mtime) = (meta.len(), meta.modified().ok());
        if self.seen == Some((mtime, len)) {
            return self.title.as_deref();
        }
        let mut at = self.at;
        if len < at {
            // Rewritten rather than appended to (or replaced): nothing read so far is about this file.
            at = 0;
            self.fresh = false;
            self.title = None;
        } else if at == 0 && len > TAIL_BYTES {
            at = len - TAIL_BYTES;
            self.fresh = true;
        }
        let Ok(mut file) = std::fs::File::open(&path) else {
            return self.title.as_deref();
        };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return self.title.as_deref();
        }
        let text = String::from_utf8_lossy(&bytes);
        // Only whole lines are read: the row a turn is writing right now is the next call's to name.
        let Some(cut) = text.rfind('\n') else {
            self.at = at;
            self.seen = Some((mtime, len));
            return self.title.as_deref();
        };
        let mut lines = text[..cut].split('\n');
        if self.fresh {
            lines.next();
        }
        // A title found on a file this watcher has read before is a `/rename`; one found on the
        // first read is the name the session already had, and it is nobody's news.
        let read_before = self.seen.is_some();
        for line in lines {
            if let Some(title) = title_of(line) {
                if read_before && self.title.as_deref() != Some(title.as_str()) {
                    self.moved_at = Some(SystemTime::now());
                }
                self.title = Some(title);
            }
        }
        self.at = at + cut as u64 + 1;
        self.fresh = false;
        self.seen = Some((mtime, len));
        self.title.as_deref()
    }
}

/// The title `path` names, read in one go - what a caller with no watcher to keep wants.
pub fn title_in(path: &Path) -> Option<String> {
    Title::watching(path.to_path_buf())
        .current()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bridge-titles-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append(path: &Path, rows: &[serde_json::Value]) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        file.flush().unwrap();
    }

    /// One `/rename` writes both rows; a later one appends another pair. The last line that names a
    /// title is the title, which is what a person reads as "what this session is called now".
    #[test]
    fn two_renames_and_the_last_one_wins() {
        let dir = fixture("rename");
        let transcript = dir.join("session.jsonl");
        append(
            &transcript,
            &[
                serde_json::json!({"type": "user", "message": "hi"}),
                serde_json::json!({"type": "custom-title", "customTitle": "stroppy-conformity"}),
                serde_json::json!({"type": "agent-name", "agentName": "stroppy-conformity"}),
                serde_json::json!({"type": "assistant", "message": "hello"}),
            ],
        );
        assert_eq!(title_in(&transcript).as_deref(), Some("stroppy-conformity"));
        append(
            &transcript,
            &[
                serde_json::json!({"type": "custom-title", "customTitle": "stroppy-rust"}),
                serde_json::json!({"type": "agent-name", "agentName": "stroppy-rust"}),
            ],
        );
        assert_eq!(title_in(&transcript).as_deref(), Some("stroppy-rust"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row a build is writing right now is not a title yet; the next read has it whole.
    #[test]
    fn a_half_written_row_is_left_for_the_next_read() {
        let dir = fixture("half");
        let transcript = dir.join("session.jsonl");
        append(
            &transcript,
            &[serde_json::json!({"type": "custom-title", "customTitle": "one"})],
        );
        let mut watch = Title::watching(transcript.clone());
        assert_eq!(watch.current(), Some("one"));
        // A row with no newline is a row still being written.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        use std::io::Write;
        write!(file, "{{\"type\":\"custom-title\",\"customTitle\":\"tw").unwrap();
        file.flush().unwrap();
        assert_eq!(watch.current(), Some("one"), "not a whole row yet");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "o\"}}").unwrap();
        file.flush().unwrap();
        assert_eq!(watch.current(), Some("two"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `/rename` mid-session is a row appended to a file the watcher has already read: the next
    /// look names it, and nothing has to poll or re-read the transcript to find it.
    #[test]
    fn a_rename_after_the_mtime_moved_is_picked_up() {
        let dir = fixture("moved");
        let transcript = dir.join("session.jsonl");
        append(
            &transcript,
            &[serde_json::json!({"type": "custom-title", "customTitle": "before"})],
        );
        let mut watch = Title::watching(transcript.clone());
        assert_eq!(watch.current(), Some("before"));
        // Unchanged: the same answer, and the file is not read again.
        assert_eq!(watch.current(), Some("before"));
        append(
            &transcript,
            &[serde_json::json!({"type": "agent-name", "agentName": "after"})],
        );
        assert_eq!(watch.current(), Some("after"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A transcript cut down to nothing (a compaction, a rotated file) is read from the start again
    /// rather than from an offset past its end, which is what a length-only reader would do.
    #[test]
    fn a_shorter_transcript_is_read_from_the_start() {
        let dir = fixture("shorter");
        let transcript = dir.join("session.jsonl");
        append(
            &transcript,
            &[
                serde_json::json!({"type": "custom-title", "customTitle": "longer-name"}),
                serde_json::json!({"type": "user", "message": "x".repeat(200)}),
            ],
        );
        let mut watch = Title::watching(transcript.clone());
        assert_eq!(watch.current(), Some("longer-name"));
        std::fs::write(
            &transcript,
            "{\"type\":\"custom-title\",\"customTitle\":\"b\"}\n",
        )
        .unwrap();
        assert_eq!(watch.current(), Some("b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Where the transcript is: the project directory of the folder `claude` ran in, named by the
    /// session id - and a bridge started in a subdirectory finds it in the ancestor it is filed under.
    #[test]
    fn the_transcript_is_filed_by_the_folder_claude_ran_in() {
        let dir = fixture("filed");
        let config = dir.join("config");
        let folder = dir.join("stroppy-io");
        let sub = folder.join("crates");
        std::fs::create_dir_all(&sub).unwrap();
        let project = project_dir(&config, &folder);
        assert_eq!(
            project,
            config
                .join("projects")
                .join(folder.to_string_lossy().replace('/', "-"))
        );
        std::fs::create_dir_all(&project).unwrap();
        let id = "5b0f0b1e-6a1f-4b58-9d5c-0b7d3f0f4c11";
        let transcript = project.join(format!("{id}.jsonl"));
        append(
            &transcript,
            &[serde_json::json!({"type": "custom-title", "customTitle": "stroppy-rust"})],
        );
        assert_eq!(
            transcript_for(&config, &folder, id).as_deref(),
            Some(transcript.as_path())
        );
        assert_eq!(
            transcript_for(&config, &sub, id).as_deref(),
            Some(transcript.as_path()),
            "an ancestor's project directory is this session's too"
        );
        assert_eq!(transcript_for(&config, &folder, "other-id"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two transcripts written since this process started are two sessions that could be it, and
    /// an ambiguous guess is worse than the folder's own name: the recovery answers one only.
    #[test]
    fn the_id_less_recovery_answers_only_an_unambiguous_folder() {
        let dir = fixture("recover");
        let project = dir.join("projects");
        std::fs::create_dir_all(&project).unwrap();
        let since = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        let one = project.join("a.jsonl");
        append(&one, &[serde_json::json!({"type": "user"})]);
        assert_eq!(only_since(&project, since).as_deref(), Some(one.as_path()));
        let two = project.join("b.jsonl");
        append(&two, &[serde_json::json!({"type": "user"})]);
        assert_eq!(only_since(&project, since), None, "two sessions, no guess");
        // A transcript older than this process is a session that has already gone quiet.
        assert_eq!(
            only_since(&project, SystemTime::now() + Duration::from_secs(60)),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// This process's start is a moment in the past, close enough that a file written a second ago
    /// is after it and one written a minute before is not.
    #[test]
    fn this_process_started_just_now() {
        let Some(start) = process_start() else {
            return; // no /proc of this shape: the recovery does not happen, by design
        };
        let now = SystemTime::now();
        assert!(start <= now, "{start:?} is not after {now:?}");
        assert!(
            now.duration_since(start).unwrap_or_default() < Duration::from_secs(600),
            "a test process is minutes old at most, not {:?}",
            now.duration_since(start)
        );
    }
}
