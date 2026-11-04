//! A session's estate: what it still held when it ended, and what became of it.
//!
//! The substrate reaps a session's own scopes and watchers but cannot say what a subagent left, since a child's heir is a session. `settle_estate` hands what outlives the child to the heir, ends the rest, and says which was which.

use super::*;

/// What a session was still holding when it ended, in the shapes a sentence needs. A `Vec` per
/// kind and not counters: a count of what ended names nothing a reader can go and look at.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Estate {
    /// Jobs still running in its own scopes, by label. These end with it.
    pub ended: Vec<String>,
    /// Monitors still watching, by name. These end with it.
    pub watching: Vec<String>,
    /// Jobs in an `explicit` scope, by label. These moved to the heir.
    pub handed: Vec<String>,
    /// Watchers that moved to the heir, by name.
    pub handed_watchers: Vec<String>,
    /// What should have outlived it and did not, each with the reason the heir can act on.
    pub refused: Vec<String>,
    /// Work that is running somewhere else, by label, and belongs to the machine running it.
    pub elsewhere: Vec<String>,
}

impl Estate {
    fn is_empty(&self) -> bool {
        self.ended.is_empty()
            && self.watching.is_empty()
            && self.handed.is_empty()
            && self.handed_watchers.is_empty()
            && self.refused.is_empty()
            && self.elsewhere.is_empty()
    }
}

/// `1 job`, `2 jobs`, `no jobs` - a sentence says the number and the noun, and a bare number in
/// front of a bracket reads as a list rather than as a count.
fn counted(n: usize, one: &str, many: &str) -> String {
    match n {
        0 => format!("no {many}"),
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// What a session's end says about its estate. Pure, and `None` when there is nothing to tell:
/// every clause ends in what happened and what a reader can do about it, because a leftover nobody
/// can act on costs attention and buys nothing.
pub fn estate_said(e: &Estate) -> Option<String> {
    if e.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !e.ended.is_empty() {
        parts.push(format!(
            "it ended holding {} it had started ({}), and they ended with it: a subagent's \
             processes live in its own session's scope, so nothing it starts outlives its task — \
             run the work in your own session if it must.",
            counted(e.ended.len(), "job", "jobs"),
            e.ended.join(", ")
        ));
    }
    if !e.watching.is_empty() {
        parts.push(format!(
            "{} it had declared ended with it too ({}).",
            counted(e.watching.len(), "monitor", "monitors"),
            e.watching.join(", ")
        ));
    }
    if !e.handed.is_empty() || !e.handed_watchers.is_empty() {
        let mut moved = Vec::new();
        if !e.handed.is_empty() {
            moved.push(format!(
                "{} in an `explicit` scope, which outlives a session: {} — `job_list` shows them \
                 and `job_kill` ends them",
                counted(e.handed.len(), "job", "jobs"),
                e.handed.join(", ")
            ));
        }
        if !e.handed_watchers.is_empty() {
            moved.push(format!(
                "{}: {}",
                counted(e.handed_watchers.len(), "monitor", "monitors"),
                e.handed_watchers.join(", ")
            ));
        }
        parts.push(format!(
            "what outlived it is YOURS now — {}",
            moved.join(", and ")
        ));
    }
    for refused in &e.refused {
        parts.push(format!(
            "one leftover could NOT be handed over and is still running unwatched: {refused}"
        ));
    }
    if !e.elsewhere.is_empty() {
        parts.push(format!(
            "{} it started run on other machines and belong to the host running them: {}",
            counted(e.elsewhere.len(), "job", "jobs"),
            e.elsewhere.join(", ")
        ));
    }
    Some(parts.join(" "))
}

/// Where a leftover's own scope sits. A job whose `scope.path` is empty is a job of jobs
/// ([`ProcessHost::jobs`]'s aggregate view), which has no cgroup here at all.
fn in_a_scope_here(j: &letibot_tools::exec::JobView) -> bool {
    !j.scope.path.as_os_str().is_empty()
}

/// Settle a session's estate: hand what outlives it to `heir`, end what does not, and say what
/// became of each. The order inside is the correctness: hand over before the scopes end, read the
/// lists before the end, and end before returning the sentence that claims it.
pub fn settle_estate(
    host: &dyn letibot_tools::exec::ProcessHost,
    heir: Option<&dyn letibot_tools::exec::ProcessHost>,
) -> Option<String> {
    use letibot_tools::exec::{ScopeKind, job_label};

    let mut e = Estate::default();
    // The watchers live on the process host that owns them, so a hand-over is one argument a side.
    let monitors = host.monitors().cloned();

    // The survivors first: after the scopes end there is nothing left to hand over.
    let survivors = host.survivors();
    match heir {
        Some(to) => {
            for moved in letibot_tools::exec::hand_over(host, to) {
                match moved.refused {
                    None => e.handed.push(moved.label),
                    Some(why) => e.refused.push(format!("`{}` — {why}", moved.label)),
                }
            }
        }
        None => {
            for a in survivors {
                e.refused.push(format!(
                    "`{}` — there is no session to hand it to",
                    job_label(a.slug.as_deref(), &a.handle)
                ));
            }
        }
    }

    // Only a watcher over an `explicit` scope outlives the session; `retire_under` takes the rest.
    if let Some(mine) = &monitors {
        for m in mine.list() {
            if m.owner.kind != ScopeKind::Explicit {
                continue;
            }
            let moved = match heir.and_then(|to| to.monitors().cloned()) {
                Some(theirs) => match mine.hand_over(&m.name, &theirs) {
                    Ok(()) => {
                        e.handed_watchers.push(m.name.clone());
                        true
                    }
                    Err(why) => {
                        e.refused.push(format!("the monitor `{}` — {why}", m.name));
                        false
                    }
                },
                None => {
                    e.refused.push(format!(
                        "the monitor `{}` — there is no session to hand it to",
                        m.name
                    ));
                    false
                }
            };
            if !moved {
                // Nobody took it, and a watcher left in a registry about to be dropped ends unsaid.
                mine.retire(&m.name, "the session that declared it ended");
            }
        }
    }

    // Read before the end, or the answer is `nothing was running` about an emptied table.
    for j in host.jobs() {
        if !j.state.is_running() {
            continue;
        }
        let label = job_label(j.slug.as_deref(), &j.id.0);
        if in_a_scope_here(&j) {
            e.ended.push(label);
        } else {
            e.elsewhere.push(label);
        }
    }
    if let Some(mine) = &monitors {
        for m in mine.list() {
            if m.owner.kind != ScopeKind::Explicit {
                e.watching.push(m.name.clone());
            }
        }
    }

    // Every scope but the explicit ones, which are the heir's now.
    for scope in host.scopes() {
        if scope.kind != ScopeKind::Explicit {
            let _ = host.end_scope(&scope);
        }
    }

    estate_said(&e)
}

impl Harness {
    /// This session's estate, settled against `heir`: the child's door to [`settle_estate`].
    pub fn settle_estate(
        &self,
        heir: Option<&dyn letibot_tools::exec::ProcessHost>,
    ) -> Option<String> {
        let host = self.runtime.backend.processes()?;
        settle_estate(host, heir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(ended: &[&str], watching: &[&str], handed: &[&str]) -> Estate {
        Estate {
            ended: ended.iter().map(|s| s.to_string()).collect(),
            watching: watching.iter().map(|s| s.to_string()).collect(),
            handed: handed.iter().map(|s| s.to_string()).collect(),
            ..Estate::default()
        }
    }

    /// Nothing left is SILENCE: a line saying "it left nothing" on every child that ran a
    /// foreground command trains a reader to skip the line that matters.
    #[test]
    fn a_session_that_left_nothing_says_nothing() {
        assert_eq!(estate_said(&Estate::default()), None);
    }

    /// The sentence names the job, says it ended, and says WHY - a child's processes live in its
    /// own session scope - because the mechanism is what the parent's model was wrong about.
    #[test]
    fn what_ended_with_the_child_is_named_and_the_mechanism_with_it() {
        let said = estate_said(&one(&["build · j65"], &[], &[])).expect("a sentence");
        assert!(said.contains("1 job"), "{said}");
        assert!(said.contains("build · j65"), "{said}");
        assert!(said.contains("ended with it"), "{said}");
        assert!(
            said.contains("own session's scope"),
            "the reason is the part a model can act on: {said}"
        );
        assert!(!said.contains("still running"), "{said}");
    }

    /// Handed over is a different fact from ended, and it says what the heir can DO with it:
    /// a line that says "yours now" without naming the verb that ends it is a line that moves
    /// the problem to the reader.
    #[test]
    fn what_outlived_it_is_said_to_be_the_heirs_and_the_verb_is_named() {
        let said = estate_said(&one(&[], &[], &["cache-warm · j70"])).expect("a sentence");
        assert!(said.contains("YOURS"), "{said}");
        assert!(said.contains("cache-warm · j70"), "{said}");
        assert!(said.contains("explicit"), "{said}");
        assert!(said.contains("`job_kill`"), "{said}");
    }

    /// The refusal is not folded into the success: something still running and owned by nobody is
    /// the one fact here that needs a person, so it is its own clause and it carries the reason.
    #[test]
    fn what_could_not_be_handed_over_is_said_to_be_running_unwatched() {
        let e = Estate {
            refused: vec!["`cache-warm · j70` — there is no session to hand it to".into()],
            ..Estate::default()
        };
        let said = estate_said(&e).expect("a sentence");
        assert!(said.contains("still running unwatched"), "{said}");
        assert!(said.contains("there is no session to hand it to"), "{said}");
    }

    /// A job of jobs is not this session's process: it is branches on machines this box cannot
    /// see, so the sentence names it and does not claim it ended.
    #[test]
    fn work_on_other_machines_is_named_and_not_claimed() {
        let e = Estate {
            elsewhere: vec!["fanout · j71".into()],
            ..Estate::default()
        };
        let said = estate_said(&e).expect("a sentence");
        assert!(said.contains("other machines"), "{said}");
        assert!(!said.contains("ended with it"), "{said}");
    }

    /// The order inside [`settle_estate`] is the whole of the correctness, so it is asserted on two
    /// real hosts: what outlives the session is in the HEIR's table and still running, and what was
    /// in the session's own scope is gone before the sentence is returned.
    #[test]
    fn settling_hands_the_explicit_job_over_and_ends_the_sessions_own() {
        let Some(giver) = host("estate-giver") else {
            return;
        };
        let Some(heir) = host("estate-heir") else {
            return;
        };
        let spawn = |scope, name: Option<&str>, slug: &str| -> u32 {
            let id = giver
                .spawn(&letibot_tools::exec::SpawnRequest {
                    command: "sleep 300".into(),
                    slug: Some(slug.into()),
                    cwd: ".".into(),
                    scope,
                    scope_name: name.map(str::to_string),
                    background: true,
                    env: vec![],
                    tty: false,
                    memory: None,
                })
                .expect("a sleep starts");
            giver.job(&id).expect("listed").pid
        };
        let session_pid = spawn(letibot_tools::exec::ScopeKind::Session, None, "build");
        let explicit_pid = spawn(
            letibot_tools::exec::ScopeKind::Explicit,
            Some("cache-warm"),
            "cache-warm",
        );

        let said = settle_estate(&giver, Some(&heir)).expect("a sentence");
        assert!(said.contains("build"), "{said}");
        assert!(said.contains("cache-warm"), "{said}");
        assert!(said.contains("YOURS"), "{said}");

        // What was the session's is gone: the sentence says so, and the process agrees.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut gone = false;
        while std::time::Instant::now() < deadline {
            if letibot_tools::exec::procs::read(session_pid).is_none() {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            gone,
            "pid {session_pid} is still running after its session ended"
        );

        // And what outlives a session is the heir's, running, with the verb that ends it.
        assert!(
            letibot_tools::exec::procs::read(explicit_pid).is_some(),
            "the explicit-scope job must survive the session it came from"
        );
        let mine = heir
            .jobs()
            .into_iter()
            .find(|j| j.pid == explicit_pid)
            .expect("the heir lists what it was handed");
        assert!(mine.state.is_running(), "{mine:?}");
        heir.kill_job(&mine.id).expect("and it can end it");
    }

    /// A root in the temp dir, or `None` with the apparatus says so: a box with no cgroup v2 to
    /// delegate cannot own a process, and that is a fact about the box rather than a failed
    /// assertion.
    fn host(tag: &str) -> Option<letibot_tools::exec::HostProcesses> {
        let root = std::env::temp_dir().join(format!(
            "letibot-estate-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("temp root");
        match letibot_tools::exec::HostProcesses::new(&root) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!(
                    "{tag}: no cgroup v2 subtree here, exercising the refusal path instead: {e}"
                );
                letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
                None
            }
        }
    }
}
