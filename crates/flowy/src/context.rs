//! The fabric as a block in the system prompt: skills on the shelf and memories that exist.
//! Skills are summaries and memories are titles, since bodies cost tokens. A door is named only when
//! the session has it ([`Reach`]). Offline is a state: the block names its source.

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

/// Which doors to the rest the session reading the block actually has. A pointer
/// is only worth printing with the way to follow it: a seat told to use a door it
/// was not given is a prompt naming a tool the model does not have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reach {
    /// The `skill` tool is seated: a shelf skill loads by id or title.
    pub skill: bool,
    /// The `flowy` tool is seated: a memory's body is its `artifact` action away.
    pub flowy: bool,
    /// `bash` is seated: the `flowy` CLI's `get` reads a body too, for a seat
    /// that has no tool action.
    pub bash: bool,
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

    /// The block, as the model reads it - naming only the doors in `reach`.
    pub fn render(&self, source: &Source, reach: Reach) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "## The fabric — seat `{}`, project {}\n",
            self.seat, self.project
        ));
        match source {
            // No clock: a live read happened now, and a time here changed the whole system
            // prompt at every open, which made every seated root read as stale after a restart.
            Source::Live => s.push_str("Read from the node just now.\n"),
            Source::Cached { read_at, why } => s.push_str(&format!(
                "THE NODE IS UNREACHABLE ({why}); this is the copy cached at {read_at} and may be \
                 stale. Nothing here can be loaded until the node is back.\n"
            )),
            Source::Unreachable { .. } => {}
        }
        let load = if reach.skill {
            " — `skill` with `action` \"load\" and `name` (an id or title) reads one in full"
        } else {
            ""
        };
        s.push_str(&format!(
            "\nSkills on the shelf ({}){load}:\n",
            self.skills.len()
        ));
        for k in &self.skills {
            s.push_str(&format!("- {} [{}] {}", k.id, k.visibility, k.title));
            if !k.summary.is_empty() {
                s.push_str(&format!(" — {}", k.summary));
            }
            s.push('\n');
        }
        let read = if reach.flowy {
            "; read one with `flowy`, `action` \"artifact\" and its id"
        } else if reach.bash {
            "; read one with the `flowy` CLI through `bash`: `flowy get /api/artifact/<id> --jq .body`"
        } else {
            ""
        };
        s.push_str(&format!(
            "\nMemories ({}) — titles only{read}:\n",
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

/// The first paragraph that is not a heading, a blockquote or an HTML comment,
/// cut to [`SUMMARY_CHARS`] on a word boundary with an ellipsis. Markdown
/// emphasis is left alone: it costs nothing and reads like the page.
pub fn first_paragraph(body: &str) -> String {
    let mut para = String::new();
    for block in body.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || block.starts_with('#') || block.starts_with("---") {
            continue;
        }
        // A blockquote: every non-empty line of the block opens with '>'.
        if block
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.starts_with('>'))
        {
            continue;
        }
        // An HTML comment and nothing else: opens `<!--`, closes `-->`.
        if block.starts_with("<!--") && block.ends_with("-->") {
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
    fn the_summary_skips_a_blockquote() {
        let body = "# Title\n\n> Quoted from another page.\n> Still the quote.\n\nThe first real paragraph.";
        assert_eq!(first_paragraph(body), "The first real paragraph.");
        // One plain line and the block is prose, not a quote - kept whole.
        assert_eq!(
            first_paragraph("> Quoted.\nA plain line."),
            "> Quoted. A plain line."
        );
    }

    #[test]
    fn the_summary_skips_an_html_comment() {
        let body = "<!-- draft: not for the summary -->\n\nThe first real paragraph.";
        assert_eq!(first_paragraph(body), "The first real paragraph.");
        // A comment may run over several lines and still be only a comment.
        assert_eq!(
            first_paragraph("<!-- one\n two -->\n\nProse after the comment."),
            "Prose after the comment."
        );
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
        let both = Reach {
            skill: true,
            flowy: true,
            bash: true,
        };
        let live = ctx.render(&Source::Live, both);
        // No clock in a live block: the read happened now, and a time here changed the whole
        // system prompt at every open (R93 item 8).
        assert!(live.contains("Read from the node just now."), "{live}");
        assert!(!live.contains("2026-09-14T11:00:00Z"), "{live}");
        assert!(live.contains("- 01S [shared] cookbook — what to type"));
        assert!(live.contains("- 01M [project] r10 data is invalid"));
        assert!(live.contains("`skill` with `action` \"load\""));
        // The seated tool beats the CLI: a seat with `flowy` reads a body without a shell.
        assert!(
            live.contains("`flowy`, `action` \"artifact\"") && !live.contains("CLI"),
            "{live}"
        );
        // A door the seat lacks is not named: no `skill`, and no way to read a body.
        let bare = ctx.render(&Source::Live, Reach::default());
        assert!(!bare.contains("`skill`"), "{bare}");
        assert!(
            !bare.contains("bash") && !bare.contains("flowy get") && !bare.contains("`artifact`"),
            "{bare}"
        );
        assert!(bare.contains("- 01M [project] r10 data is invalid"));
        // A seat with only `bash` keeps the CLI sentence it had.
        let shell = ctx.render(
            &Source::Live,
            Reach {
                bash: true,
                ..Reach::default()
            },
        );
        assert!(shell.contains("`flowy` CLI through `bash`"), "{shell}");
        let cached = ctx.render(
            &Source::Cached {
                read_at: "2026-09-13T00:00:00Z".into(),
                why: "node unreachable: refused".into(),
            },
            both,
        );
        assert!(cached.contains("UNREACHABLE (node unreachable: refused); this is the copy cached at 2026-09-13T00:00:00Z"));
        assert!(FabricContext::render_unreachable("seat", "down").contains("no cached copy"));
    }

    /// The live block is the same string at every open, whatever the clock says: it is part of the
    /// system prompt, and a session's prompt is compared with the one this daemon composes now.
    #[test]
    fn an_open_time_does_not_change_the_live_block() {
        let at = |t: &str| FabricContext {
            seat: "seat".into(),
            project: "Lab".into(),
            read_at: t.into(),
            skills: Vec::new(),
            memories: Vec::new(),
        };
        assert_eq!(
            at("2026-09-14T11:00:00Z").render(&Source::Live, Reach::default()),
            at("2026-10-12T03:04:05Z").render(&Source::Live, Reach::default()),
            "a restarted daemon composes the block again, and the two must be one prompt"
        );
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
