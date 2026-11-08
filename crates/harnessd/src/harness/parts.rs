//! What the daemon holds once and every session borrows: vocabulary, wiring, mode store, journals and review desk.
//! Apart from [`Harness`](crate::harness::Harness) because `TurnEngine` borrows the vocabulary and renderer, and one struct owning both would be self-referential.

use super::*;

/// Everything the engine borrows for the life of the daemon. Held apart from [`Harness`] because
/// `TurnEngine<'a>` borrows the vocabulary and the renderer, and a struct owning both the borrow and
/// the borrowed would be self-referential. Every field is an `Arc`, so a clone is a few refcounts.
#[derive(Clone)]
pub struct Parts {
    pub vocab: std::sync::Arc<Vocab>,
    pub wiring: std::sync::Arc<Wiring>,
    /// Per-project mode store, loaded once at daemon start: a session's point is its project's row
    /// (longest ancestor wins), not the daemon's `--mode` flag. Locked because `/mode` writes it
    /// while session opens read it.
    pub mode_store: std::sync::Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// The subagent (task) journal, shared with every subagent runner so spawns and
    /// finishes land in one place and reach the dashboard's state file.
    pub tasks: std::sync::Arc<crate::tasks::TaskJournal>,
    /// The language servers this daemon can reach, shared with the `lsp` tool and
    /// the dashboard's `lsp` panel.
    pub lsp: std::sync::Arc<letibot_tools::builtins::lsp::LspConfig>,
    /// The loaded skills, shared with the `skill` tool and the dashboard's `skills`
    /// panel.
    pub skills: std::sync::Arc<letibot_tools::builtins::skill::SkillRegistry>,
    /// Every live todo board this daemon holds, by session id: the map a parent's `todo_write` with
    /// a `target` resolves its child through. It rides [`Parts`] because it belongs to the daemon,
    /// and the store path rides along because the resolver's flush runs on another thread.
    pub child_boards: std::sync::Arc<crate::child_todos::ChildBoards>,
    /// The watcher set of the subagent TREE this session belongs to, when it is a child.
    /// `None` for a root, whose set is its own; a child carries its parent's, so the tree shares the
    /// watching bookkeeping, the level above and the ROOT a permission card can be drawn by.
    pub tree_watch: Option<std::sync::Arc<crate::jobwatch::JobWatchers>>,
    /// The subagent handle list a child of this session shares: `None` for a root, `Some` for a
    /// child, so the whole tree hands out and collects one list. Without it a settlement names a
    /// handle no runner in `open` holds.
    pub(crate) tree_slots: Option<TaskSlots>,
    /// The head's answers, as the whole tree reaches them: a child has no head, so its gate's card is
    /// posted to the tree's ROOT and settled by that session's `Answers`, or nothing could answer it.
    /// `None` for a root, whose own are built at open.
    pub tree_head: Option<std::sync::Arc<crate::answers::Answers>>,
    /// The daemon's review desk: who is reviewing what, and what they have handed in. It rides
    /// [`Parts`] because a review has two sides in two sessions, and one `Arc` is what makes them one
    /// act. Not a store: the durable row is the queue's own.
    pub reviews: std::sync::Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    /// The daemon's Claude Code reviewers (`claude -p`, R116), when a project's `[queue.claude]`
    /// makes one. Here for [`Parts::reviews`]'s reason: the queue's thread starts a run and the
    /// review pool polls it in a later pass, so both have to hold the same table of processes.
    pub claude: std::sync::Arc<crate::mergequeue::review::claude::Runs>,
    /// Every open session's own watcher set and slots, by id - see [`trees::Trees`]. Here
    /// because the daemon's worker runs a PARKED child's later turn while the slot it must settle
    /// is its parent's, on a thread the worker cannot reach.
    pub(crate) trees: super::trees::Trees,
}

impl Parts {
    pub fn load(cfg: &Config) -> Result<Parts, HarnessError> {
        let wiring = cfg.dialect.wiring(cfg.effort.as_deref());
        // Which vocabulary, decided by what the turns need: a GGUF is read by llama.cpp, and no GGUF
        // with a provider means the byte vocabulary, whose ids never leave this machine. No GGUF and no
        // provider is the same byte vocabulary and no refusal: a turn on the local route is what is.
        let vocab = match &cfg.vocab_gguf {
            Some(path) => {
                if !path.is_file() {
                    return Err(HarnessError::Setup(format!(
                        "no vocabulary GGUF at {}. For a split model, pass the first shard.",
                        path.display()
                    )));
                }
                load_gguf(path)
                    .map_err(|e| HarnessError::Setup(format!("loading the vocabulary: {e}")))?
            }
            None => {
                let spec = wiring.spec();
                Vocab::bytes(
                    spec.control_tokens
                        .as_slice()
                        .iter()
                        .map(|t| t.literal.as_ref())
                        .chain(spec.stop_tokens.iter().map(|t| t.literal.as_ref())),
                    spec.stop_tokens.iter().map(|t| t.literal.as_ref()),
                )
            }
        };
        let skills = std::sync::Arc::new(
            letibot_tools::builtins::skill::SkillRegistry::load_for(&cfg.skill_dirs())
                .with_pins(cfg.skills_pin().iter().cloned()),
        );
        let lsp = std::sync::Arc::new(letibot_tools::builtins::lsp::LspConfig::default());
        let tasks = std::sync::Arc::new(crate::tasks::TaskJournal::new(
            crate::tasks::default_state_path(),
            lsp.clone(),
            skills.clone(),
        ));
        Ok(Parts {
            vocab: std::sync::Arc::new(vocab),
            wiring: std::sync::Arc::new(wiring),
            mode_store: std::sync::Arc::new(std::sync::RwLock::new(crate::modes::ModeStore::open(
                cfg.store.as_deref(),
            ))),
            tasks,
            lsp,
            skills,
            // The daemon-wide board map, over the same store the sessions write - see the
            // field's doc for why it is here and not on a harness.
            child_boards: crate::child_todos::ChildBoards::shared(cfg.store.clone()),
            // A root session is its own tree; it has no parent's watcher set to join.
            tree_watch: None,
            // Nor a parent's handle list: a root's runner gets a fresh one.
            tree_slots: None,
            // Nor a parent's head answers: a root's own are built as its gate is, and a
            // root with no head-wired adjudicator has none to pass down. See `tree_head`.
            tree_head: None,
            // The daemon's review desk, empty: the queue's session opens a review on it when it
            // starts a gatekeeper, and the child hands its verdict back through the same one.
            reviews: std::sync::Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new()),
            // Empty too, and swapped for the queue's own by the daemon that starts one (see where
            // `parts.reviews` is set): no Claude reviewer runs until a project asks for one.
            claude: std::sync::Arc::new(crate::mergequeue::review::claude::Runs::new()),
            // Empty: every harness puts its own entry in as it opens.
            trees: super::trees::Trees::default(),
        })
    }
}
