//! A persistent shadow conversation resumed from its own record after a restart. Each kept turn's
//! row ends with a `--- kept turn {json}` line (tail sent, reply, authored spans, fed row reached),
//! so the replayed requests are the same bytes and a provider's cached prefix still matches.

use super::Job;
use super::feed::Log;
use super::verdict::Span;
use super::words::Author;

/// The marker of the kept-turn line. It is the body's last line, written after any watched text,
/// so a row quoting this marker cannot forge one.
pub const MARK: &str = "--- kept turn ";

/// One kept turn of a job's conversation, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub job: Job,
    pub turn: usize,
    /// The fed-row index the conversation had sent up to once this turn was kept.
    pub upto: usize,
    /// The log's chain at fed row `upto - 1`: a restart refeeds the watched rows, and a resume is
    /// only safe when the same rows land at the same indexes.
    pub last: Option<String>,
    pub tail: String,
    pub reply: String,
    pub spans: Vec<Span>,
}

fn author_str(a: &Author) -> String {
    match a {
        Author::Operator => "operator".into(),
        Author::Assistant => "assistant".into(),
        Author::Tool => "tool".into(),
        Author::Harness => "harness".into(),
        Author::Peer(from) => format!("peer:{from}"),
    }
}

fn author_of(s: &str) -> Option<Author> {
    Some(match s {
        "operator" => Author::Operator,
        "assistant" => Author::Assistant,
        "tool" => Author::Tool,
        "harness" => Author::Harness,
        other => Author::Peer(other.strip_prefix("peer:")?.to_string()),
    })
}

impl Kept {
    /// The line appended to the exchange row's body.
    pub fn line(&self) -> String {
        let spans: Vec<serde_json::Value> = self
            .spans
            .iter()
            .map(|s| serde_json::json!([s.start, s.end, author_str(&s.author), s.row]))
            .collect();
        let v = serde_json::json!({
            "job": self.job.as_str(),
            "turn": self.turn,
            "upto": self.upto,
            "last": self.last,
            "tail": self.tail,
            "reply": self.reply,
            "spans": spans,
        });
        format!("{MARK}{v}\n")
    }

    /// The kept turn a row body ends with, if it ends with one.
    pub fn parse(body: &str) -> Option<Kept> {
        let last = body.trim_end_matches('\n').rsplit('\n').next()?;
        let v: serde_json::Value = serde_json::from_str(last.strip_prefix(MARK)?).ok()?;
        let n = |k: &str| v.get(k)?.as_u64().map(|x| x as usize);
        let s = |k: &str| v.get(k)?.as_str().map(str::to_string);
        let mut spans = Vec::new();
        for sp in v.get("spans")?.as_array()? {
            let a = sp.as_array()?;
            let (start, end) = (a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize);
            let tail_len = v.get("tail")?.as_str()?.len();
            if start > end || end > tail_len {
                return None;
            }
            spans.push(Span {
                start,
                end,
                author: author_of(a.get(2)?.as_str()?)?,
                row: a.get(3).and_then(|r| r.as_u64()).map(|r| r as usize),
            });
        }
        Some(Kept {
            job: Job::parse(&s("job")?)?,
            turn: n("turn")?,
            upto: n("upto")?,
            last: s("last"),
            tail: s("tail")?,
            reply: s("reply")?,
            spans,
        })
    }

    /// Whether `log`, refed after a restart, holds the rows this turn was sent at the same indexes.
    pub fn matches(&self, log: &Log) -> Result<(), String> {
        if self.upto > log.end() {
            return Err(format!(
                "the record reached fed row {}, the watched rows give {}",
                self.upto,
                log.end()
            ));
        }
        let Some(want) = &self.last else {
            return Ok(());
        };
        if self.upto <= log.start() {
            return Err(format!(
                "fed row {} is no longer held to check against",
                self.upto - 1
            ));
        }
        match log.chain_at(self.upto - 1) {
            Some(h) if h == *want => Ok(()),
            _ => Err(format!(
                "the watched rows up to fed row {} differ from the ones the record was sent",
                self.upto - 1
            )),
        }
    }
}

/// `job`'s kept turns since its conversation last began, in order, from the shadow's row bodies.
/// A gap in the turn numbers (a row that was not written) means the history cannot be rebuilt as
/// sent, so nothing is resumed until the next turn 1.
pub fn kept_of<'a>(bodies: impl IntoIterator<Item = &'a str>, job: Job) -> Vec<Kept> {
    let mut out: Vec<Kept> = Vec::new();
    let mut broken = false;
    for k in bodies
        .into_iter()
        .filter_map(Kept::parse)
        .filter(|k| k.job == job)
    {
        if k.turn == 1 {
            out.clear();
            broken = false;
        } else if k.turn != out.len() + 1 {
            broken = true;
        }
        out.push(k);
    }
    if broken { Vec::new() } else { out }
}
