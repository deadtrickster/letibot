//! The fabric, as a block in the system prompt: what skills are on the shelf
//! and what memories exist — so the model knows what it could load, and loads
//! a body only when it needs one.
//!
//! Given by the operator 2026-09-14: *"skills are summaries of full pages,
//! memories are titles."* A skill's summary is its first paragraph; a memory
//! is its title and id. Both are cheap in tokens and both are pointers — the
//! `skill` tool and `flowy get` are the doors to the rest.
//!
//! # Offline is a state, not an empty block
//!
//! `docs/tool-design-brief.md` §3b keeps a no-flowy mode, and closed loop §5
//! says a lost encoder is declared, not papered over. So the block comes from
//! one of three places and says which: the node, read just now; the last copy
//! cached on disk, with its age; or nowhere, in which case the block says the
//! fabric is unreachable and no copy exists. A session with no seat has no
//! block at all, and the disclosure says `fabric: OFF`.
//!
//! # When it is read
//!
//! At session open, into the system prompt — message 0, which is never
//! rewritten. After a compaction, or on a resume, a fresh reading that differs
//! from the last one goes in as a **system update** (§5.3: appended, never
//! rewritten), so a stale block is corrected without a cold re-prefill.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::{Artifact, Node, NodeError};
use crate::seat::rfc3339_now;

/// How many of each to carry. A shelf of seven fits whole; a memory list of
/// hundreds does not, and the newest are the ones a session is likeliest to
/// want.
pub const MAX_SKILLS: usize = 40;
pub const MAX_MEMORIES: usize = 60;
/// A skill's summary: its first paragraph, cut here.
pub const SUMMARY_CHARS: usize = 220;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLine {
    pub id: String,
    pub title: String,
    pub visibility: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryLine {
    pub id: String,
    pub title: String,
    pub visibility: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabricContext {
    pub seat: String,
    pub project: String,
    pub read_at: String,
    pub skills: Vec<SkillLine>,
    pub memories: Vec<MemoryLine>,
}

/// Where a block came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Live,
    Cached { read_at: String, why: String },
    Unreachable { why: String },
}

impl FabricContext {
    /// Read the shelf and the memories through the node. The skill bodies are
    /// fetched for their first paragraph; a shelf is small.
    pub fn fetch(node: &Node, seat: &str, project: &str) -> Result<FabricContext, NodeError> {
        let mut skills = Vec::new();
        for a in node.artifacts_of_kind("skill", MAX_SKILLS)? {
            let body = if a.body.is_empty() {
                node.artifact(&a.id)
                    .map(|full| full.body)
                    .unwrap_or_default()
            } else {
                a.body.clone()
            };
            skills.push(SkillLine {
                id: a.id,
                title: a.title,
                visibility: a.visibility,
                summary: first_paragraph(&body),
            });
        }
        let memories = node
            .artifacts_of_kind("note", MAX_MEMORIES)?
            .into_iter()
            .map(|a: Artifact| MemoryLine {
                id: a.id,
                title: a.title,
                visibility: a.visibility,
            })
            .collect();
        Ok(FabricContext {
            seat: seat.to_string(),
            project: project.to_string(),
            read_at: rfc3339_now(),
            skills,
            memories,
        })
    }

    /// The block, as the model reads it.
    pub fn render(&self, source: &Source) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "## The fabric — seat `{}`, project {}\n",
            self.seat, self.project
        ));
        match source {
            Source::Live => s.push_str(&format!("Read from the node at {}.\n", self.read_at)),
            Source::Cached { read_at, why } => s.push_str(&format!(
                "THE NODE IS UNREACHABLE ({why}); this is the copy cached at {read_at} and may be \
                 stale. Nothing here can be loaded until the node is back.\n"
            )),
            Source::Unreachable { .. } => {}
        }
        s.push_str(&format!(
            "\nSkills on the shelf ({}) — `skill load <id or title>` reads one in full:\n",
            self.skills.len()
        ));
        for k in &self.skills {
            s.push_str(&format!("- {} [{}] {}", k.id, k.visibility, k.title));
            if !k.summary.is_empty() {
                s.push_str(&format!(" — {}", k.summary));
            }
            s.push('\n');
        }
        s.push_str(&format!(
            "\nMemories ({}) — titles only; `flowy get /api/artifact/<id> --jq .body` for one:\n",
            self.memories.len()
        ));
        for m in &self.memories {
            s.push_str(&format!("- {} [{}] {}\n", m.id, m.visibility, m.title));
        }
        s
    }

    /// The block for a seat with no reachable node and no cache.
    pub fn render_unreachable(seat: &str, why: &str) -> String {
        format!(
            "## The fabric — seat `{seat}`\nTHE NODE IS UNREACHABLE ({why}) and no cached copy \
             of the shelf exists on this box. Skills and memories on the fabric are unknown \
             here until it is back; nothing above is a claim that there are none.\n"
        )
    }

    // ---- the cache --------------------------------------------------------

    pub fn cache_path(dir: Option<&Path>, seat: &str) -> Result<PathBuf, std::io::Error> {
        let dir = match dir {
            Some(d) => d.to_path_buf(),
            None => {
                let base = std::env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|h| PathBuf::from(h).join(".local").join("state"))
                    })
                    .ok_or_else(|| {
                        std::io::Error::other("neither XDG_STATE_HOME nor HOME is set")
                    })?;
                base.join("letibot").join("flowy")
            }
        };
        std::fs::create_dir_all(&dir)?;
        let safe: String = seat
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        Ok(dir.join(format!("{safe}-fabric.json")))
    }

    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn load(path: &Path) -> Option<FabricContext> {
        let raw = std::fs::read(path).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Live if the node answers (and the copy is refreshed on disk), else the
    /// cached copy labelled with its age, else nothing. One call, three honest
    /// outcomes.
    pub fn read(
        node: &Node,
        seat: &str,
        project: &str,
        cache_dir: Option<&Path>,
    ) -> (Option<FabricContext>, Source) {
        let path = Self::cache_path(cache_dir, seat).ok();
        match Self::fetch(node, seat, project) {
            Ok(ctx) => {
                if let Some(p) = &path {
                    let _ = ctx.save(p);
                }
                (Some(ctx), Source::Live)
            }
            Err(e) => {
                let why = e.to_string();
                match path.as_deref().and_then(Self::load) {
                    Some(ctx) => {
                        let read_at = ctx.read_at.clone();
                        (Some(ctx), Source::Cached { read_at, why })
                    }
                    None => (None, Source::Unreachable { why }),
                }
            }
        }
    }
}

/// The first paragraph that is not a heading, cut to [`SUMMARY_CHARS`] on a
/// word boundary with an ellipsis. Markdown emphasis is left alone; it costs
/// nothing and a summary that reads like the page is easier to match to it.
pub fn first_paragraph(body: &str) -> String {
    let mut para = String::new();
    for block in body.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || block.starts_with('#') || block.starts_with("---") {
            continue;
        }
        para = block.split_whitespace().collect::<Vec<_>>().join(" ");
        break;
    }
    if para.chars().count() <= SUMMARY_CHARS {
        return para;
    }
    let mut cut: String = para.chars().take(SUMMARY_CHARS).collect();
    if let Some(sp) = cut.rfind(' ') {
        cut.truncate(sp);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_is_the_first_real_paragraph_cut_on_a_word() {
        let body = "# Title\n\n---\n\nEvery rule here was paid for. Each one names the day.\n\nSecond para.";
        assert_eq!(
            first_paragraph(body),
            "Every rule here was paid for. Each one names the day."
        );
        let long = "word ".repeat(100);
        let s = first_paragraph(&long);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= SUMMARY_CHARS + 1);
        assert_eq!(first_paragraph("# only a heading"), "");
    }

    #[test]
    fn the_block_names_its_source_and_the_doors_to_the_rest() {
        let ctx = FabricContext {
            seat: "seat".into(),
            project: "Lab".into(),
            read_at: "2026-09-14T11:00:00Z".into(),
            skills: vec![SkillLine {
                id: "01S".into(),
                title: "cookbook".into(),
                visibility: "shared".into(),
                summary: "what to type".into(),
            }],
            memories: vec![MemoryLine {
                id: "01M".into(),
                title: "r10 data is invalid".into(),
                visibility: "project".into(),
            }],
        };
        let live = ctx.render(&Source::Live);
        assert!(live.contains("Read from the node at 2026-09-14T11:00:00Z"));
        assert!(live.contains("- 01S [shared] cookbook — what to type"));
        assert!(live.contains("- 01M [project] r10 data is invalid"));
        assert!(live.contains("`skill load <id or title>`"));
        let cached = ctx.render(&Source::Cached {
            read_at: "2026-09-13T00:00:00Z".into(),
            why: "node unreachable: refused".into(),
        });
        assert!(cached.contains("UNREACHABLE (node unreachable: refused); this is the copy cached at 2026-09-13T00:00:00Z"));
        assert!(FabricContext::render_unreachable("seat", "down").contains("no cached copy"));
    }

    #[test]
    fn the_cache_round_trips() {
        let d = std::env::temp_dir().join(format!("letibot-fabric-{}", std::process::id()));
        let p = FabricContext::cache_path(Some(&d), "a/b").unwrap();
        assert!(p.ends_with("a-b-fabric.json"));
        let ctx = FabricContext {
            seat: "a/b".into(),
            project: "Lab".into(),
            read_at: "t".into(),
            skills: vec![],
            memories: vec![],
        };
        ctx.save(&p).unwrap();
        assert_eq!(FabricContext::load(&p).unwrap(), ctx);
    }
}
