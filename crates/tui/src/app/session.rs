//! The head's place in the daemon: attaching, the link and getting it back, stopping,
//! and the tree of sessions this head can switch between.

use super::*;
use crate::ui::render::dur_human;
use letibot_sessionlog::client::Unreadable;
use letibot_sessionlog::origin::SessionOrigin;
use letibot_sessionlog::registry::short_id;
use letibot_sessionlog::view::Warned;

impl App {
    /// Which process this head is attached to: called by the caller that opened the socket, once per connection,
    /// including a reconnect, where the answer can be a different process. The head is the one holder of this fact,
    /// since `/status`, the farewell and the driver all read it from here.
    pub fn set_daemon_pid(&mut self, pid: Option<i32>) {
        self.daemon_pid = pid;
    }

    /// The daemon's pid, or `None` when the kernel would not name the peer.
    pub fn daemon_pid(&self) -> Option<i32> {
        self.daemon_pid
    }

    /// The stop this head is waiting on, or `None`: `Some` from the moment the frame goes out until the head exits.
    /// Read by the renderer for its line, by the driver for the four facts it observes, and by `main` for the
    /// farewell.
    pub fn stopping(&self) -> Option<&Stopping> {
        self.stopping.as_ref()
    }

    /// The same, to write the observations into. The driver is the only writer: it holds
    /// the socket, the clock and the pid, and none of those are the app's.
    pub fn stopping_mut(&mut self) -> Option<&mut Stopping> {
        self.stopping.as_mut()
    }

    /// The head has asked, and is now waiting. Called by the driver once, when the
    /// frame has been written (or failed to be), and never before - a head that showed
    /// this line while it had not sent anything would be lying about what it did.
    pub fn stop_began(&mut self, who: &str, sent: bool, pid: Option<i32>, now_ms: u64) {
        self.stopping = Some(Stopping {
            who: who.to_string(),
            since_ms: now_ms,
            deadline_ms: now_ms.saturating_add(STOP_DEADLINE_MS),
            sent,
            acked: false,
            closed: false,
            gone: false,
            pid,
            turn_busy: self.turn_busy(),
            forced: None,
        });
        self.redraw = true;
    }

    /// The head is stopping the daemon the forced way from the start, because the
    /// daemon is an older build. No stop frame went out - see [`ForceWhy::OlderDaemon`]  -
    /// so `sent` is false and the farewell says why rather than *could NOT be sent*.
    pub fn force_stop_began(&mut self, who: &str, pid: Option<i32>, why: ForceWhy, now_ms: u64) {
        self.stopping = Some(Stopping {
            who: who.to_string(),
            since_ms: now_ms,
            deadline_ms: now_ms,
            sent: false,
            acked: false,
            closed: false,
            gone: false,
            pid,
            turn_busy: self.turn_busy(),
            forced: Some(Forcing {
                why,
                since_ms: now_ms,
                outcome: None,
            }),
        });
        self.redraw = true;
    }

    /// The ordinary stop did not finish and the head is escalating, once: called when the deadline passes with the
    /// process still there. A second call is ignored, and if the escalation fails too the person is told what to run.
    pub fn force_stop_escalated(&mut self, why: ForceWhy, now_ms: u64) {
        if let Some(s) = self.stopping.as_mut()
            && s.forced.is_none()
        {
            s.forced = Some(Forcing {
                why,
                since_ms: now_ms,
                outcome: None,
            });
            self.redraw = true;
        }
    }

    /// The forced stop has an answer. `gone` is the force path's own observation of the
    /// process; it is folded into [`Stopping::gone`] so every reader sees one fact.
    pub fn force_stop_finished(&mut self, outcome: ForceOutcome) {
        if let Some(s) = self.stopping.as_mut()
            && let Some(f) = s.forced.as_mut()
        {
            if outcome.gone {
                s.gone = true;
            }
            f.outcome = Some(outcome);
            self.redraw = true;
        }
    }

    /// The protocol the attached daemon said it speaks in its `Hello`, or `None` before one.
    pub fn daemon_protocol(&self) -> Option<u32> {
        self.daemon_protocol
    }

    /// Whether a plain detach should be sent on the way out: not while a stop is in flight, since the daemon's
    /// `Detach` handling would take this head off the session it is shutting down. The connection is going anyway.
    pub fn wants_detach(&self) -> bool {
        self.stopping.is_none()
    }

    /// The farewell this head owes after the screen is gone: a person who chose *stop* and got a running daemon
    /// learns it here, once, on stderr, rather than from `ps` a day later. `None` when there is nothing to say.
    pub fn stop_farewell(&self) -> Option<String> {
        let s = self.stopping.as_ref()?;
        let secs = s.since_ms.max(self.now_ms).saturating_sub(s.since_ms) / 1000;
        // A forced stop is always said, whether or not it worked: the person chose *stop* and the head did
        // something sharper, interrupting the turns and signalling the process, so the farewell says so and why,
        // even when the daemon is gone.
        let forced = s.forced.as_ref().map(|f| {
            let done = match &f.outcome {
                Some(o) if o.gone => format!("done: {}.", o.said),
                Some(o) => format!("the forced way did not finish it either: {}.", o.said),
                None => "the forced way had not finished when this head left.".to_string(),
            };
            format!("{}\n  {done}", f.why.line())
        });
        if s.gone {
            return forced;
        }
        // A `Bye` is not the daemon going: it is published from the connection thread the instant the stop is read,
        // so the wait is not cut short and this sentence is composed once `gone` settles. A `Bye` makes it stronger
        // rather than quieter, not a reason to stop waiting.
        let pid = pid_word(s.pid);
        let ask = if matches!(
            s.forced,
            Some(Forcing {
                why: ForceWhy::OlderDaemon { .. },
                ..
            })
        ) {
            "was not sent as a stop frame, because the daemon is an older build"
        } else if self.bye.is_some() {
            "was acknowledged, and the daemon began shutting down"
        } else if s.acked {
            "was acknowledged and did not stop"
        } else if s.sent {
            "was sent and never acknowledged"
        } else {
            "could NOT be sent"
        };
        let because = if s.turn_busy {
            // And a round can be inside a command: one worker runs the round's tools, so a stop arriving mid-command
            // waits for it, and that command's own deadline is what ends it.
            "A turn was running; the daemon interrupts it on the way out, so this is the \
             round a COMMAND is inside — and its own deadline is what ends that. This is a \
             slow stop rather than a refused one."
        } else {
            "No turn was running, so there was nothing for it to finish."
        };
        let forced = forced.map(|f| format!("{f}\n  ")).unwrap_or_default();
        Some(format!(
            "the daemon was asked to stop and had not gone {secs}s later.\n  \
             the request {ask}; the daemon is still there ({pid}).\n  {because}\n  \
             {forced}`letibot --stop --force` finishes it — it aborts in-flight turns over the \
             protocol, then signals, and says `NOT stopped` if the process survives."
        ))
    }

    /// Why the daemon ended this, for after the screen is given back: `Some` only when a `Bye` arrived. The caller
    /// prints it once the terminal is restored, since anything said into the transcript goes down with the
    /// alternate screen.
    pub fn farewell(&self) -> Option<&str> {
        self.bye.as_deref()
    }

    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// The next door call's number, for the `call_id` the daemon keys its pending set on.
    pub fn next_head_run(&mut self) -> u64 {
        self.head_run_seq += 1;
        self.head_run_seq
    }

    /// Tell the head it is about to ask the daemon, so the frames it draws meanwhile tell the truth. Called once,
    /// by a head that has taken the screen. See [`Self::attaching`].
    pub fn begin_attach(&mut self) {
        self.begin_attach_at(self.now_ms);
    }

    /// The same, with the clock stated rather than read, for a caller that knows it  -
    /// and for a test, which has no clock.
    pub fn begin_attach_at(&mut self, now_ms: u64) {
        self.attaching = true;
        self.attach_started_ms = now_ms;
    }

    /// The head id the daemon just seated this connection with, once. Taken rather than read, because a client that
    /// re-applied a stale one would ack into the session it left.
    pub fn take_seated(&mut self) -> Option<String> {
        self.seated.take()
    }

    /// The picker's rows, as one enumeration: every key and the drawing read this, because working out which row a
    /// cursor is on separately per reader is the two-enumerations defect. A head in a conversation draws the daemon
    /// as a tree of projects, system agents and sessions; a head in a subagent draws its family.
    pub(crate) fn session_rows(&self) -> Vec<SessionRow> {
        // The chain from the current session up to its root, by id - so the way back to where you
        // are is always on the screen.
        let mut path: Vec<String> = vec![self.session_id.clone()];
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            path.push(p.to_string());
            cur = p;
        }
        let mut out: Vec<SessionRow> = Vec::new();
        // This head is in a subagent: the parent and its children and nothing else. Ungrouped on purpose, since
        // folds over a list of two or three rows would be a second way to hide the answer to where am I.
        if let Some(parent) = self.parent_session()
            && let Some(i) = self.index_of(&parent)
        {
            let id = self.sessions[i].session_id.clone();
            let kids = self.children_in(&id, None);
            let force = self.family_open(&parent, 0);
            out.push(SessionRow {
                idx: Some(i),
                depth: 0,
                node: Node::Session,
                open: kids.then(|| force || self.children_open(&id, &path)),
            });
            self.push_children(&mut out, &parent, 1, &path, force, None);
            return out;
        }
        let place = self.placements();
        let projects = self.projects(&place);
        let here = self.here_nodes(&place);
        let now = letibot_sessionlog::registry::now_ms();
        let in_bucket = |b: Bucket| -> Vec<&Project> {
            projects
                .iter()
                .filter(|p| self.bucket_of(&place, &p.key, now) == b)
                .collect()
        };
        for project in in_bucket(Bucket::Own) {
            self.push_project(&mut out, project, 0, &place, &here, &path);
        }
        for b in [Bucket::Recent, Bucket::Older] {
            let group = in_bucket(b);
            if group.is_empty() {
                continue;
            }
            let node = Node::Bucket {
                bucket: b,
                count: group.len(),
            };
            let open = self.node_open(&node, &here);
            out.push(SessionRow {
                idx: None,
                depth: 0,
                node,
                open: Some(open),
            });
            if open {
                for project in group {
                    self.push_project(&mut out, project, 1, &place, &here, &path);
                }
            }
        }
        if !self.fleet.is_empty() || self.fleet_unlisted > 0 {
            let node = Node::Packages {
                count: self.fleet.len(),
            };
            let open = self.node_open(&node, &here);
            out.push(SessionRow {
                idx: None,
                depth: 0,
                node,
                open: Some(open),
            });
            if open {
                for f in &self.fleet {
                    out.push(SessionRow {
                        idx: None,
                        depth: 1,
                        node: Node::Fleet { id: f.id.clone() },
                        open: None,
                    });
                }
                if self.fleet_unlisted > 0 {
                    out.push(SessionRow {
                        idx: None,
                        depth: 1,
                        node: Node::FleetUnlisted {
                            count: self.fleet_unlisted,
                        },
                        open: None,
                    });
                }
            }
        }
        out
    }

    /// One project's line and, when it is open, its tree and `system` branch. `base` is the line's own
    /// depth: 0 on its own, 1 inside a recency group.
    fn push_project(
        &self,
        out: &mut Vec<SessionRow>,
        project: &Project,
        base: usize,
        place: &[Placement],
        here: &[String],
        path: &[String],
    ) {
        // A removed session is not counted either - a project line saying `(7)` over six
        // rows is a number that lies about what it hides, and the same filter feeds the
        // built-in layer's copy of this list (`App::sessions_feed`).
        let members: Vec<usize> = (0..self.sessions.len())
            .filter(|&i| place[i].project == project.key && !self.removed_chain(i))
            .collect();
        let node = Node::Project {
            key: project.key.clone(),
            name: project.name.clone(),
            count: members.len(),
        };
        let open = self.node_open(&node, here);
        out.push(SessionRow {
            idx: None,
            depth: base,
            node,
            open: Some(open),
        });
        if !open {
            return;
        }
        // The project's own tree - the sessions nobody spawned, and their children at any
        // depth. A child whose parent is not on this list (a session whose parent is gone) is
        // a root here, which is the honest reading: an orphan is not a subagent.
        let branch = Branch {
            project: &project.key,
            system: None,
            place,
        };
        for &i in &members {
            if place[i].system.is_some() || self.parent_on_list(i).is_some() {
                continue;
            }
            self.push_session(out, i, base + 1, path, false, Some(branch));
        }
        // The `system` branch, when this project has one.
        let system: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| place[i].system.is_some())
            .collect();
        if system.is_empty() {
            return;
        }
        let node = Node::System {
            key: system_key(&project.key),
            count: system.len(),
        };
        let open = self.node_open(&node, here);
        out.push(SessionRow {
            idx: None,
            depth: base + 1,
            node,
            open: Some(open),
        });
        if !open {
            return;
        }
        for kind in SystemKind::ALL {
            let group: Vec<usize> = system
                .iter()
                .copied()
                .filter(|&i| place[i].system == Some(kind))
                .collect();
            if group.is_empty() {
                continue;
            }
            let node = Node::Group {
                key: group_key(&project.key, kind),
                kind,
                count: group.len(),
            };
            let open = self.node_open(&node, here);
            out.push(SessionRow {
                idx: None,
                depth: base + 2,
                node,
                open: Some(open),
            });
            if !open {
                continue;
            }
            let branch = Branch {
                project: &project.key,
                system: Some(kind),
                place,
            };
            for &i in &group {
                // A row is drawn once, under its parent when that parent is in this same
                // group; a reviewer whose parent is the operator's own conversation is a root
                // of `reviewers`, because its parent is not on this branch.
                if self.parent_on_list(i).is_some_and(|p| {
                    place[p].system == Some(kind) && place[p].project == project.key
                }) {
                    continue;
                }
                self.push_session(out, i, base + 3, path, false, Some(branch));
            }
        }
    }

    /// One session's row and, when it is open, its children: the recursive half of the tree, shared
    /// by the project branch and the `system` groups. `force` is the family view's rule for its first
    /// level only, so a row's `open` and the rows drawn under it are one answer.
    fn push_session(
        &self,
        out: &mut Vec<SessionRow>,
        i: usize,
        depth: usize,
        path: &[String],
        force: bool,
        branch: Option<Branch<'_>>,
    ) {
        // A row the person took off the list is not drawn, and neither is anything under it: the
        // unit of removal is the subtree, since a child whose parent is off the tree has no place on
        // it. One guard covers all three ways a session becomes a row. See `app::removal`.
        if self.removed_chain(i) {
            return;
        }
        let id = self.sessions[i].session_id.clone();
        let kids = self.children_in(&id, branch);
        let open = force || self.children_open(&id, path);
        out.push(SessionRow {
            idx: Some(i),
            depth: depth.min(MAX_INDENT),
            node: Node::Session,
            open: kids.then_some(open),
        });
        if open {
            self.push_children(out, &id, depth + 1, path, false, branch);
        }
    }

    /// Whether a session's own children are on the list - the operator opened it, or the chain
    /// to where this head is standing runs through it.
    fn children_open(&self, id: &str, path: &[String]) -> bool {
        self.expanded.iter().any(|e| e == id) || self.child_on_path(id, path)
    }

    /// Whether one of `id`'s children is on the chain to the current session.
    fn child_on_path(&self, id: &str, path: &[String]) -> bool {
        self.sessions
            .iter()
            .any(|s| s.parent_session_id.as_deref() == Some(id) && path.contains(&s.session_id))
    }

    /// Whether a session has a child drawn in `branch` - the fact the fold glyph is made of: a
    /// `+` over a row whose children are in another branch opens nothing.
    fn children_in(&self, id: &str, branch: Option<Branch<'_>>) -> bool {
        self.sessions.iter().enumerate().any(|(k, s)| {
            s.parent_session_id.as_deref() == Some(id) && branch.is_none_or(|b| b.holds(k))
        })
    }

    /// The index of a session's parent, when that parent is on this list: `None` for a session
    /// nobody spawned and for one whose parent this daemon does not hold. `pub(crate)` because
    /// `app::removal` walks the same edge.
    pub(crate) fn parent_on_list(&self, i: usize) -> Option<usize> {
        self.sessions[i]
            .parent_session_id
            .as_deref()
            .and_then(|p| self.index_of(p))
    }

    pub(crate) fn index_of(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.session_id == id)
    }

    /// A path with `$HOME` written as `~`, against this head's home and not the environment's:
    /// the list's rows, the plugin's feed and the status line all read it here, and two readers of
    /// the environment is how a picture came to depend on which process drew it.
    pub(crate) fn tilde_path(&self, path: &str) -> String {
        let home = self.home.as_str();
        match path.strip_prefix(home) {
            Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
                format!("~{rest}")
            }
            _ => path.to_string(),
        }
    }

    /// The session a typed number names: the n-th session, not the n-th row. A group's line takes
    /// no number, because a number is something a reader types to go somewhere. `None` past the
    /// last session and for `0`, since the list is numbered from 1.
    pub(crate) fn session_at_number(&self, n: usize) -> Option<usize> {
        // The numbers are the drawn list's, whichever of the two drew it, because a number that
        // means one thing on the screen and another here is the two-enumerations defect.
        self.picker_rows()
            .iter()
            .filter(|r| r.number.is_some())
            .filter_map(|r| r.session)
            .nth(n.checked_sub(1)?)
    }

    /// Where each session of the daemon's list is drawn: its project, and the `system` group it
    /// belongs in when it is one of the harness's own agents. Parallel to `self.sessions` and not a
    /// map, because every reader indexes the other one and a map would be a second way to spell it.
    pub(crate) fn placements(&self) -> Vec<Placement> {
        let own: Vec<Project> = self
            .sessions
            .iter()
            .map(|s| project_of(&s.wiring.workspace))
            .collect();
        (0..self.sessions.len())
            .map(|i| Placement {
                // The root of the chain decides, because a child's workspace can be a review clone
                // or another machine: only a session nobody spawned is placed by its own path.
                project: own[self.root_of(i)].key.clone(),
                system: self.system_kind(i),
            })
            .collect()
    }

    /// The session at the top of this one's chain - itself for a session nobody spawned.
    fn root_of(&self, i: usize) -> usize {
        let mut at = i;
        for _ in 0..MAX_CHAIN {
            match self.parent_on_list(at) {
                Some(parent) => at = parent,
                None => break,
            }
        }
        at
    }

    /// Which system agent this session is, if it is one: its own row's marks first, then the
    /// chain's. The order is the point, since the review queue's own session is a gatekeeper and
    /// everything hanging from it is a reviewer.
    fn system_kind(&self, i: usize) -> Option<SystemKind> {
        let mut at = i;
        for _ in 0..MAX_CHAIN {
            let s = &self.sessions[at];
            let here = at == i;
            if letibot_sessionlog::is_gatekeeper_session(&s.session_id) {
                return Some(if here {
                    SystemKind::Gatekeeper
                } else {
                    SystemKind::Reviewer
                });
            }
            if s.origin == SessionOrigin::Queue {
                return Some(if here && s.parent_session_id.is_none() {
                    SystemKind::Gatekeeper
                } else {
                    SystemKind::Reviewer
                });
            }
            // A brief carries no seat, so a reviewer is known here by the brief its title was cut
            // from; `App::fold_subagents` fills the same gap the same way.
            if s.title
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX)
            {
                return Some(SystemKind::Reviewer);
            }
            if s.title
                .starts_with(letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX)
            {
                return Some(SystemKind::Keeper);
            }
            match self.parent_on_list(at) {
                Some(parent) => at = parent,
                None => break,
            }
        }
        None
    }

    /// The projects this daemon holds, in the order it lists them - the top level of the tree.
    pub(crate) fn projects(&self, place: &[Placement]) -> Vec<Project> {
        let mut out: Vec<Project> = Vec::new();
        for p in place {
            if !out.iter().any(|q| q.key == p.project) {
                out.push(Project {
                    key: p.project.clone(),
                    name: String::new(),
                });
            }
        }
        // The name, and the whole path only when two projects would read alike: two checkouts
        // called `letibot` are two rows a reader cannot tell apart, so both say where they are.
        let names: Vec<String> = out.iter().map(|p| project_name(&p.key)).collect();
        let shared: Vec<bool> = names
            .iter()
            .map(|n| names.iter().filter(|m| *m == n).count() > 1)
            .collect();
        for (i, p) in out.iter_mut().enumerate() {
            p.name = if shared[i] {
                self.tilde_path(&p.key)
            } else {
                names[i].clone()
            };
        }
        out
    }

    /// Which top-level group a project is drawn in: the daemon's own workspace (the project of the session this
    /// head is in), a session active inside [`RECENT_MS`] of `now`, or everything older.
    pub(crate) fn bucket_of(&self, place: &[Placement], key: &str, now: u64) -> Bucket {
        if self
            .index_of(&self.session_id)
            .is_some_and(|i| place[i].project == key)
        {
            return Bucket::Own;
        }
        let moved = (0..self.sessions.len())
            .filter(|&i| place[i].project == key && !self.removed_chain(i))
            .map(|i| self.sessions[i].moved_ms)
            .max()
            .unwrap_or(0);
        if moved >= now.saturating_sub(RECENT_MS) && moved > 0 {
            Bucket::Recent
        } else {
            Bucket::Older
        }
    }

    /// The nodes on the way to where this head is standing: the project, its `system` branch and
    /// the group, so a list that hides where you are cannot happen. `system` is on the chain only
    /// when this head is drawn in it, or a group line would open for a branch nobody is near.
    fn here_nodes(&self, place: &[Placement]) -> Vec<String> {
        let Some(i) = self.index_of(&self.session_id) else {
            return Vec::new();
        };
        let p = &place[i];
        let mut out = vec![p.project.clone()];
        if let Some(kind) = p.system {
            out.push(system_key(&p.project));
            out.push(group_key(&p.project, kind));
        }
        out
    }

    /// Whether a node's rows are on the list: the person's answer when they gave one, then the
    /// nodes on the way to where this head is standing, then the node's own default.
    pub(crate) fn node_open(&self, node: &Node, here: &[String]) -> bool {
        let key = node.key();
        if let Some(open) = self.picker_folds.get(key) {
            return *open;
        }
        if here.iter().any(|h| h == key) || self.expanded.iter().any(|e| e == key) {
            return true;
        }
        node.default_open()
    }

    /// Whether a row's children are on the list because this head is standing among them. The
    /// family view shows the parent's children whatever the collapse state says, and the fold glyph
    /// reads the same answer: a `▸` next to rows it is hiding is the one thing this list must not say.
    pub(crate) fn family_open(&self, id: &str, depth: usize) -> bool {
        depth == 0 && self.parent_session().as_deref() == Some(id)
    }

    /// A session's children, in the daemon's order, one step deeper: shown when their parent is
    /// expanded or when one of them is on the chain to the current session. `force` applies to one
    /// level only, which is what keeps the family view a family and not the subtree behind it.
    pub(crate) fn push_children(
        &self,
        out: &mut Vec<SessionRow>,
        parent: &str,
        depth: usize,
        path: &[String],
        force: bool,
        branch: Option<Branch<'_>>,
    ) {
        let open = force || self.expanded.iter().any(|e| e == parent);
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.as_deref() != Some(parent) {
                continue;
            }
            // A row belongs to exactly one branch, so a child of another branch is not drawn
            // under it: it is a root where it is drawn.
            if branch.is_some_and(|b| !b.holds(i)) {
                continue;
            }
            if !open && !path.contains(&s.session_id) {
                continue;
            }
            self.push_session(out, i, depth, path, false, branch);
        }
    }

    /// The conversation this head's session belongs to: itself, unless it is a sub-session. The
    /// header counts conversations, and this keeps a head driving a child from reading `0/4`.
    pub(crate) fn session_root(&self) -> String {
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            cur = p;
        }
        cur.to_string()
    }

    /// How many rows this session's store row holds while this head holds none of them, asked where both sources are
    /// in hand. Only while the head holds nothing: a compacted view is trimmed to the window, so `items.len()` is
    /// smaller than `stored_items` for any long conversation.
    pub(crate) fn rows_the_store_holds(&self) -> Option<u32> {
        if !self.items.is_empty() {
            return None;
        }
        self.sessions
            .iter()
            .find(|s| s.session_id == self.session_id)
            .map(|b| b.stored_items)
            .filter(|n| *n > 0)
    }

    pub(crate) fn session_label(&self, id: &str) -> String {
        let brief = self.sessions.iter().find(|s| s.session_id == id);
        // A subagent is named by its slug, the name its parent was handed; its title is the first
        // line of a brief that can run to 256 characters.
        if let Some(b) = brief.filter(|b| !b.slug.trim().is_empty()) {
            return b.slug.clone();
        }
        brief
            .filter(|s| !s.title.is_empty())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| short_id(id))
    }

    /// A child as a person reads it, from what this head knows of it: a short id, or its slug when
    /// it has one. See [`crate::app::child_name`].
    pub(crate) fn child_label(&self, id: &str) -> String {
        let slug = self
            .subagents
            .iter()
            .find(|s| s.session_id == id)
            .map(|s| s.slug.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|s| s.session_id == id)
                    .map(|s| s.slug.clone())
            })
            .unwrap_or_default();
        crate::app::child_name(&slug, id)
    }

    /// Ask the daemon for a session, and switch to it once seated: what `--continue` and `--session
    /// ID` turn into. It cannot be an `Attach` naming the id, since the daemon refuses an attach to a
    /// session it does not hold and *not held yet* is the state a resume is for.
    pub fn request_session(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.want_new_session = true;
        self.fetching = Some(id.to_string());
        self.queued.push(Action::ResumeSession(id.to_string()));
    }

    /// Make a session and go there once this head is attached: `--new [TITLE]` against a daemon
    /// that is already running.
    pub fn request_new_session(&mut self, title: &str) {
        self.want_new_session = true;
        self.queued.push(Action::NewSession(title.to_string()));
    }

    /// A frame this head could not read: said and counted, and not fatal. It leaves `seq` alone and acks nothing,
    /// since no frame was parsed and inventing a seq would rewind the read mark over frames already read.
    pub fn unreadable(&mut self, u: Unreadable) -> Disposition {
        self.unreadable += 1;
        // `ts` is 0: this happened on the socket rather than on the session's log, and the log's
        // clock is not this. A note with no timestamp renders without one.
        self.note(Note::Warned(Warned {
            code: "unreadable_frame".into(),
            detail: u.said(),
            ts: 0,
        }));
        self.redraw = true;
        Disposition::Control
    }

    /// The daemon connection has gone, called when a write fails, when the pump's channel closes and when a
    /// reconnect fails. Idempotent, since both ways it is noticed arrive together, so the elapsed time is measured
    /// from the first report. A head that is going away does not reconnect.
    pub fn link_down(&mut self, why: &str) {
        if self.quit || self.bye.is_some() {
            return;
        }
        // No answer is coming, so nothing is asked for: a `SuggestShell` kept here would go on
        // saying *asking the model* for ever, waiting on an id that was never sent.
        self.clear_shell_suggestions();
        // A link that went down because this head asked is not news: the daemon closing our socket
        // is the answer arriving, and reconnecting would open a second socket to a process on its
        // way out.
        if self.stopping.is_some() {
            return;
        }
        // And the pane goes with the connection: the pty is the daemon's, so a head that cannot
        // reach it can neither feed the screen nor forward the one key that leaves.
        self.drop_pane();
        if self.link.is_down() {
            // Already known: refresh the reason if this report has one and keep the
            // clock. Both reports are true; the first is the more useful clock.
            if let Link::Reconnecting { why: known, .. } = &mut self.link
                && !why.is_empty()
            {
                *known = why.to_string();
            }
            return;
        }
        self.link = Link::Reconnecting {
            since_ms: self.now_ms,
            attempts: 0,
            why: why.to_string(),
            next_try_ms: self.now_ms.saturating_add(RECONNECT_BACKOFF_MS),
        };
        self.redraw = true;
    }

    /// Whether the caller should try to get back now: the link is down and the backoff has passed.
    /// The timing lives on the head because the head has the clock, and a caller with its own copy
    /// would be a second version of one rule.
    pub fn should_reconnect(&self) -> bool {
        matches!(&self.link, Link::Reconnecting { next_try_ms, .. } if self.now_ms >= *next_try_ms)
    }

    /// An attempt to get back is out: the caller has opened a socket and sent its `ATTACH`, which stops the retry
    /// from eating its own answer, since a loop that asked between the two would close the socket about to be
    /// answered.
    pub fn reconnect_sent(&mut self) {
        if let Link::Reconnecting { next_try_ms, .. } = &mut self.link {
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
        }
    }

    /// An attempt to get back has failed, and this is the last thing it said. Counted, because a
    /// head that has tried forty times and one that has tried once are in different situations, and
    /// *still reconnecting* says neither.
    pub fn reconnect_failed(&mut self, why: &str) {
        if let Link::Reconnecting {
            attempts,
            why: known,
            next_try_ms,
            ..
        } = &mut self.link
        {
            *attempts += 1;
            *known = why.to_string();
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
            self.redraw = true;
        }
    }

    /// The daemon is back: the `Hello` says so, being the frame that seats the connection. It returns the sentence
    /// rather than filing it, since the next thing the arm does is fold in the snapshot, which replaces `self.notes`
    /// wholesale. `None` when nothing was down.
    pub(crate) fn link_up(&mut self) -> Option<String> {
        let Link::Reconnecting {
            since_ms, attempts, ..
        } = self.link.clone()
        else {
            return None;
        };
        let out = self.now_ms.saturating_sub(since_ms);
        // Only when there were failed attempts: the ordinary reconnect succeeds first try, and
        // empty brackets are a head saying a number where there is none.
        let tries = if attempts == 0 {
            String::new()
        } else {
            format!(
                " ({} attempt{})",
                attempts,
                if attempts == 1 { "" } else { "s" }
            )
        };
        self.link = Link::Attached;
        self.redraw = true;
        Some(format!(
            "the daemon is back after {}{tries}. Resuming from seq {} — anything the \
             daemon has for me in the gap arrives as events, or as a resync if it is \
             larger than the daemon still holds.",
            dur_human(out),
            self.seq,
        ))
    }

    /// A command did not leave, because there is no daemon to send it to. Said once per batch, since
    /// the alternative is one sentence per action and the batch is usually one action.
    pub fn refused_while_detached(&mut self) {
        self.say("no daemon connection — that did not go out");
        self.redraw = true;
    }

    /// Whether the link to the daemon is down: what the screen asks before it draws the detached
    /// line, and what `submit` asks before it turns a line into a prompt.
    pub fn detached(&self) -> bool {
        self.link.is_down()
    }

    /// Is this head holding everything it would send, because the daemon speaks an older protocol and nobody has said
    /// attach anyway. Older locks and newer does not: an unknown frame from a newer daemon is a reading problem the
    /// head survives, while an older daemon's missing arm closes the socket.
    pub(crate) fn skew_locked(&self) -> bool {
        self.daemon_protocol
            .is_some_and(|d| d < letibot_sessionlog::protocol::PROTOCOL_VERSION)
            && self
                .daemon_seat
                .is_some_and(|seat| self.skew_override_for != Some(seat))
    }

    /// [`letibot_sessionlog::protocol_skew`]'s sentence for the daemon this head is seated with, or
    /// `None` when the builds match. Every refusal and the resident line draw from here, so the
    /// argument cannot be said two ways.
    pub(crate) fn skew_sentence(&self) -> Option<String> {
        self.daemon_protocol.and_then(|d| {
            letibot_sessionlog::protocol_skew(d, letibot_sessionlog::protocol::PROTOCOL_VERSION)
        })
    }

    /// `ctrl-^`: attach anyway. Nothing is re-sent, since a refused line was held in the composer and not queued,
    /// and re-sending somebody's words after they moved on is an act nobody asked for. What it sends are the reads
    /// the seating owes.
    pub(crate) fn attach_anyway(&mut self) {
        if !self.skew_locked() {
            return;
        }
        self.skew_override_for = self.daemon_seat;
        self.queued.push(Action::TermStatus);
        self.refetch_session_facts();
        let older = self
            .daemon_protocol
            .unwrap_or(letibot_sessionlog::protocol::PROTOCOL_VERSION);
        self.say(&format!(
            "attached anyway — this head now sends to a daemon speaking protocol {older} \
             against its own {}. The first frame the two do not share still ends the \
             connection, with a `Bye` naming both builds; restarting the daemon is the fix.",
            letibot_sessionlog::protocol::PROTOCOL_VERSION
        ));
        self.redraw = true;
    }

    /// A command did not leave, because this head is holding its sends against an older daemon.
    /// [`App::refused_while_detached`]'s shape, once per batch. The sentence names both why nothing
    /// left and what to do about it, because the reader just pressed a key.
    pub(crate) fn refused_while_locked(&mut self) {
        if let Some(said) = self.skew_sentence() {
            self.say(&format!(
                "{said} Nothing was sent — this attach is read-only by default, and \
                 {ATTACH_ANYWAY_CHORD} attaches anyway.",
            ));
        }
        self.redraw = true;
    }

    /// Leave the pane without ending it: the rectangle goes and the conversation comes back, and nothing is sent, so
    /// the program keeps running on the daemon's pty and a later `!term` attaches to the same run. See [`TermPane`].
    pub fn detach(&mut self) -> bool {
        let Some(p) = self.term.as_mut() else {
            return false;
        };
        p.detached = true;
        self.redraw = true;
        true
    }

    /// `!term close`: the ending, and the head asks first. The four answers are a pane of this head's own, a live one
    /// the daemon named, no pane at all, and not asked yet, which holds the line and runs this same decision when the
    /// status arrives. Nothing here ends anything.
    pub(crate) fn begin_close(&mut self, which: Option<u32>) -> Option<Action> {
        // This head's own pane, when it is the one meant.
        if let Some(p) = &self.term
            && (which.is_none() || which == p.pane)
        {
            let Some(pane) = p.pane else {
                self.say(
                    "the pane has not started yet — the daemon has not numbered it. Nothing was \
                     ended; try again in a moment.",
                );
                self.redraw = true;
                return None;
            };
            let line = p.line.clone();
            self.ask_close(&line, pane);
            return None;
        }
        match (self.term_fact.clone(), which) {
            (PaneFact::Running(list), Some(n)) => match list.iter().find(|b| b.pane == n) {
                Some(b) => {
                    let line = format!("!term {}", b.command);
                    self.ask_close(&line, n);
                }
                None => {
                    self.say(&format!(
                        "there is no pane {n} running in this session — {}. Nothing was ended.",
                        panes_said(&list)
                    ));
                    self.redraw = true;
                }
            },
            (PaneFact::Running(list), None) if list.len() == 1 => {
                let line = format!("!term {}", list[0].command);
                self.ask_close(&line, list[0].pane);
            }
            (PaneFact::Running(list), None) => {
                self.say(&format!(
                    "this session has {} panes running — {}. `!term close N` says which; \
                     nothing was ended.",
                    list.len(),
                    panes_said(&list)
                ));
                self.redraw = true;
            }
            (PaneFact::None, _) => {
                self.say(
                    "this session has no pane to end — `!term COMMAND` starts one, and \
                     `!term` attaches to one that is already running",
                );
                self.redraw = true;
            }
            (PaneFact::Unasked, _) => {
                self.close_pending = true;
                self.close_which = which;
                self.say("asking the daemon what this session is running…");
                self.redraw = true;
                return Some(Action::TermStatus);
            }
        }
        None
    }

    /// Raise the confirmation card, and it is the only place [`App::term_ask`] is set - so
    /// there is one place a pane can be ended from, and it is the one that asks.
    pub(crate) fn ask_close(&mut self, line: &str, pane: u32) {
        // A card that names one of several says which by number; one that is the only pane
        // reads as it always did.
        let several = matches!(&self.term_fact, PaneFact::Running(l) if l.len() > 1);
        let line = if several {
            format!("pane {pane}, {line}")
        } else {
            line.to_string()
        };
        let line = line.as_str();
        self.term_ask = Some(TermAsk {
            line: line.to_string(),
            pane,
        });
        self.redraw = true;
    }

    /// Go to another session: the one door every switch goes through, and the reason the wait is a state and not a
    /// gap. A `Switch` is a round trip, so until the second `Hello` lands the screen holds the session being left,
    pub(crate) fn switch_to(&mut self, id: String) -> Option<Action> {
        if let Some(f) = self.fleet.iter().find(|f| f.id == id) {
            // Another daemon holds it, and this head is attached to one socket: the command is the
            // way in.
            let said = format!("{} - {}", f.state.word(), f.attach_command());
            self.say(&said);
            self.redraw = true;
            return None;
        }
        if id == self.session_id {
            self.close_surface(Surface::Picker);
            self.redraw = true;
            self.say("already here");
            return None;
        }
        // A session on disk and not in this daemon has to be brought in before it can be switched to: two frames,
        // the second sent when the daemon answers the first. `want_new_session` is reused because going to a session
        // the daemon just named is one behaviour.
        if self.sessions.iter().any(|b| b.session_id == id && !b.live) {
            self.want_new_session = true;
            self.say(&format!(
                "resuming {} from the store…",
                self.session_label(&id)
            ));
            self.wait_for_session(&id);
            return Some(Action::ResumeSession(id));
        }
        // `since_seq` is not sent: this head has no state for the session it is going to, so a snapshot is the only
        // honest ask, and it keeps no per-session marks, which would be a cache with no invalidation rule.
        self.wait_for_session(&id);
        Some(Action::Switch(id))
    }

    /// The wait a switch is, as one act: both arms of [`Self::switch_to`] call it, which is what makes one pending
    /// state true rather than a claim. `fetching` is the session asked for and `attaching` is *told nothing yet*, and
    /// the `Hello` that answers clears both.
    fn wait_for_session(&mut self, id: &str) {
        self.fetching = Some(id.to_string());
        self.begin_attach();
        if self.is_open(Surface::Subagents) {
            self.fold_subagents();
        }
    }

    /// The session whose children the subagents pane shows: the one this head stands in, or while a switch is in
    /// flight the one it asked for. Without it, `esc` out of a subagent drew the child's own empty list for as long
    /// as the switch took.
    pub(crate) fn subagent_parent(&self) -> &str {
        match self.fetching.as_deref() {
            Some(to) if self.is_open(Surface::Subagents) => to,
            _ => &self.session_id,
        }
    }

    /// Whether the conversation this head holds is one it is on its way out of: a switch is a round trip, so until
    /// the second `Hello` lands every row in `items` is the session being left. Not `attaching`, which is the state
    /// where the head holds no rows at all.
    pub(crate) fn leaving(&self) -> bool {
        self.fetching
            .as_deref()
            .is_some_and(|to| to != self.session_id)
    }

    /// The session this head is on its way to, as a person reads it, or `None`. Read by the status row, and the
    /// reason a switch is a state rather than a blank screen: the conversation drawn while it happens is the one
    /// being left, and nothing in it says a move is in flight.
    pub(crate) fn switching_to(&self) -> Option<String> {
        let id = self.fetching.as_deref()?;
        self.leaving().then(|| self.session_label(id))
    }

    /// Everything a seated head must ask for, in one place: attach, re-attach and the return from a switch all land
    /// in the `Hello` arm and answer with `ListSessions`, `ListJobs` and `Settings`. A `Hello` carries a copy, and a
    /// copy is not an ask, so every answer is re-folded.
    pub(crate) fn refetch_session_facts(&mut self) {
        self.queued.push(Action::Settings);
        self.queued.push(Action::ListJobs);
        self.queued.push(Action::ListSessions);
        // And the board: the read the head draws *who waits on the person* from. Without it the read happened only
        // when the `asks` settings row said a question waited, so a head that reattached after it was taken back kept
        // the copy it had.
        if !self.queued.contains(&Action::ListTodos) {
            self.queued.push(Action::ListTodos);
        }
        // The queues, for the edge: the composer's bottom edge draws this session's repository's standing, and before
        // this the only ask for the snapshot was opening the pane, so a head that never opened `/queue` drew none.
        if !self.queued.contains(&Action::ListMergeQueue) {
            self.queued.push(Action::ListMergeQueue);
        }
    }

    /// The session that spawned this one, or `None` when this head is not in a subagent. Read off the daemon's list
    /// and not inferred from the id, since a head string-matching `s-…-sub-…` would be inventing a fact the registry
    /// already states.
    pub(crate) fn parent_session(&self) -> Option<String> {
        self.sessions
            .iter()
            .find(|s| s.session_id == self.session_id)
            .and_then(|s| s.parent_session_id.clone())
    }
}

/// The daemon connection, as far as this head can tell: a socket goes away for ordinary reasons, and the head's job
/// is to keep drawing the session it has, say the link is down, and keep trying to get it back. It is not to exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Attached,
    /// Down, and the caller is retrying. `attempts` counts the tries made since it went
    /// down; `since_ms` is this head's own clock, so `now - since_ms` is how long the
    /// operator has been without a daemon.
    Reconnecting {
        since_ms: u64,
        attempts: u32,
        /// The last thing known about why: the io error, the pump going away, or the
        /// refusal from the last attempt to connect.
        why: String,
        /// No attempt before this, on the head's own clock. The backoff lives here
        /// rather than in the caller so a test can drive it with `clock`.
        next_try_ms: u64,
    },
}

impl Link {
    pub fn is_down(&self) -> bool {
        matches!(self, Link::Reconnecting { .. })
    }
}

/// A head that has asked the daemon to stop, and is waiting to find out.
///
/// The operator chose *exit, and stop the daemon too* and got the exit without the stop, leaving `harnessd`
/// idle. The old code discarded both results and returned:
///
/// ```text
/// let _ = self.client.stop(app.seq, &who);
/// let _ = self.client.detach();
/// ```
///
/// A request is not an outcome: the head does not leave while the stop is unresolved, and keeps apart three
/// observations, each a different answer for the operator:
/// - `sent`: the write returned `Ok`, the most a writer can know.
/// - `acked`: the daemon answered `Accepted { note: "stopping" }`, so the request was read.
/// - `closed`: the daemon's socket file is gone.
/// - `gone`: the process has exited and been collected, by `waitpid(WNOHANG)` for this head's own child and
///   by `/proc` otherwise, a zombie counting as gone. The only observation that is *the daemon has gone*.
///
/// A bare `fs::metadata("/proc/{pid}")` test would report a stopped daemon as running, since a zombie keeps its
/// directory until its parent waits. See [`crate::driver::Parentage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopping {
    /// The identity that asked, as the daemon will have announced it.
    pub who: String,
    /// The head's own clock when the frame went out.
    pub since_ms: u64,
    /// When the head stops waiting and says what it saw.
    pub deadline_ms: u64,
    pub sent: bool,
    pub acked: bool,
    pub closed: bool,
    pub gone: bool,
    /// The daemon's pid, from `SO_PEERCRED` on this very connection: the process at the other end of the socket and
    /// not a number read out of a file. `None` when the kernel would not say, which the farewell states rather than
    /// fills in.
    pub pid: Option<i32>,
    /// Whether the model was still working when the head gave up waiting: the daemon interrupts its turns on the way
    /// out, so this is not a turn finishing its round. It is what tells a stop inside a command from a daemon that
    /// did not go.
    pub turn_busy: bool,
    /// The force path, when the head took it - directly for an older daemon, or once,
    /// automatically, when the ordinary stop's deadline passed with the process still there.
    /// `None` for a stop that never needed it.
    pub forced: Option<Forcing>,
}

/// Why the head stopped the daemon the forced way: an exit-and-stop that did not take against an older daemon, and
/// the operator's answer that the head should take the force path itself with a line saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForceWhy {
    /// The daemon's `Hello` named an older protocol than this head speaks, so such a daemon may not read the stop
    /// frame at all: the head does not ask and wait, it goes straight to the force path, whose frame every daemon it
    /// can attach to understands.
    OlderDaemon { daemon: u32, head: u32 },
    /// The ordinary stop was asked for and the process was still there at the deadline.
    DidNotStop { waited_ms: u64 },
}

impl ForceWhy {
    /// The sentence, said on the screen while the head waits and again in the farewell.
    pub fn line(&self) -> String {
        let how = format!(
            "stopping it the forced way: in-flight turns are interrupted first, then it is \
             sent SIGTERM, and SIGKILL if it is still there {}s later",
            FORCE_TERM_GRACE_MS / 1000
        );
        match self {
            ForceWhy::OlderDaemon { daemon, head } => format!(
                "the daemon is an older build (protocol {daemon}, this head speaks {head}) — {how}"
            ),
            ForceWhy::DidNotStop { waited_ms } => format!(
                "the daemon did not stop within {}s — {how}",
                waited_ms / 1000
            ),
        }
    }
}

/// The force path in flight, and its answer once it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forcing {
    pub why: ForceWhy,
    /// The head's own clock when the force path started.
    pub since_ms: u64,
    pub outcome: Option<ForceOutcome>,
}

/// What the force path did - `crate::head::force_stop`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceOutcome {
    /// The daemon's process is gone, observed (zombie-aware, as `watch_stop` observes it).
    pub gone: bool,
    /// What was done, in order, as the operator reads it: the interrupt's answer, each
    /// signal and what followed it, or why no signal was sent.
    pub said: String,
}

/// How long the force path gives SIGTERM before SIGKILL: five seconds, the figure
/// `scripts/letibot --stop --force` waits (`wait_gone`) between the two, so the head's force
/// path and the wrapper's are one path with one number.
pub const FORCE_TERM_GRACE_MS: u64 = 5_000;

impl Stopping {
    /// Has the question been answered? Either the daemon's process is gone or the deadline has passed and the head
    /// can say what it observed. A force path in flight holds the head either way, since it ends on its own
    /// observation of the process.
    pub fn resolved(&self, now_ms: u64) -> bool {
        match &self.forced {
            Some(f) => f.outcome.is_some(),
            None => self.gone || now_ms >= self.deadline_ms,
        }
    }

    /// The line the head draws while it waits, so the screen is never a freeze.
    pub fn waiting_line(&self, now_ms: u64) -> String {
        if let Some(f) = &self.forced {
            return format!(
                "stopping the daemon: {} — {} so far.",
                f.why.line(),
                dur_human(now_ms.saturating_sub(f.since_ms)),
            );
        }
        let out = now_ms.saturating_sub(self.since_ms);
        let left = self.deadline_ms.saturating_sub(now_ms);
        let seen = if self.acked {
            "the daemon answered and is shutting down"
        } else if self.sent {
            "the request went out and the daemon has not answered yet"
        } else {
            "the request could not be sent — the socket is already gone"
        };
        format!(
            "stopping the daemon: {seen} — {} waiting, {} before this head stops it \
             the forced way. A turn already generating is interrupted on the \
             way out, not waited for.",
            dur_human(out),
            dur_human(left),
        )
    }
}

/// How long a head waits for a daemon it asked to stop, and how long an orderly stop is given before it is forced.
/// Five seconds was the wrapper's figure for SIGTERM then SIGKILL, and it was fine while the deadline only decided
/// when to print a sentence.
pub const STOP_DEADLINE_MS: u64 = 15_000;

/// How long between attempts to get back: flat, and the same number the other head uses. A daemon coming back is
/// back in well under a second, and one that is gone costs one connect per interval, so growing it would optimise
/// nothing.
pub const RECONNECT_BACKOFF_MS: u64 = 2_000;

/// When a head with no daemon stops saying `reconnecting` and starts saying how long, and what the person can do.
/// Under it a drop is usually over before the sentence is read; over it the wait is not a moment.
pub const LINK_IMPATIENT_MS: u64 = 15_000;

/// A pid as a sentence fragment, or the honest absence: `None` is *the kernel would not name the peer*, which is a
/// different statement from a pid of zero, and printing a number the head does not have sends the person to `ps`.
pub(crate) fn pid_word(pid: Option<i32>) -> String {
    match pid {
        Some(p) => format!("pid {p}"),
        None => "a pid the kernel did not name".to_string(),
    }
}

/// Which daemon a head is drawing: a pair rather than the pid alone, because the kernel reuses pids, so a daemon
/// restarted onto the same number is a different daemon with the same identity, and the protocol version is the
/// other half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonSeat {
    /// The process at the other end of this socket, from `SO_PEERCRED`. `None` when the kernel
    /// would not say, which compares equal to another `None` - the one case this cannot tell
    /// apart, and the reason the protocol version is beside it.
    pub(crate) pid: Option<i32>,
    /// The version the daemon claims, from the `Hello` that seated this connection.
    pub(crate) protocol: u32,
}

/// The branch of the tree a walk is inside: the project, and the `system` group when the walk is under it. `None`
/// is the family view. One value rather than two arguments, since the placements and the branch are only meaningful
/// together.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Branch<'a> {
    pub(crate) project: &'a str,
    pub(crate) system: Option<SystemKind>,
    pub(crate) place: &'a [Placement],
}

impl Branch<'_> {
    /// Whether session `i` is drawn in this branch - a child of another branch is a root of its
    /// own, never a row here.
    fn holds(&self, i: usize) -> bool {
        self.place
            .get(i)
            .is_some_and(|p| p.project == self.project && p.system == self.system)
    }
}

/// Where one session is drawn - the project, and the `system` group it belongs in when it is
/// one of the harness's own agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) project: String,
    pub(crate) system: Option<SystemKind>,
}

/// A project: the top level of the tree - a workspace root, shown by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    /// The path the project is, and the key its fold state is kept under.
    pub(crate) key: String,
    /// What the row says: the folder's name, or the whole path when two projects would read alike.
    pub(crate) name: String,
}

/// One of the harness's own agents: the groups under a project's `system` branch. The three are the marks this tree
/// can read off a session row, and a row carrying none of them is drawn as the session it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemKind {
    /// The review queue's own session, one per project (`s-gatekeeper-<key>`).
    Gatekeeper,
    /// A review's own child, and anything it spawned.
    Reviewer,
    /// The notes keeper, which repairs memory and runs nothing.
    Keeper,
}

impl SystemKind {
    pub(crate) const ALL: [SystemKind; 3] = [
        SystemKind::Gatekeeper,
        SystemKind::Reviewer,
        SystemKind::Keeper,
    ];

    /// The word on the group's line, and the last part of its fold key.
    pub(crate) fn title(self) -> &'static str {
        match self {
            SystemKind::Gatekeeper => "gatekeepers",
            SystemKind::Reviewer => "reviewers",
            SystemKind::Keeper => "keepers",
        }
    }

    /// One line saying what the group is - a fold whose contents a reader has to guess at is a fold
    /// they will not open.
    pub(crate) fn legend(self) -> &'static str {
        match self {
            SystemKind::Gatekeeper => "the review queue's own session, one per project",
            SystemKind::Reviewer => "a review's own child, and anything it spawned",
            SystemKind::Keeper => "the notes keeper, which repairs memory and runs nothing",
        }
    }
}

/// The fold key of the packages line. Session ids begin `s-` and project keys are paths.
pub(crate) const PACKAGES_KEY: &str = "fleet/packages";
pub(crate) const UNLISTED_KEY: &str = "fleet/unlisted";

/// How recent a project's newest session must be for it to sit in `other projects` rather than `older`.
pub(crate) const RECENT_MS: u64 = 24 * 60 * 60 * 1000;
pub(crate) const RECENT_KEY: &str = "groups/recent";
pub(crate) const OLDER_KEY: &str = "groups/older";

/// What the first project line says it is. Said once, since the same words under forty lines are noise.
pub(crate) const PROJECT_LEGEND: &str = "a workspace root, and every session that runs in it";

/// Where a project is drawn: open at the top, or in one of two folded groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    Own,
    Recent,
    Older,
}

impl Bucket {
    /// The word the feed carries for it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Bucket::Own => "own",
            Bucket::Recent => "recent",
            Bucket::Older => "older",
        }
    }
}

/// What the line over other daemons' packages says it is.
pub(crate) const FLEET_LEGEND: &str =
    "package sessions of other folders' daemons - enter says how to attach";

/// What a row of the session list is - a session, or one of the three lines the tree is built
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Project {
        key: String,
        name: String,
        count: usize,
    },
    System {
        key: String,
        count: usize,
    },
    Group {
        key: String,
        kind: SystemKind,
        count: usize,
    },
    Session,
    /// A folded group of projects: `Recent` or `Older`, never `Own`.
    Bucket {
        bucket: Bucket,
        count: usize,
    },
    /// The line over the package sessions of other daemons.
    Packages {
        count: usize,
    },
    /// The last line of the group: how many stopped packages are not listed.
    FleetUnlisted {
        count: usize,
    },
    /// One of them. Not in `App::sessions`: another daemon holds it.
    Fleet {
        id: String,
    },
}

impl Node {
    /// The key this node's fold state is kept under - a path for a project, `…/system` and
    /// `…/system/<group>` below it. Session ids begin `s-`, so the two can never collide.
    pub(crate) fn key(&self) -> &str {
        match self {
            Node::Project { key, .. } | Node::System { key, .. } | Node::Group { key, .. } => key,
            Node::Bucket {
                bucket: Bucket::Older,
                ..
            } => OLDER_KEY,
            Node::Bucket { .. } => RECENT_KEY,
            Node::Packages { .. } => PACKAGES_KEY,
            Node::FleetUnlisted { .. } => UNLISTED_KEY,
            Node::Fleet { id } => id,
            Node::Session => "",
        }
    }

    /// What the line says: the name, and how many rows it hides.
    pub(crate) fn title(&self) -> String {
        match self {
            Node::Project { name, count, .. } => format!("{name} ({count})"),
            Node::System { count, .. } => format!("system ({count})"),
            Node::Group { kind, count, .. } => format!("{} ({count})", kind.title()),
            Node::Bucket {
                bucket: Bucket::Older,
                count,
            } => format!("older ({count})"),
            Node::Bucket { count, .. } => format!("other projects ({count})"),
            Node::Packages { count } => format!("packages in other daemons ({count})"),
            Node::FleetUnlisted { count } => letibot_sessionlog::fleet::unlisted_words(*count),
            Node::Session | Node::Fleet { .. } => String::new(),
        }
    }

    /// The faint line under a node, saying what it is.
    pub(crate) fn legend(&self) -> &'static str {
        match self {
            Node::Project { .. } => "",
            Node::Bucket {
                bucket: Bucket::Older,
                ..
            } => "projects with nothing in the last 24 hours",
            Node::Bucket { .. } => "projects with a session active in the last 24 hours",
            Node::System { .. } => "the sessions the harness runs for itself",
            Node::Group { kind, .. } => kind.legend(),
            Node::Packages { .. } => FLEET_LEGEND,
            Node::Session | Node::Fleet { .. } | Node::FleetUnlisted { .. } => "",
        }
    }

    /// How a node stands before anybody has folded it: closed, all four. A project's line and its `system` branch
    /// are closed too: on a box with a hundred sessions open ones bury the one being looked for.
    pub(crate) fn default_open(&self) -> bool {
        false
    }
}

/// One row of the session picker. Four facts rather than a bare `usize`, because the list is a tree: which session
/// or node it is, how deep it sits, and whether its own rows are under it. Deriving any of them at the drawing site
/// is the second way of answering one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Index into `App::sessions`, or `None` for a node's own line - a stop with an act of its
    /// own (`enter` folds it) and no session to switch to.
    pub(crate) idx: Option<usize>,
    /// 0 for a project, deeper inside the tree.
    pub(crate) depth: usize,
    /// What this row is: a session, or one of the tree's own lines.
    pub(crate) node: Node,
    /// Whether this row's rows are drawn under it - `None` for a row that has none in this
    /// branch, `Some(open)` for one that folds. The glyph and the enumeration are this one answer.
    pub(crate) open: Option<bool>,
}

/// How far up a parent chain this tree will walk before it decides the chain loops. A child is
/// always created after its parent, so a real chain is short; this is a store somebody edited.
pub(crate) const MAX_CHAIN: usize = 64;

/// How far in a row is drawn. The renderer caps the indent at three steps (rano's tree list,
/// and a name pushed four steps in is off the right edge), and the built-in `sessions` layer caps
/// at the same number - one picture, two drawings.
const MAX_INDENT: usize = 3;

/// The project a workspace belongs to: the top level of the session list, and the one rule a worktree needs so that
/// it is not a project of its own. A session's workspace is whatever the daemon serves, so a checkout, a child's
/// worktree and a review clone all resolve to the repository that owns them.
pub(crate) fn project_of(workspace: &str) -> Project {
    let path = workspace.trim_end_matches('/');
    if path.is_empty() {
        return Project {
            key: String::new(),
            name: NO_WORKSPACE.to_string(),
        };
    }
    let segs: Vec<&str> = path.split('/').collect();
    let Some(at) = segs.iter().rposition(|s| *s == WORKTREES) else {
        return named(path);
    };
    let above = segs[..at].join("/");
    // `…/.claude/worktrees/agent-x` - the worktrees folder is the repository's own, so what
    // precedes `.claude` IS the project.
    if segs.get(at.wrapping_sub(1)) == Some(&".claude") {
        let repo = segs[..at - 1].join("/");
        return named(&repo);
    }
    let after = &segs[at + 1..];
    match after.len() {
        // `…/worktrees/<repository>/agent-x` - one folder per repository, for an org folder of
        // several, so the repository is named here.
        n if n >= 2 => named(&format!("{above}/{}", after[0])),
        // `…/letibot/worktrees/agent-x` - the folder is named after the repository inside it.
        1 => {
            let name = above.rsplit('/').next().unwrap_or_default();
            named(&format!("{above}/{name}"))
        }
        // `…/worktrees` itself: nothing below it to name a repository by.
        _ => named(&above),
    }
}

/// A project from its key, with the name a person reads.
fn named(key: &str) -> Project {
    Project {
        key: key.to_string(),
        name: project_name(key),
    }
}

/// What a project is called: the folder's name, which is what the operator asked for - *"shown
/// by its name, not its full path"*.
fn project_name(key: &str) -> String {
    if key.is_empty() {
        return NO_WORKSPACE.to_string();
    }
    key.rsplit('/').next().unwrap_or(key).to_string()
}

/// What a session with no workspace at all is drawn under - a daemon too old to say, or a session
/// whose row predates the column. It is a name, not a group invented for nothing.
const NO_WORKSPACE: &str = "(no workspace)";

/// The folder `task_start` puts a worktree in, spelled once.
const WORKTREES: &str = "worktrees";

/// The fold key of a project's `system` branch.
pub(crate) fn system_key(project: &str) -> String {
    format!("{project}/system")
}

/// The fold key of one group under `system`.
pub(crate) fn group_key(project: &str, kind: SystemKind) -> String {
    format!("{}/system/{}", project, kind.title())
}

/// The live panes, as a person reads them: `1 `!term mc`, 2 `!term nano``.
pub(crate) fn panes_said(list: &[letibot_sessionlog::PaneBrief]) -> String {
    list.iter()
        .map(|b| format!("{} `!term {}`", b.pane, b.command))
        .collect::<Vec<_>>()
        .join(", ")
}
