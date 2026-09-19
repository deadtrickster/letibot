//! `digest` — send a large finding to a subagent and get back the answer.
//!
//! **The second half of `transcript`, and the reason `transcript` can afford to
//! search everything.** The operator, 2026-09-19: *"even more — we can do a
//! summarization of all findings by streaming them to a subagent… as a second
//! tool… and offer it for big matches"*.
//!
//! # The cost this exists to refuse
//!
//! A transcript sweep routinely matches more than the window it is being read
//! into. The ordinary answer — return a page, offer `offset`, let the caller page
//! — is the wrong one here, because paging a conversation into a context to find
//! one fact about it spends the whole conversation to learn one sentence. It is
//! the cost `transcript` was built to avoid, arrived at one page at a time.
//!
//! So the bytes never cross this context. `digest` re-runs the search itself,
//! chunks what it finds, and folds the chunks through a subagent — each chunk
//! going from the tool straight into a child's prompt. What comes back here is
//! the answer, and the answer is the size of an answer.
//!
//! ```text
//!   transcript(match: "compaction")  ->  412 matched, 12 shown, and an offer
//!   digest(match: "compaction", question: "why did the summary get discarded?")
//!                                    ->  one paragraph, 412 rows read by someone else
//! ```
//!
//! # Folding, not map-reduce
//!
//! Each chunk is answered **with what the previous chunks concluded already in
//! hand**, so a fact established in chunk 1 is available when chunk 7 contradicts
//! it. A fan-out that summarised each chunk independently and concatenated the
//! results would produce seven partial answers and no reconciliation, which for a
//! conversation — where the interesting thing is almost always that something
//! CHANGED — is the one shape that cannot find what is being asked for.
//!
//! The order is oldest-first for the same reason: a conversation read backwards
//! reports its conclusions before the things that caused them.
//!
//! # What it will not do
//!
//! It does not answer from its own knowledge. A subagent that read the rows and
//! found nothing says so, and this tool passes that through rather than
//! smoothing it into a plausible summary — a digest that invents continuity is
//! worse than no digest, because it reads exactly like one that found it.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::task::{TaskRunner, TaskSpec, TaskStatus};
use super::transcript::{TranscriptQuery, TranscriptSource};

/// One fold step: a subagent reads `chunk` and updates `so_far`.
///
/// A trait rather than a direct `TaskRunner` call so the daemon can run the fold
/// however it likes — a nested turn, a cheaper model, a pool — without this file
/// knowing. [`SubagentDigest`] is the one that ships, over the `task` seam that
/// already exists.
pub trait DigestRunner: Send + Sync {
    /// `so_far` is empty on the first chunk. Returns what is now known.
    fn fold(
        &self,
        question: &str,
        so_far: &str,
        chunk: &str,
        n: usize,
        of: usize,
    ) -> Result<String, String>;
}

/// The shipping fold: one subagent per chunk, over [`TaskRunner`].
///
/// A subagent per chunk rather than one long-lived child, because `TaskRunner`'s
/// contract is start-then-collect and a child cannot be handed a second prompt.
/// Each one is seated `read-only` — it is being asked to read text that is
/// already in its prompt, and a digest that could write is a summariser that can
/// edit the thing it is summarising.
pub struct SubagentDigest {
    runner: Arc<dyn TaskRunner>,
    /// How long one chunk may take before the fold gives up and reports what it
    /// has. Generous: the child is reading, not searching.
    per_chunk: std::time::Duration,
}

impl SubagentDigest {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        SubagentDigest {
            runner,
            per_chunk: std::time::Duration::from_secs(300),
        }
    }

    pub fn with_timeout(mut self, per_chunk: std::time::Duration) -> Self {
        self.per_chunk = per_chunk;
        self
    }
}

impl DigestRunner for SubagentDigest {
    fn fold(
        &self,
        question: &str,
        so_far: &str,
        chunk: &str,
        n: usize,
        of: usize,
    ) -> Result<String, String> {
        let prompt = format!(
            "You are reading part {n} of {of} of a longer body of text on somebody \
             else's behalf. They asked:\n\n  {question}\n\n{}\
             Answer ONLY from the text below. If this part does not bear on the \
             question, say so in one line and repeat what was already known — do not \
             pad, and do not answer from your own knowledge. Quote the exact words \
             where the text settles something.\n\n\
             ----- part {n} of {of} -----\n{chunk}\n----- end -----",
            if so_far.is_empty() {
                String::new()
            } else {
                format!(
                    "What the earlier parts established, which you are updating rather \
                     than replacing:\n\n{so_far}\n\n"
                )
            },
        );
        let spec = TaskSpec {
            role: "researcher".into(),
            downgrade: crate::schema::Downgrade::parse("read-only").unwrap_or_default(),
            ..Default::default()
        };
        let handle = self.runner.start(&prompt, &spec)?;
        match self.runner.collect(&handle, self.per_chunk) {
            TaskStatus::Done { answer } => Ok(answer),
            TaskStatus::Failed { why } => Err(format!("part {n} of {of}: {why}")),
            TaskStatus::Running { .. } => Err(format!(
                "part {n} of {of} was still running after {}s",
                self.per_chunk.as_secs()
            )),
            TaskStatus::Unknown => Err(format!(
                "part {n} of {of}: the subagent `{handle}` was started and then could \
                 not be found, so nothing read it"
            )),
        }
    }
}

/// No runner: refuse by name. A digest that quietly answered from the caller's own
/// model would be this tool doing the exact thing it exists to avoid.
pub struct NoDigest;

impl DigestRunner for NoDigest {
    fn fold(&self, _: &str, _: &str, _: &str, _: usize, _: usize) -> Result<String, String> {
        Err("no subagent runner is installed in this session, so there is nobody to \
             read the findings. `transcript` with a `limit` still works — it costs \
             this context the rows."
            .into())
    }
}

pub struct DigestTool {
    src: Arc<dyn TranscriptSource>,
    runner: Arc<dyn DigestRunner>,
}

impl DigestTool {
    pub fn new(src: Arc<dyn TranscriptSource>, runner: Arc<dyn DigestRunner>) -> Self {
        DigestTool { src, runner }
    }
}

/// Bytes of findings per subagent prompt. Sized so a chunk plus the question plus
/// the running answer sits well inside a small model's window — the child doing
/// the reading is not necessarily the model doing the asking.
const CHUNK: usize = 24_000;
/// The most chunks one call will fold. A sweep bigger than this is reported with
/// what it covered, not silently half-read: the failure mode of an unbounded fold
/// is an answer about the first third of a conversation presented as an answer
/// about the conversation.
const MAX_CHUNKS: usize = 24;
/// Rows the search may pull. High — the point is to read everything — but finite,
/// so a `match`-less call on a huge session is bounded rather than unbounded.
const MAX_ROWS: usize = 4_000;

impl Tool for DigestTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "digest",
            "ASK A QUESTION ABOUT MORE TRANSCRIPT THAN FITS IN YOUR CONTEXT. Takes the \
             same predicates as `transcript` — `match`, `kind`, `tool`, `last`, \
             `session`, `history` — plus `question`. It re-runs the search with no \
             limit, streams every matching row to a subagent in order, and returns \
             what the subagent concluded. The rows never enter your context: you pay \
             for the answer, not the conversation. Use it whenever `transcript` \
             reports more matches than it showed, and whenever the question is \
             \"what happened\" rather than \"find me this line\".",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "What you want to know. Be specific — the subagent sees this and the rows, and nothing else about why you are asking."
                    },
                    "match": {"type": "string", "description": "Text to look for, case-insensitive. Omit to digest every row in scope."},
                    "kind": {"type": "string", "description": "user, assistant, reasoning, tool_result, system, mark — comma-separated for several."},
                    "tool": {"type": "string", "description": "For tool_result rows: which tool produced them."},
                    "last": {"type": "integer", "description": "Only the newest N rows."},
                    "session": {"type": "string", "description": "Another session, by id or part of its title. Defaults to this one."},
                    "history": {"type": "boolean", "description": "Walk back into transcripts this session forked from. Default true."}
                },
                "required": ["question"]
            }),
            // `Session`, like `task`: it runs no host command itself, and the child's
            // own gate governs whatever the child does.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(question) = args
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|q| !q.is_empty())
        else {
            return Invocation::failed(
                "digest needs a question",
                "Give `question` the thing you want to know — the subagent sees it and \
                 the rows, and nothing else. Without one there is nothing to read FOR, \
                 and a summary of a conversation with no question in front of it is \
                 the conversation again, longer."
                    .to_string(),
            );
        };

        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let q = TranscriptQuery {
            session: s("session"),
            matching: s("match"),
            kinds: s("kind")
                .map(|k| {
                    k.split(',')
                        .map(|p| p.trim().to_ascii_lowercase())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            tool: s("tool"),
            last: args.get("last").and_then(|v| v.as_u64()).map(|v| v as u32),
            history: args.get("history").and_then(|v| v.as_bool()).unwrap_or(true),
            limit: MAX_ROWS,
            ..Default::default()
        };

        let hits = match self.src.find(&q) {
            Ok(h) => h,
            Err(e) => {
                return Invocation::failed(
                    e,
                    "Nothing was read and no subagent was started.".to_string(),
                );
            }
        };
        if hits.rows.is_empty() {
            return Invocation::ok(format!(
                "nothing matched, so there was nothing to digest and no subagent was \
                 started. {} row(s) were searched across {} transcript(s) of session \
                 {}. Call `transcript` with the same predicates for the miss report — \
                 it names which predicate excluded what.",
                hits.scanned,
                hits.chain.len(),
                hits.session_id
            ));
        }

        // Oldest first: a conversation read backwards states its conclusions before
        // the things that caused them, and the fold is cumulative.
        let mut rows = hits.rows.clone();
        rows.sort_by_key(|r| (std::cmp::Reverse(r.generation), r.seq));

        let chunks = chunk_rows(&rows, CHUNK);
        let total = chunks.len().min(MAX_CHUNKS);
        let covered: usize = chunks.iter().take(total).map(|c| c.rows).sum();

        let mut so_far = String::new();
        for (i, c) in chunks.iter().take(total).enumerate() {
            let n = i + 1;
            // §8.5 liveness: a fold is minutes of work with nothing on the screen,
            // and the operator's rule from the oracle wait is that a pause nobody
            // announces reads as a hang.
            ctx.progress(format!(
                "digesting part {n} of {total} ({} rows, {} bytes)",
                c.rows,
                c.text.len()
            ));
            match self.runner.fold(question, &so_far, &c.text, n, total) {
                Ok(answer) => so_far = answer,
                Err(e) => {
                    // What was read is still worth having, and saying how far it got
                    // is the difference between a partial answer and a wrong one.
                    return Invocation::failed(
                        format!("the fold stopped at part {n} of {total}: {e}"),
                        if so_far.is_empty() {
                            "Nothing was read before it stopped, so there is no partial \
                             answer to report."
                                .to_string()
                        } else {
                            format!(
                                "What the first {} part(s) had established, which covers \
                                 the OLDEST rows and not the newest:\n\n{so_far}",
                                n - 1
                            )
                        },
                    );
                }
            }
        }

        let mut out = format!(
            "{so_far}\n\n---\nRead by a subagent over {covered} row(s) of session {}, \
             in {total} part(s), across {} transcript(s){}. {} row(s) were searched.\n",
            hits.session_id,
            hits.chain.len(),
            if rows.iter().any(|r| r.generation > 0) {
                " — including transcripts this conversation forked away from, which are \
                 not in your context"
            } else {
                ""
            },
            hits.scanned,
        );
        if chunks.len() > total {
            out.push_str(&format!(
                "\nNOT EVERYTHING: {} of {} matched rows were read. The rest are the \
                 NEWEST ones, cut at the {MAX_CHUNKS}-part ceiling. Narrow with `match` \
                 or `last` and ask again — this answer is about the older end of the \
                 conversation.\n",
                covered,
                rows.len(),
            ));
        }
        if hits.matched > rows.len() {
            out.push_str(&format!(
                "\n{} rows matched but only {} were pulled ({MAX_ROWS} is the ceiling), \
                 so this is not the whole of what matched.\n",
                hits.matched,
                rows.len()
            ));
        }
        Invocation::ok(out)
    }
}

/// One chunk of rows, rendered for a prompt.
struct Chunk {
    text: String,
    rows: usize,
}

/// Pack rows into chunks at a byte budget, never splitting a row across two —
/// a row cut in half is read by the child as two rows, and a tool result whose
/// first half says `ok` and second half says `failed` is exactly the row that
/// gets cut.
///
/// A single row larger than the budget gets a chunk of its own rather than being
/// dropped: it is over budget either way, and the child can at least be told what
/// it is looking at.
fn chunk_rows(rows: &[super::transcript::TranscriptRow], cap: usize) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut cur = String::new();
    let mut n = 0usize;
    for r in rows {
        let piece = format!(
            "[gen {} seq {} {}{}]\n{}\n\n",
            r.generation,
            r.seq,
            r.kind,
            if r.tool.is_empty() {
                String::new()
            } else {
                format!(" {}", r.tool)
            },
            r.text.trim()
        );
        if !cur.is_empty() && cur.len() + piece.len() > cap {
            out.push(Chunk { text: std::mem::take(&mut cur), rows: n });
            n = 0;
        }
        cur.push_str(&piece);
        n += 1;
    }
    if !cur.is_empty() {
        out.push(Chunk { text: cur, rows: n });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::transcript::{ChainLink, SessionRow, TranscriptHits, TranscriptRow};
    use std::sync::Mutex;

    struct Rows(Vec<TranscriptRow>);

    impl TranscriptSource for Rows {
        fn sessions(&self) -> Vec<SessionRow> {
            Vec::new()
        }
        fn find(&self, q: &TranscriptQuery) -> Result<TranscriptHits, String> {
            let rows: Vec<TranscriptRow> = self
                .0
                .iter()
                .filter(|r| match &q.matching {
                    None => true,
                    Some(m) => r.text.to_lowercase().contains(&m.to_lowercase()),
                })
                .cloned()
                .collect();
            Ok(TranscriptHits {
                matched: rows.len(),
                scanned: self.0.len(),
                session_id: "s1".into(),
                session_title: String::new(),
                chain: vec![ChainLink {
                    transcript_id: "s1#t0".into(),
                    generation: 0,
                    rows: self.0.len() as u32,
                    forked_at_seq: None,
                }],
                rows,
            })
        }
    }

    /// Records every fold it is asked for, so a test can assert on the ORDER and on
    /// what each part was handed.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(String, String, usize, usize)>>,
    }

    impl DigestRunner for Recorder {
        fn fold(
            &self,
            question: &str,
            so_far: &str,
            chunk: &str,
            n: usize,
            of: usize,
        ) -> Result<String, String> {
            self.seen
                .lock()
                .unwrap()
                .push((so_far.to_string(), chunk.to_string(), n, of));
            Ok(format!("after part {n}: {question}"))
        }
    }

    struct Boom;
    impl DigestRunner for Boom {
        fn fold(&self, _: &str, _: &str, _: &str, n: usize, _: usize) -> Result<String, String> {
            if n == 1 {
                Ok("part one said something".into())
            } else {
                Err("the child died".into())
            }
        }
    }

    fn row(generation: u32, seq: u32, text: &str) -> TranscriptRow {
        TranscriptRow {
            transcript_id: format!("s1#t{generation}"),
            seq,
            kind: "user".into(),
            tool: String::new(),
            text: text.into(),
            created_ms: 0,
            generation,
        }
    }

    fn ask(
        rows: Vec<TranscriptRow>,
        runner: Arc<dyn DigestRunner>,
        args: Value,
    ) -> crate::ToolResult {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(DigestTool::new(Arc::new(Rows(rows)), runner)))
            .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
        rt.invoke(
            "t",
            &letibot_transcript::ToolCall {
                id: "c".into(),
                name: "digest".into(),
                arguments: args.to_string(),
            },
            &mut crate::NullToolSink,
        )
    }

    /// The whole point: many rows go out, one answer comes back, and the rows are
    /// not in it.
    #[test]
    fn the_rows_go_to_the_subagent_and_only_the_answer_comes_back() {
        let rows: Vec<TranscriptRow> = (0..40)
            .map(|i| row(0, i, &format!("row {i}: {}", "x".repeat(2_000))))
            .collect();
        let rec: Arc<Recorder> = Arc::new(Recorder::default());
        let r = ask(rows, rec.clone(), serde_json::json!({"question": "what happened"}));
        // More than one part, because 40 * 2k is past the chunk budget.
        let seen = rec.seen.lock().unwrap();
        assert!(seen.len() > 1, "it folded in {} part(s)", seen.len());
        // The answer is the LAST fold's, and the bulk never reached this context.
        assert!(r.payload.contains("what happened"), "{}", r.payload);
        assert!(
            !r.payload.contains(&"x".repeat(2_000)),
            "the rows did not come back: {} bytes",
            r.payload.len()
        );
        assert!(r.payload.contains("Read by a subagent over 40 row(s)"), "{}", r.payload);
    }

    /// The fold is cumulative and oldest-first: part 2 is handed part 1's answer,
    /// and generation 1 (older) is read before generation 0.
    #[test]
    fn the_fold_carries_what_is_known_and_reads_oldest_first() {
        let rows = vec![
            row(0, 0, &format!("the newer one {}", "n".repeat(20_000))),
            row(1, 9, &format!("the older one {}", "o".repeat(20_000))),
        ];
        let rec: Arc<Recorder> = Arc::new(Recorder::default());
        ask(rows, rec.clone(), serde_json::json!({"question": "q"}));
        let seen = rec.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "two chunks");
        assert!(seen[0].1.contains("the older one"), "gen 1 is read first");
        assert!(seen[1].1.contains("the newer one"), "gen 0 second");
        assert!(seen[0].0.is_empty(), "the first fold knows nothing yet");
        assert_eq!(seen[1].0, "after part 1: q", "the second is handed the first");
    }

    /// A fold that dies mid-way reports how far it got and what it had — a partial
    /// answer labelled as partial, not a whole one that is quietly missing the end.
    #[test]
    fn a_dead_subagent_returns_what_was_read_and_says_it_is_the_old_end() {
        let rows = vec![
            row(0, 0, &"a".repeat(20_000)),
            row(0, 1, &"b".repeat(20_000)),
        ];
        let r = ask(rows, Arc::new(Boom), serde_json::json!({"question": "q"}));
        assert!(
            matches!(&r.outcome, letibot_transcript::ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(r.payload.contains("part one said something"), "{}", r.payload);
        assert!(r.payload.contains("OLDEST"), "{}", r.payload);
    }

    /// No question is a refusal, not a summary of everything.
    #[test]
    fn a_digest_without_a_question_refuses() {
        let r = ask(vec![row(0, 0, "x")], Arc::new(NoDigest), serde_json::json!({}));
        assert!(matches!(&r.outcome, letibot_transcript::ToolOutcome::Failed { .. }));
        assert!(r.payload.contains("nothing to read FOR"), "{}", r.payload);
    }

    /// Nothing matched: no subagent is started, and the caller is sent to
    /// `transcript` for the miss report rather than given an empty summary.
    #[test]
    fn a_miss_starts_no_subagent() {
        let r = ask(
            vec![row(0, 0, "something else")],
            Arc::new(NoDigest),
            serde_json::json!({"question": "q", "match": "absent"}),
        );
        assert!(r.payload.contains("nothing to digest"), "{}", r.payload);
        assert!(r.payload.contains("no subagent was"), "{}", r.payload);
    }

    /// A row larger than the chunk budget gets its own chunk rather than being
    /// split — a tool result cut in half reads as two results.
    #[test]
    fn an_oversized_row_is_never_split() {
        let rows = vec![row(0, 0, &"z".repeat(50_000)), row(0, 1, "small")];
        let chunks = chunk_rows(&rows, CHUNK);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].rows, 1);
        assert!(chunks[0].text.contains(&"z".repeat(50_000)), "whole, not halved");
    }

    #[test]
    fn no_runner_refuses_by_name_rather_than_answering_here() {
        let r = ask(
            vec![row(0, 0, "x")],
            Arc::new(NoDigest),
            serde_json::json!({"question": "q"}),
        );
        assert!(r.payload.contains("nobody to read") || {
            matches!(&r.outcome, letibot_transcript::ToolOutcome::Failed { reason }
                if reason.contains("nobody to read"))
        });
    }
}
