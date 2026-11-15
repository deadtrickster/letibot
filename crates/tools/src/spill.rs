//! Output is bounded by spilling: the middle goes to a store keyed by content
//! hash and the model is told where, so nothing is silently truncated. An unset
//! budget is a true no-op, not an unlimited one chosen by default.

use std::collections::BTreeMap;
use std::sync::Mutex;

use letibot_transcript::seam::Seam;
use sha2::{Digest, Sha256};

mod durable;
pub use durable::{
    DEFAULT_SPILL_BUDGET, DirStore, ROOTS, SPILLS_DIR, TOMBSTONE_CAP, TOMBSTONES, Tombstone, Usage,
    is_legacy_dir, record_root, recorded_roots,
};

/// What is being spilled, for a policy that wants to know before it decides.
#[derive(Debug, Clone, Copy)]
pub struct SpillContext<'a> {
    /// The tool that produced the output.
    pub tool: &'a str,
    /// The arguments it was called with. A predictor needs them: the whole file and
    /// forty lines of it deserve different budgets, and only these say which.
    pub args: &'a serde_json::Value,
    /// The size of the full output in UTF-8 bytes.
    pub bytes: usize,
}

/// The threshold is a decision, not a constant.
pub trait InlineBudget: Send + Sync {
    /// The most bytes this output may occupy inline, or `None` for no opinion,
    /// which is a genuine no-op and not an unlimited budget chosen by default.
    fn max_inline_bytes(&self, ctx: &SpillContext<'_>) -> Option<usize>;

    /// The most lines this output may occupy inline, `None` for no line limit.
    fn max_inline_lines(&self, _ctx: &SpillContext<'_>) -> Option<usize> {
        None
    }

    /// How this policy describes itself in `EXPLAIN`: a number nobody can trace
    /// back to a decision is worse than none.
    fn describe(&self) -> String;
}

/// Unset. The policy declines to have an opinion and nothing is ever spilled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBudget;

impl InlineBudget for NoBudget {
    fn max_inline_bytes(&self, _ctx: &SpillContext<'_>) -> Option<usize> {
        None
    }
    fn describe(&self) -> String {
        "unset (no spill)".into()
    }
}

/// One number for every tool.
#[derive(Debug, Clone, Copy)]
pub struct FixedBudget(pub usize);

impl InlineBudget for FixedBudget {
    fn max_inline_bytes(&self, _ctx: &SpillContext<'_>) -> Option<usize> {
        Some(self.0)
    }
    fn describe(&self) -> String {
        format!("fixed {} bytes", self.0)
    }
}

/// One number per tool, because a `find` and a `read` do not deserve the same
/// budget. A tool with no entry falls back to `default`, which may itself be unset.
#[derive(Debug, Clone, Default)]
pub struct PerToolBudget {
    pub default: Option<usize>,
    pub per_tool: BTreeMap<String, usize>,
}

impl InlineBudget for PerToolBudget {
    fn max_inline_bytes(&self, ctx: &SpillContext<'_>) -> Option<usize> {
        self.per_tool.get(ctx.tool).copied().or(self.default)
    }
    fn describe(&self) -> String {
        let per: Vec<String> = self
            .per_tool
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        match self.default {
            Some(d) => format!("per-tool [{}], default {d}", per.join(", ")),
            None => format!("per-tool [{}], otherwise unset", per.join(", ")),
        }
    }
}

/// The daemon's default ceiling, opencode's numbers: 50 KiB and 2000 lines.
pub const DEFAULT_INLINE_BYTES: usize = 50 * 1024;
pub const DEFAULT_INLINE_LINES: usize = 2000;

/// The generic cap every tool falls under, a new or plugin tool included. A tool's own tighter
/// cap (`bash` keeps a 30 KB tail) bites first and this never sees the excess.
#[derive(Debug, Clone)]
pub struct OutputCap {
    pub bytes: Option<usize>,
    pub lines: Option<usize>,
    /// Tools that bound themselves with an offset and report it; spilling them again only
    /// moves the same bytes behind a second locator.
    pub exempt: Vec<String>,
}

impl Default for OutputCap {
    fn default() -> Self {
        OutputCap {
            bytes: Some(DEFAULT_INLINE_BYTES),
            lines: Some(DEFAULT_INLINE_LINES),
            exempt: vec!["job_output".into(), "recall".into()],
        }
    }
}

impl OutputCap {
    fn applies(&self, tool: &str) -> bool {
        !self.exempt.iter().any(|t| t == tool)
    }
}

impl InlineBudget for OutputCap {
    fn max_inline_bytes(&self, ctx: &SpillContext<'_>) -> Option<usize> {
        self.bytes.filter(|_| self.applies(ctx.tool))
    }
    fn max_inline_lines(&self, ctx: &SpillContext<'_>) -> Option<usize> {
        self.lines.filter(|_| self.applies(ctx.tool))
    }
    fn describe(&self) -> String {
        let bytes = self
            .bytes
            .map_or("no byte cap".into(), |b| format!("{b} bytes"));
        let lines = self
            .lines
            .map_or("no line cap".into(), |l| format!("{l} lines"));
        format!("{bytes} / {lines}, except {}", self.exempt.join(", "))
    }
}

/// The predicted case, deliberately the same trait: the predictor is a function of
/// the call, so plugging a model in means writing that function and touching no
/// tool, result type or call site.
pub struct PredictedBudget<F> {
    pub predict: F,
    pub label: String,
}

impl<F> InlineBudget for PredictedBudget<F>
where
    F: Fn(&SpillContext<'_>) -> Option<usize> + Send + Sync,
{
    fn max_inline_bytes(&self, ctx: &SpillContext<'_>) -> Option<usize> {
        (self.predict)(ctx)
    }
    fn describe(&self) -> String {
        format!("predicted ({})", self.label)
    }
}

/// Where a full output went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRef {
    /// Content hash. The locator, and the argument to `read_spill`.
    pub hash: String,
    pub full_bytes: usize,
    pub inline_bytes: usize,
}

#[derive(Debug)]
pub enum SpillError {
    NotFound(String),
    /// The store held it once and evicted it over its budget.
    Evicted(Tombstone),
    Io(std::io::Error),
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpillError::NotFound(h) => write!(f, "no spilled output with hash {h}"),
            SpillError::Evicted(t) => f.write_str(&t.said()),
            SpillError::Io(e) => write!(f, "spill store: {e}"),
        }
    }
}

impl std::error::Error for SpillError {}

/// One spilled output, as `read_spill` lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillEntry {
    pub hash: String,
    pub tool: String,
    pub bytes: usize,
}

/// Where full outputs live, keyed by content hash. [`FileStore`] is the
/// host-local one: `0600` under a per-session hashed directory.
pub trait SpillStore: Send + Sync {
    fn put(&self, tool: &str, bytes: &[u8]) -> Result<String, SpillError>;
    /// A byte range of a stored output. `None` is the whole thing.
    fn get(&self, hash: &str, range: Option<std::ops::Range<usize>>)
    -> Result<Vec<u8>, SpillError>;
    fn list(&self) -> Vec<SpillEntry>;
    /// The full size of one stored output.
    fn size(&self, hash: &str) -> Option<usize> {
        self.list()
            .into_iter()
            .find(|e| e.hash == hash)
            .map(|e| e.bytes)
    }
}

/// SHA-256 truncated for legibility: the hash the ledger uses too, and long enough
/// that a collision is not the failure mode anybody meets.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())[..16].to_string()
}

/// In-process storage, for tests and for a headless run with no session directory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<BTreeMap<String, (String, Vec<u8>)>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SpillStore for MemoryStore {
    fn put(&self, tool: &str, bytes: &[u8]) -> Result<String, SpillError> {
        let hash = content_hash(bytes);
        self.inner
            .lock()
            .expect("spill store mutex")
            .insert(hash.clone(), (tool.to_string(), bytes.to_vec()));
        Ok(hash)
    }

    fn get(
        &self,
        hash: &str,
        range: Option<std::ops::Range<usize>>,
    ) -> Result<Vec<u8>, SpillError> {
        let g = self.inner.lock().expect("spill store mutex");
        let (_, bytes) = g
            .get(hash)
            .ok_or_else(|| SpillError::NotFound(hash.into()))?;
        Ok(slice_range(bytes, range))
    }

    fn list(&self) -> Vec<SpillEntry> {
        self.inner
            .lock()
            .expect("spill store mutex")
            .iter()
            .map(|(hash, (tool, bytes))| SpillEntry {
                hash: hash.clone(),
                tool: tool.clone(),
                bytes: bytes.len(),
            })
            .collect()
    }
}

fn slice_range(bytes: &[u8], range: Option<std::ops::Range<usize>>) -> Vec<u8> {
    match range {
        None => bytes.to_vec(),
        Some(r) => {
            let start = r.start.min(bytes.len());
            let end = r.end.min(bytes.len()).max(start);
            bytes[start..end].to_vec()
        }
    }
}

/// Files under a per-session directory, mode `0600`.
#[derive(Debug)]
pub struct FileStore {
    dir: std::path::PathBuf,
}

impl FileStore {
    /// `base/<hash of session id>/`: the id is hashed because it can carry a name,
    /// and a directory listing is a place names leak.
    pub fn new(base: impl AsRef<std::path::Path>, session_id: &str) -> Result<Self, SpillError> {
        let dir = base.as_ref().join(content_hash(session_id.as_bytes()));
        std::fs::create_dir_all(&dir).map_err(SpillError::Io)?;
        Ok(FileStore { dir })
    }

    fn path(&self, hash: &str) -> std::path::PathBuf {
        self.dir.join(format!("{hash}.out"))
    }
}

impl SpillStore for FileStore {
    fn put(&self, _tool: &str, bytes: &[u8]) -> Result<String, SpillError> {
        use std::io::Write;
        let hash = content_hash(bytes);
        let path = self.path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path).map_err(SpillError::Io)?;
        f.write_all(bytes).map_err(SpillError::Io)?;
        Ok(hash)
    }

    fn get(
        &self,
        hash: &str,
        range: Option<std::ops::Range<usize>>,
    ) -> Result<Vec<u8>, SpillError> {
        let bytes =
            std::fs::read(self.path(hash)).map_err(|_| SpillError::NotFound(hash.to_string()))?;
        Ok(slice_range(&bytes, range))
    }

    fn list(&self) -> Vec<SpillEntry> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return out;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(hash) = name.strip_suffix(".out") else {
                continue;
            };
            out.push(SpillEntry {
                hash: hash.to_string(),
                // The store does not carry the tool; the journal does. Empty rather
                // than invented.
                tool: String::new(),
                bytes: e.metadata().map(|m| m.len() as usize).unwrap_or(0),
            });
        }
        out
    }
}

/// The policy and the store together; one of these lives on the runtime.
pub struct Spiller {
    pub budget: Box<dyn InlineBudget>,
    pub store: Box<dyn SpillStore>,
}

impl Spiller {
    /// No threshold and an in-process store: nothing spills until a budget is
    /// configured, because unset is a no-op.
    pub fn unset() -> Self {
        Spiller {
            budget: Box::new(NoBudget),
            store: Box::new(MemoryStore::new()),
        }
    }

    pub fn new(budget: Box<dyn InlineBudget>, store: Box<dyn SpillStore>) -> Self {
        Spiller { budget, store }
    }

    /// Bound one output: what goes inline, and where the rest is if it spilled. The head and the
    /// tail stay; the middle is replaced by one [`Seam`] line naming its size and locator.
    pub fn apply(&self, payload: String, ctx: &SpillContext<'_>) -> (String, Option<SpillRef>) {
        let byte_cap = self.budget.max_inline_bytes(ctx);
        let line_cap = self.budget.max_inline_lines(ctx);
        let over_bytes = byte_cap.is_some_and(|c| payload.len() > c);
        let over_lines = line_cap.is_some_and(|c| payload.lines().count() > c);
        if !over_bytes && !over_lines {
            return (payload, None);
        }

        // Every number in the seam is at most the payload's length, so a seam drawn with that
        // number everywhere is the widest one this payload can get.
        let n = payload.len();
        let widest = Seam {
            bytes: n,
            lines: n,
            hash: "0".repeat(16),
            offset: n,
        };
        let reserve = widest.render().len() + 2;

        // Preview, seam and joins must fit the cap; if they cannot, spill is abandoned and the
        // original is left inline untruncated.
        let byte_budget = match byte_cap {
            Some(c) => match c.checked_sub(reserve).filter(|b| *b > 0) {
                Some(b) => b,
                None => return (payload, None),
            },
            None => n,
        };
        let line_budget = match line_cap {
            Some(c) if c < 3 => return (payload, None),
            Some(c) => c - 1,
            None => usize::MAX,
        };

        let hash = match self.store.put(ctx.tool, payload.as_bytes()) {
            Ok(h) => h,
            // A storage failure falls back to the untouched payload: losing the
            // output is worse than exceeding a budget.
            Err(_) => return (payload, None),
        };

        let (h, t) = head_tail(&payload, byte_budget, line_budget);
        let (head, middle, tail) = (&payload[..h], &payload[h..t], &payload[t..]);
        let seam = Seam {
            bytes: middle.len(),
            lines: middle.lines().count(),
            hash: hash.clone(),
            offset: h,
        }
        .render();
        let join = if head.is_empty() || head.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let inline = format!("{head}{join}{seam}\n{tail}");
        debug_assert!(
            byte_cap.is_none_or(|c| inline.len() <= c),
            "spill arithmetic overran the cap: {} > {byte_cap:?}",
            inline.len()
        );
        (
            inline,
            Some(SpillRef {
                hash,
                full_bytes: n,
                inline_bytes: head.len() + tail.len(),
            }),
        )
    }
}

/// Where the head ends and the tail starts: half of each budget each way, on a char boundary
/// and, where that costs less than half, on a line boundary, so `[h..t]` is a run of whole
/// lines `read_spill` can hand back as they were.
fn head_tail(s: &str, bytes: usize, lines: usize) -> (usize, usize) {
    let (head_bytes, tail_bytes) = (bytes.div_ceil(2), bytes / 2);
    let (head_lines, tail_lines) = (lines.div_ceil(2), lines / 2);

    let mut h = floor_char_boundary(s, head_bytes);
    match after_nth_newline(s, head_lines) {
        Some(i) if i <= h => h = i,
        _ => {
            if let Some(i) = s[..h].rfind('\n')
                && (i + 1) * 2 >= h
            {
                h = i + 1;
            }
        }
    }

    let mut t = ceil_char_boundary(s, s.len().saturating_sub(tail_bytes));
    let last = start_of_last_lines(s, tail_lines);
    if last >= t {
        t = last;
    } else if t > 0
        && s.as_bytes()[t - 1] != b'\n'
        && let Some(i) = s[t..].find('\n')
        && i * 2 <= s.len() - t
    {
        t += i + 1;
    }
    (h, t.max(h))
}

/// The byte after the `n`th newline, or `None` when there are fewer.
fn after_nth_newline(s: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    s.match_indices('\n').nth(n - 1).map(|(i, _)| i + 1)
}

/// Where the last `n` lines start; a trailing newline ends the last line rather than opening one.
fn start_of_last_lines(s: &str, n: usize) -> usize {
    if n == 0 {
        return s.len();
    }
    let body = s.strip_suffix('\n').unwrap_or(s);
    body.rmatch_indices('\n')
        .nth(n - 1)
        .map_or(0, |(i, _)| i + 1)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(args: &'a serde_json::Value, bytes: usize) -> SpillContext<'a> {
        SpillContext {
            tool: "read",
            args,
            bytes,
        }
    }

    #[test]
    fn unset_is_a_no_op_and_not_a_guess() {
        let s = Spiller::unset();
        let args = serde_json::json!({});
        let big = "x".repeat(10_000);
        let (inline, spill) = s.apply(big.clone(), &ctx(&args, big.len()));
        assert_eq!(inline, big);
        assert!(spill.is_none());
        assert!(s.store.list().is_empty(), "nothing may be stored either");
    }

    #[test]
    fn a_spilled_result_never_exceeds_the_cap_and_is_recoverable_whole() {
        let cap = 400;
        let s = Spiller::new(Box::new(FixedBudget(cap)), Box::new(MemoryStore::new()));
        let args = serde_json::json!({});
        let full: String = (0..400).map(|i| format!("line {i}\n")).collect();
        let (inline, spill) = s.apply(full.clone(), &ctx(&args, full.len()));
        let spill = spill.expect("a 2.8 KB payload under a 400 byte cap must spill");
        assert!(inline.len() <= cap, "{} > {cap}", inline.len());
        assert_eq!(
            inline.lines().filter(|l| Seam::parse(l).is_some()).count(),
            1
        );
        // Never truncated: the whole original is retrievable by its locator.
        let back = s.store.get(&spill.hash, None).unwrap();
        assert_eq!(String::from_utf8(back).unwrap(), full);
        // And a range works, because that is what makes the hint actionable.
        let part = s.store.get(&spill.hash, Some(0..6)).unwrap();
        assert_eq!(part, b"line 0");
    }

    #[test]
    fn head_and_tail_both_survive() {
        let s = Spiller::new(Box::new(FixedBudget(500)), Box::new(MemoryStore::new()));
        let args = serde_json::json!({});
        let full: String = (0..400).map(|i| format!("line {i}\n")).collect();
        let (inline, _) = s.apply(full.clone(), &ctx(&args, full.len()));
        assert!(inline.contains("line 0"), "head is missing: {inline}");
        assert!(inline.contains("line 399"), "tail is missing: {inline}");
    }

    #[test]
    fn a_cap_too_small_for_the_notice_abandons_the_spill_rather_than_truncating() {
        let s = Spiller::new(Box::new(FixedBudget(20)), Box::new(MemoryStore::new()));
        let args = serde_json::json!({});
        let full = "y".repeat(5_000);
        let (inline, spill) = s.apply(full.clone(), &ctx(&args, full.len()));
        assert_eq!(inline, full, "the original must be left whole");
        assert!(spill.is_none());
    }

    #[test]
    fn a_storage_failure_leaves_the_output_alone() {
        struct Broken;
        impl SpillStore for Broken {
            fn put(&self, _t: &str, _b: &[u8]) -> Result<String, SpillError> {
                Err(SpillError::Io(std::io::Error::other("disk")))
            }
            fn get(
                &self,
                h: &str,
                _r: Option<std::ops::Range<usize>>,
            ) -> Result<Vec<u8>, SpillError> {
                Err(SpillError::NotFound(h.into()))
            }
            fn list(&self) -> Vec<SpillEntry> {
                vec![]
            }
        }
        let s = Spiller::new(Box::new(FixedBudget(300)), Box::new(Broken));
        let args = serde_json::json!({});
        let full = "z".repeat(4_000);
        let (inline, spill) = s.apply(full.clone(), &ctx(&args, full.len()));
        assert_eq!(inline, full);
        assert!(spill.is_none());
    }

    #[test]
    fn multibyte_output_is_cut_on_a_character_boundary() {
        let s = Spiller::new(Box::new(FixedBudget(400)), Box::new(MemoryStore::new()));
        let args = serde_json::json!({});
        let full = "日本語のテキスト\n".repeat(200);
        let (inline, _) = s.apply(full.clone(), &ctx(&args, full.len()));
        // Getting here at all means no panic on a boundary; assert it is valid.
        assert!(inline.is_char_boundary(0));
        assert!(inline.len() <= 400);
    }

    #[test]
    fn the_predictor_is_the_same_interface() {
        // Plugging a model in later is writing this closure, not
        // touching a tool.
        let budget = PredictedBudget {
            predict: |c: &SpillContext<'_>| {
                if c.args.get("full").and_then(|v| v.as_bool()) == Some(true) {
                    None
                } else {
                    Some(200)
                }
            },
            label: "asked-for-everything heuristic".into(),
        };
        let asked = serde_json::json!({"full": true});
        assert_eq!(budget.max_inline_bytes(&ctx(&asked, 9_000)), None);
        let ordinary = serde_json::json!({});
        assert_eq!(budget.max_inline_bytes(&ctx(&ordinary, 9_000)), Some(200));
    }

    #[test]
    fn a_per_tool_budget_falls_back_to_its_default() {
        let mut b = PerToolBudget {
            default: Some(1_000),
            ..Default::default()
        };
        b.per_tool.insert("glob".into(), 100);
        let args = serde_json::json!({});
        assert_eq!(b.max_inline_bytes(&ctx(&args, 5)), Some(1_000));
        let g = SpillContext {
            tool: "glob",
            args: &args,
            bytes: 5,
        };
        assert_eq!(b.max_inline_bytes(&g), Some(100));
    }
}
