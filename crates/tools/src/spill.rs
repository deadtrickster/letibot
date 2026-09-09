//! Clause 5: **output is bounded and spilled, never truncated** (§8.3), with D6's
//! correction applied from the start.
//!
//! # D6: `max_inline_bytes` is not a number
//!
//! The operator's answer was *"configurable, with the ability to plug in a
//! prediction model"*. So the threshold is an **interface** — [`InlineBudget`] —
//! and a fixed byte count is merely its simplest implementation:
//!
//! | implementation | here |
//! |---|---|
//! | unset (a genuine no-op) | [`NoBudget`] |
//! | fixed threshold | [`FixedBudget`] |
//! | per-tool threshold | [`PerToolBudget`] |
//! | predicted | [`PredictedBudget`], which takes the predictor as a function |
//!
//! It keeps **no default**. `NoBudget` is not "unlimited chosen by us"; it is the
//! policy declining to have an opinion, and [`Spiller::apply`] then leaves the
//! output exactly as the tool produced it. That distinction is the reason D6 says
//! unset must stay a no-op rather than a silent guess.
//!
//! # The arithmetic, from §8.3, in order
//!
//! - the preview budget is `cap − reserve`, where `reserve` is the byte cost of
//!   the notice computed against a **worst-case digit count**, so
//!   `preview + "\n\n" + notice` never exceeds `cap`;
//! - if the notice alone would exceed the cap, **spill is abandoned** and the
//!   original is left inline rather than truncated silently;
//! - head/tail split of the preview budget: `head = ceil(budget/2)`,
//!   `tail = floor(budget/2)`;
//! - the notice is `(Omitted <N> bytes. Full <kind> result stored at: <locator>.
//!   <hint>)`;
//! - the full original is stored, and **a storage failure falls back to the
//!   untouched inline content rather than erroring the call**.
//!
//! Ours differs from DeepSeek's in one way, and it is clause 1 applied to spilling
//! itself: the locator is a content hash and `read_spill(hash, range)` is a tool,
//! so the hint is actionable rather than advisory.

use std::collections::BTreeMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// What is being spilled, for a policy that wants to know before it decides.
#[derive(Debug, Clone, Copy)]
pub struct SpillContext<'a> {
    /// The tool that produced the output.
    pub tool: &'a str,
    /// The arguments it was called with. A predictor needs these: "the whole file"
    /// and "lines 1–40 of it" deserve different budgets, and only the arguments
    /// say which one this is.
    pub args: &'a serde_json::Value,
    /// The size of the full output in UTF-8 bytes.
    pub bytes: usize,
}

/// D6's interface. **The threshold is a decision, not a constant.**
pub trait InlineBudget: Send + Sync {
    /// The most bytes this output may occupy inline, or `None` for "no opinion",
    /// which is a genuine no-op and not an unlimited budget chosen by default.
    fn max_inline_bytes(&self, ctx: &SpillContext<'_>) -> Option<usize>;

    /// How this policy describes itself in `EXPLAIN`. A number nobody can trace to
    /// a decision is the thing §6 exists to abolish.
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

/// One number for every tool. What M1 ships, when the operator sets one.
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

/// A number per tool, because *"a `find` and a `read` do not deserve the same
/// budget"*. A tool with no entry falls back to `default`, which may itself be
/// unset.
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

/// The predicted case, whose whole point is that it is the **same trait**.
///
/// The predictor is a function of the call, so plugging a model in later means
/// writing that function — not touching a tool, a result type or a call site,
/// which is what D6 says retrofitting behind a constant comparison would cost.
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
    /// The exact notice appended to the preview.
    pub notice: String,
}

#[derive(Debug)]
pub enum SpillError {
    NotFound(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpillError::NotFound(h) => write!(f, "no spilled output with hash {h}"),
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

/// Where full outputs live.
///
/// §8.3 puts them in a `tool_spill` table keyed by content hash; that table is the
/// token core's to own, so this is the interface it will implement. [`FileStore`]
/// is the host-local one M1 runs on and follows DeepSeek's mechanics: `0600` under
/// a per-session hashed directory.
pub trait SpillStore: Send + Sync {
    fn put(&self, tool: &str, bytes: &[u8]) -> Result<String, SpillError>;
    /// A byte range of a stored output. `None` is the whole thing.
    fn get(&self, hash: &str, range: Option<std::ops::Range<usize>>)
    -> Result<Vec<u8>, SpillError>;
    fn list(&self) -> Vec<SpillEntry>;
}

/// The content hash a locator is. SHA-256, truncated for legibility — the same
/// hash the ledger uses, and long enough that a collision is not the failure mode
/// anybody will meet.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())[..16].to_string()
}

/// In-process storage. What the tests and a headless run without a session
/// directory use.
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
    /// `base/<hash of session id>/`. The session id is hashed rather than used
    /// directly because it can carry a name, and a directory listing is a place
    /// names leak.
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
                // The store does not carry the tool; the journal does. Present and
                // empty rather than invented.
                tool: String::new(),
                bytes: e.metadata().map(|m| m.len() as usize).unwrap_or(0),
            });
        }
        out
    }
}

/// The policy and the store, together. One of these lives on the runtime.
pub struct Spiller {
    pub budget: Box<dyn InlineBudget>,
    pub store: Box<dyn SpillStore>,
}

impl Spiller {
    /// The M1 default: **no threshold** and an in-process store. Nothing spills
    /// until somebody configures a budget, which is D6's "unset is a no-op".
    pub fn unset() -> Self {
        Spiller {
            budget: Box::new(NoBudget),
            store: Box::new(MemoryStore::new()),
        }
    }

    pub fn new(budget: Box<dyn InlineBudget>, store: Box<dyn SpillStore>) -> Self {
        Spiller { budget, store }
    }

    /// Bound one output. Returns what goes inline and, if it spilled, where the
    /// rest is.
    pub fn apply(&self, payload: String, ctx: &SpillContext<'_>) -> (String, Option<SpillRef>) {
        let Some(cap) = self.budget.max_inline_bytes(ctx) else {
            return (payload, None);
        };
        if payload.len() <= cap {
            return (payload, None);
        }

        // The reserve is computed against the worst case: the omitted count can
        // never have more digits than the full length has.
        let digits = decimal_digits(payload.len());
        let hash_width = 16;
        let reserve = notice(&"9".repeat(digits), ctx.tool, &"0".repeat(hash_width)).len();

        // `preview + "\n\n" + notice` must fit. If the notice alone cannot, spill
        // is abandoned and the original is left inline **untruncated**.
        let Some(budget) = cap.checked_sub(reserve + 2).filter(|b| *b > 0) else {
            return (payload, None);
        };

        let hash = match self.store.put(ctx.tool, payload.as_bytes()) {
            Ok(h) => h,
            // A storage failure falls back to the untouched inline content rather
            // than erroring the call. Losing the output is worse than exceeding a
            // budget.
            Err(_) => return (payload, None),
        };

        let preview = head_tail(&payload, budget);
        let notice = notice(
            &(payload.len() - preview.len()).to_string(),
            ctx.tool,
            &hash,
        );
        let inline = format!("{preview}\n\n{notice}");
        debug_assert!(
            inline.len() <= cap,
            "spill arithmetic overran the cap: {} > {cap}",
            inline.len()
        );
        let full_bytes = payload.len();
        (
            inline,
            Some(SpillRef {
                hash,
                full_bytes,
                inline_bytes: preview.len(),
                notice,
            }),
        )
    }
}

fn notice(omitted: &str, kind: &str, locator: &str) -> String {
    format!(
        "(Omitted {omitted} bytes. Full {kind} result stored at: {locator}. \
         Call read_spill with hash={locator} — and a byte range if you only need part \
         of it — to get the rest.)"
    )
}

fn decimal_digits(n: usize) -> usize {
    let mut d = 1;
    let mut n = n;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// `head = ceil(budget/2)`, `tail = floor(budget/2)`, both shrunk to a char
/// boundary and, where it costs nothing, to a line boundary — a head cut mid-line
/// and joined to a tail cut mid-line reads as one line that never existed.
fn head_tail(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let head_budget = budget.div_ceil(2);
    // One byte of the budget goes to the newline that joins the halves.
    let tail_budget = (budget / 2).saturating_sub(1);

    let head_end = floor_char_boundary(s, head_budget);
    let head = trim_to_last_newline(&s[..head_end]);

    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(tail_budget));
    let tail = trim_to_first_newline(&s[tail_start..]);

    if tail.is_empty() {
        head.to_string()
    } else {
        format!("{head}\n{tail}")
    }
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

fn trim_to_last_newline(s: &str) -> &str {
    match s.rfind('\n') {
        // Only if it does not throw away most of the half: a single very long line
        // would otherwise produce an empty preview.
        Some(i) if i * 2 >= s.len() => &s[..i],
        _ => s,
    }
}

fn trim_to_first_newline(s: &str) -> &str {
    match s.find('\n') {
        Some(i) if i * 2 <= s.len() => &s[i + 1..],
        _ => s,
    }
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
        assert!(inline.contains("Omitted"));
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
        // D6's point: plugging a model in later is writing this closure, not
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
