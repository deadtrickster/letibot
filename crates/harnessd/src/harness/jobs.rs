//! **The jobs and tools this session reports**: the table a head draws, the rows `/job` and
//! `/tools` print, and the live output read a pane is answered from.
use super::*;

impl Harness {
    /// The jobs this session has started, one line each, for `/job` with no
    /// argument. The pane draws the same facts; this is for reading them without
    /// leaving the composer, and for finding an id to pass to `/job ID`.
    /// **One row, one line.** A command's text is whatever the model wrote, and a
    /// heredoc carries newlines inside it: `cd … && python3 - <<'PY'\ns = open("…`.
    /// Truncating that to 60 *characters* keeps the newline, so a row meant to be one
    /// line became two, and a listing of many of them became unreadable — the second
    /// half of what the operator saw on 2026-09-20.
    ///
    /// Every run of whitespace becomes one space before the cut, so the width the
    /// caller asks for is the width it gets. The ellipsis is added only when
    /// something was actually removed, so a short command is not decorated with a
    /// promise of more.
    pub(crate) fn one_line(command: &str, width: usize) -> String {
        let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() <= width {
            return flat;
        }
        let cut: String = flat.chars().take(width).collect();
        format!("{cut}…")
    }

    /// **Which of the host's jobs a person would act on.**
    ///
    /// Every command runs in a job scope, so `ProcessHost::jobs()` is the whole
    /// session's process history. The two kinds worth a row are the ones
    /// somebody can still do something about: a job that was put in the
    /// background — `background` is `Some` exactly then, by its own doc — and
    /// anything still running, which is what ctrl-o moves and what an operator
    /// might want to interrupt, whether or not it was ever backgrounded.
    ///
    /// A finished foreground `grep` is neither, and three hundred of them are
    /// what `/job` printed before this existed.
    pub(crate) fn worth_listing(j: &letibot_tools::exec::JobView) -> bool {
        j.background.is_some() || j.state.is_running()
    }

    /// **The jobs, as data, for any head that asks.**
    ///
    /// The same list `job_lines` prints, before it is prose. Every decision in it
    /// is the daemon's: which jobs are worth listing (`worth_listing`), what the
    /// command reads as (`one_line`), what the state word is. A head renders
    /// these rows; it does not rebuild them out of the event stream, which is how
    /// a job that outlived its turn used to lose its name and how each head came
    /// to need its own copy of the rules. The operator, 2026-09-20: *"regarding
    /// jobs, subagents, etc, i expect them to be handled by harnessd not the
    /// heads"*.
    pub fn job_entries(&self) -> Vec<letibot_sessionlog::protocol::JobEntry> {
        let Some(host) = self.runtime.backend.processes() else {
            return Vec::new();
        };
        host.jobs()
            .iter()
            .filter(|j| Self::worth_listing(j))
            .map(Self::job_entry)
            .collect()
    }

    /// **One job as the wire carries it.** Pure, so the row can be asserted without a host:
    /// what this exists to pin is the name, and the honest emptiness of a job nobody named.
    pub(crate) fn job_entry(
        j: &letibot_tools::exec::JobView,
    ) -> letibot_sessionlog::protocol::JobEntry {
        letibot_sessionlog::protocol::JobEntry {
            id: j.id.0.clone(),
            command: Self::one_line(&j.command, 120),
            // **The name the caller stated, or nothing.** Never derived here either: this is
            // the daemon's listing, and a name it invented would be the same defect one
            // layer up from the tool.
            slug: j.slug.clone().unwrap_or_default(),
            how: j
                .background
                .as_ref()
                .map(|b| b.phrasing())
                .unwrap_or_default(),
            state: j.state.word(),
            running: j.state.is_running(),
            never_ran: j.state.never_ran(),
            // **Read out of the COMMAND, not out of what has been captured** (R41). Nothing
            // has to run to know it, and the answer does not change while the job does — so a
            // reader learns which of their running jobs cannot be watched before they open
            // the pane, rather than by finding an empty window and guessing why.
            redirect: letibot_tools::builtins::output_redirect_path(&j.command),
            produced: j.produced,
            elapsed_ms: j
                .ran_for
                .unwrap_or(j.elapsed)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        }
    }

    /// **Install this session's live job-output read**, which is what a head's `ReadJobOutput`
    /// is answered from while the worker is busy with a turn
    /// (`letibot_sessionlog::registry::JobOutputSource`).
    ///
    /// It holds the process host and nothing else of the harness, so it reads exactly what
    /// [`Harness::job_output_window`] reads for a job the host holds — the same
    /// [`job_window_for`] decision over the same two reads. A job the host does not hold
    /// answers `None` and the read is queued, because that answer comes from the store.
    pub(crate) fn install_job_output_source(&self) {
        let Some(host) = self.runtime.backend.processes_arc() else {
            return;
        };
        self.session_registry.set_job_output_source(
            &self.cfg.session_id,
            Arc::new(move |job, offset| {
                live_job_output(host.as_ref(), job, offset, JOB_OUTPUT_WINDOW)
            }),
        );
    }

    /// Push the job table to the registry, where a head's `ListJobs` is answered
    /// from. Called wherever the table can have changed, for the same reason
    /// `publish_settings` is: a mailbox nobody refills is a stale answer that
    /// looks like a current one.
    pub fn publish_jobs(&self) {
        self.session_registry
            .set_jobs(&self.cfg.session_id, self.job_entries());
        // **And the monitors, on the same refill** — the monitors pane's rows, so a head that
        // asks after a round, a firing or a retire reads this moment's table.
        self.session_registry
            .set_monitors(&self.cfg.session_id, self.monitor_entries());
    }

    /// **This session's monitors as the wire carries them**: every live one, then the ended ones
    /// newest last ([`MONITORS_ENDED_SHOWN`] of them), each with how it ended. A continuous monitor
    /// is one row however often it fired, with the count.
    pub fn monitor_entries(&self) -> Vec<letibot_sessionlog::protocol::MonitorEntry> {
        let Some(monitors) = self.monitors.as_ref() else {
            return Vec::new();
        };
        monitor_entries_of(
            &monitors.list(),
            &monitors.firings(),
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// **One job, as `/job` draws it** — `release-build · j65  running  4096 bytes  cargo test`,
    /// and `j65  running  …` for a job nobody named.
    ///
    /// One function so the listing and the pane's own rows cannot spell a name two ways, and
    /// so the row is assertable without a host. The id is always drawn — it is the handle
    /// `job_output` and `/job ID` take — and a name goes in front of it, which is where a
    /// reader's eye lands first.
    pub(crate) fn job_row(j: &letibot_tools::exec::JobView) -> String {
        format!(
            "  {}  {}  {} bytes  {}",
            Self::job_name(j),
            j.state.word(),
            j.produced,
            Self::one_line(&j.command, 60)
        )
    }

    /// **What a job is called on a row**: `release-build · j65`, or just `j65` when nobody
    /// named it.
    ///
    /// The rule itself is [`letibot_tools::exec::JobView::label`] — one spelling for the
    /// daemon's listing, the `JobEntry` a head draws and a tool's own refusal — and the id
    /// is never dropped: a job with no name draws exactly what it drew before this existed.
    pub(crate) fn job_name(j: &letibot_tools::exec::JobView) -> String {
        j.label()
    }

    pub fn job_lines(&self) -> Vec<String> {
        let Some(host) = self.runtime.backend.processes() else {
            return vec!["this session has no process host, so it has no jobs".into()];
        };
        let all = host.jobs();
        // **The background ones, and whatever is still running.**
        //
        // `host.jobs()` is every process this session ever spawned — the host
        // gives each one a job id and a scope, which is how a turn-scoped reap
        // finds them. That is right for the host and wrong for this listing: the
        // operator ran `/job` after a turn and got **three hundred lines** of
        // finished `grep`, `cat` and `sed` calls, one per command the model had
        // run all session. Their words, 2026-09-20: *"some fuckery with jobs"*,
        // *"hundreds of lines"*.
        //
        // `background: Option<Backgrounding>` is the field that already draws
        // this distinction — its own doc says "or `None` if it never was" — and
        // a still-running job is kept whether or not anybody backgrounded it,
        // because that is the one ctrl-o moves and the one worth interrupting.
        // The empty-case sentence below has always said this listing is about
        // background jobs; now it is.
        let jobs: Vec<_> = all.iter().filter(|j| Self::worth_listing(j)).collect();
        if jobs.is_empty() {
            return vec!["no jobs. The model backgrounds a command with bash's `background: true`; ctrl-o moves the running one.".into()];
        }
        let mut out = vec![format!("{} job(s) this session has started.", jobs.len())];
        out.push(String::new());
        for j in &jobs {
            out.push(Self::job_row(j));
        }
        out.push(String::new());
        // The ones left out are counted, not hidden: a filtered listing that does
        // not say it filtered is a listing the reader draws wrong conclusions from.
        let hidden = all.len() - jobs.len();
        if hidden > 0 {
            out.push(format!(
                "{hidden} finished foreground command(s) not shown — every command runs \
                 in a job scope, and only the backgrounded and the still-running are \
                 jobs anybody acts on."
            ));
        }
        out.push("`/job ID` reads what one wrote.".into());
        out
    }

    /// **A background job's retained output, for the operator not the model.**
    ///
    /// The jobs pane lists what is running and how many bytes it produced, and
    /// until now that was all: the only way to read the bytes it was counting was
    /// to ask the model to call `job_output`. The operator, looking straight at
    /// the row: *"i go to jobs panel and no way to get job output"*.
    ///
    /// The same read `job_output` does — `ProcessHost::output` over the capture
    /// ring — reached by a slash reply rather than a tool call, so it needs no new
    /// protocol frame and no version bump.
    pub fn job_output(&self, job: &str, offset: u64, limit: usize) -> Result<Vec<String>, String> {
        let Some(host) = self.runtime.backend.processes() else {
            return Err("this session has no process host, so it has no jobs".into());
        };
        let jid = letibot_tools::exec::JobId(job.to_string());
        let Some(view) = host.job(&jid) else {
            return Err(format!(
                "no job `{job}` here; `/job` with no argument lists them"
            ));
        };
        let slice = host
            .output(&jid, offset, limit)
            .map_err(|e| e.to_string())?;

        // A job that has written nothing is not an empty answer about its output:
        // whether it is still running decides what the silence means. The same
        // distinction `job_output` draws, in the same words.
        // **A job whose output was redirected has, and its window must say so** — R41's
        // case, which the MODEL's `job_output` already answers
        // (`letibot_tools::builtins::jobs`) and which this path still got wrong: the head
        // read *"wrote nothing at all"* about a build that wrote megabytes to a file.
        if slice.produced == 0
            && let Some(path) = letibot_tools::builtins::output_redirect_path(&view.command)
        {
            return Ok(vec![format!(
                "`{job}`'s output goes to `{path}`, not to its window — the capture is empty \
                 by construction ({}). Read the file, or `tail -n` it.",
                view.state.word()
            )]);
        }
        if slice.produced == 0 {
            return Ok(vec![if view.state.is_running() {
                format!("`{job}` is still running and has written nothing yet.")
            } else {
                format!("`{job}` {} and wrote nothing at all.", view.state.word())
            }]);
        }

        let mut out: Vec<String> = progress_lines(&slice.text());
        out.push(String::new());
        out.push(format!(
            "[{} — bytes {}..{} of {} produced{}]",
            view.state.word(),
            slice.from,
            slice.to,
            slice.produced,
            if slice.dropped > 0 {
                format!(", {} dropped off the front", slice.dropped)
            } else {
                String::new()
            }
        ));
        if slice.to < slice.produced {
            out.push(format!("more: /job {job} --offset {}", slice.to));
        }
        Ok(out)
    }

    /// **What this conversation can actually call**, for `/tools`.
    ///
    /// Two lists, and the gap between them is the point. The REGISTRY is what this
    /// daemon seated when it opened the session; the PROMPT is what message zero
    /// announces, and it is fixed the moment a transcript starts — a resume
    /// replays the stored prefix because the stored tokens were produced under it.
    ///
    /// So a daemon restarted with a new tool has it seated and unannounced, and the
    /// model cannot call a tool it has never been told about however the banner
    /// reads. That gap cost the operator an hour once — *"i did letibot --stop and
    /// leticode --continue but still no exec"* — and the banner was no help,
    /// because the banner is computed from the registry.
    ///
    /// This names it in the one place somebody would look.
    pub fn tools_lines(&self) -> Vec<String> {
        let schemas = self.runtime.registry.schemas();
        let announced = tool_names(&self.prefix.tools_json);
        let seated: std::collections::BTreeSet<String> =
            schemas.iter().map(|s| s.name.clone()).collect();

        let mut out = vec![format!("{} tool(s) seated in this session.", schemas.len())];
        out.push(String::new());
        let mut by_name = schemas;
        by_name.sort_by(|a, b| a.name.cmp(&b.name));
        for s in &by_name {
            // A tool the prompt has never heard of is marked where the eye already
            // is, rather than only in a footnote below the list.
            let mark = if announced.contains(&s.name) {
                "  "
            } else {
                "! "
            };
            out.push(format!(
                "{mark}{:<14} {:<8} {}",
                s.name,
                format!("({})", s.access.as_str()),
                first_sentence(&s.description),
            ));
        }
        out.push(String::new());

        let missing: Vec<String> = seated.difference(&announced).cloned().collect();
        let stale: Vec<String> = announced.difference(&seated).cloned().collect();
        if missing.is_empty() && stale.is_empty() {
            out.push(
                "The prompt this conversation speaks announces exactly these, so \
                 everything seated is callable."
                    .into(),
            );
            return out;
        }
        if !missing.is_empty() {
            out.push(format!(
                "! {} seated but NOT ANNOUNCED: {}.",
                missing.len(),
                missing.join(", ")
            ));
            out.push(
                "  Message zero is fixed when a transcript starts, so a tool seated \
                 after this conversation began is one the model has never been told \
                 about — it cannot call it, whatever the banner says."
                    .into(),
            );
            out.push(
                "  `/compact` now forks onto the seated prompt and picks them up; \
                 `/reseat` does the same without waiting for the context to fill, and \
                 without summarising."
                    .into(),
            );
        }
        if !stale.is_empty() {
            out.push(format!(
                "! {} announced but NO LONGER SEATED: {}. The model may call these and \
                 will be told the tool is unknown.",
                stale.len(),
                stale.join(", ")
            ));
        }
        out
    }

    /// **The jobs still running in this session's VM as it is closed** — which die with it,
    /// unannounced until this (TODO.md R92.1, second half). A VM child's backend is closed right
    /// after its first turn (`HarnessTaskRunner::run`); a job it backgrounded and did not wait on
    /// was killed with the guest, its watcher stopped first, so no settlement ever came and its
    /// row read `running` for ever.
    ///
    /// Called just before [`Harness::close_backend`]: returns the jobs, and writes each one's row
    /// as `lost with its VM` (and delivered — the parent is told, in the answer) so the store does
    /// not remind anyone of it after a restart. Empty when the backend runs no processes.
    pub fn jobs_lost_with_the_vm(&self) -> Vec<letibot_tools::exec::JobView> {
        let Some(host) = self.runtime.backend.processes() else {
            return Vec::new();
        };
        let lost: Vec<_> = host
            .jobs()
            .into_iter()
            .filter(|j| j.state.is_running())
            .collect();
        if lost.is_empty() {
            return lost;
        }
        if let Some(store) = self.cfg.store.as_ref().and_then(|p| Store::open(p).ok()) {
            for j in &lost {
                let row = JobRecord {
                    handle: j.id.0.clone(),
                    command: j.command.clone(),
                    how: String::new(),
                    state: VM_LOST.to_string(),
                    produced: j.produced,
                    elapsed_ms: j.elapsed.as_millis() as u64,
                    redirect: None,
                };
                let _ = store.put_job(&self.cfg.session_id, &row);
                let _ = store.deliver_job(&self.cfg.session_id, &row.handle);
            }
        }
        lost
    }

    /// The session's own scope, for a monitor the daemon declares on the session's
    /// behalf — the flowy listener. `None` when the backend cannot start processes,
    /// which is also when there are no monitors to declare it in.
    pub fn session_scope(&self) -> Option<letibot_tools::exec::ScopeId> {
        self.runtime
            .backend
            .processes()?
            .scope_for(letibot_tools::exec::ScopeKind::Session, None)
            .ok()
    }
}
