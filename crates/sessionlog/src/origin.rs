//! **Who a session is for** — the operator's own conversation, or one of the kinds of session a
//! harness makes for itself — and the one rule every list of sessions is grouped by.
//!
//! # The operator's report, which is the whole of this module's reason
//!
//! *"in session view the automatic sessions must be grouped somehow, so my vs auto. otherwise im
//! lost in subagents."* A daemon on this box holds one conversation of theirs and thirty rows of
//! work it did on that conversation's behalf — task children at three depths, the review queue's
//! own session and its reviewers, whatever a package was told to start, the leftovers of
//! `--prompt` runs — and a picker that draws them in one column ordered by *when something last
//! happened in it* puts a reviewer above the thing the person is actually doing.
//!
//! # What the rule reads, and what it deliberately does not
//!
//! Two stored facts and one derivation, in this order:
//!
//! 1. **`session.origin`** ([`SessionOrigin`]) — written once, by the daemon, when the session is
//!    made. It is the only thing that can say *a package started this* or *this is a one-shot's*.
//! 2. **`parent_session_id`** — a `task` child is somebody's subagent, at any depth. This is the
//!    mark the tree already uses and it needs no column.
//! 3. **The review queue's id** ([`crate::is_gatekeeper_session`]) — a **fallback**, for the rows
//!    written before `origin` existed. Preferring the field and falling back to the spelling is
//!    the rule the brief asks for; the spelling is kept because the id is derived from the
//!    workspace (`crate::gatekeeper_session`) and is stable for the life of a folder.
//!
//! What it does NOT read is the title, which is the operator's to change at any moment — a
//! grouping rule that a `/rename` can move a session out of is a rule that fights its user.
//!
//! # The queue propagates up, and nothing else does
//!
//! A reviewer is a `task` child of the queue's session, so by the parent rule it would be a
//! *subagent* — and *subagents* is where the operator's own children belong. So the root of the
//! chain decides for the whole chain **when that root is the queue**: a review, and a review of a
//! review, is the review queue's work and is drawn with it. Every other chain is decided by the
//! row itself: a task child of the operator's conversation is a subagent, however deep.

use crate::is_gatekeeper_session;

/// **The identity the MCP bridge attaches under** — `letibot-mcp`, the only head on this box
/// that is a program holding a session rather than a person's terminal.
///
/// Kept here rather than in the bridge because two crates have to agree about it: the bridge
/// sends it on every `ATTACH`, and the daemon names it on the session's row so a picker can tell
/// a package's conversation from the operator's (see [`SessionOrigin::of_opener`]). One string,
/// one definition, and `letibot-bridge`'s own `IDENTITY` is this constant.
pub const MCP_IDENTITY: &str = "letibot-mcp";

/// **How a session came to exist**, as the daemon wrote it down.
///
/// A closed vocabulary rather than a free string: a picker groups by it, and a value nobody
/// knows would be a row that belongs to no group at all.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// **The operator's own** — a `leticode` start, `/new`, a `--continue`, a head that asked
    /// over the socket. The default, and the answer for every row written before this column
    /// existed that nothing else claims: before it, every row was one of these.
    #[default]
    Operator,
    /// **A package started it** — a head that is not a terminal, today `letim`'s bridge
    /// ([`MCP_IDENTITY`]). Still a conversation: the person attached to it afterwards is the
    /// operator, and they may drive it for hours. It is grouped apart because *they* did not
    /// open it, and six of them sit among the two they did.
    Package,
    /// **The review queue's own session** — started by the daemon at startup
    /// (`harnessd`'s `sessions::gatekeeper`), one per workspace, whose board is the queue.
    Queue,
    /// **A one-shot's** — `harnessd --prompt …`, which opens a session, runs the steps, prints
    /// the answers and exits. The session's rows stay, and a run a day for a month is thirty
    /// rows in a list nobody asked for.
    OneShot,
}

impl SessionOrigin {
    /// The store's spelling — `session.origin`, and the wire's (the brief carries it as this
    /// string, so a head sees the same word the database holds).
    pub fn as_str(self) -> &'static str {
        match self {
            SessionOrigin::Operator => "operator",
            SessionOrigin::Package => "package",
            SessionOrigin::Queue => "queue",
            SessionOrigin::OneShot => "one_shot",
        }
    }

    /// The store's spelling read back. An unknown word is `None` — *this build cannot say who
    /// that session was for* — rather than a guess, because a guess here is a session drawn in
    /// the wrong group.
    pub fn parse(s: &str) -> Option<Self> {
        // `one-shot` accepted as well as the store's `one_shot`: the column is ours, but a
        // hand-written row or a future rename is not worth a wrong group.
        match s {
            "operator" => Some(SessionOrigin::Operator),
            "package" => Some(SessionOrigin::Package),
            "queue" => Some(SessionOrigin::Queue),
            "one_shot" | "one-shot" => Some(SessionOrigin::OneShot),
            _ => None,
        }
    }

    /// **What a head that asked for a session is**, from the `ATTACH` it opened with.
    ///
    /// Keyed on the **identity** and not the kind, and the kind is not an oversight: the
    /// protocol's kinds are `tui`, `remote`, `flowy`, `acp`, and `remote` is *every* head that is
    /// not a terminal — the bridge's is `remote`, and so is the throwaway connection the head
    /// opens to ask a daemon to stop (`stop-force`), which never makes a session. The identity
    /// is the name a person recognises in the decision corpus, and it is what makes `letim`'s
    /// sessions the only ones that land in the packages group.
    pub fn of_opener(_kind: &str, identity: &str) -> Self {
        if identity == MCP_IDENTITY {
            SessionOrigin::Package
        } else {
            SessionOrigin::Operator
        }
    }
}

/// **The groups a session list is drawn in** — *mine* first, then one group per automatic kind.
///
/// The order is the point: the operator scrolls a list to find their own conversations, so those
/// are above every fold and are never folded themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionGroup {
    /// The operator's own conversations.
    Mine,
    /// `task` children, at any depth.
    Subagents,
    /// The review queue's session and everything it spawned.
    ReviewQueue,
    /// Conversations a package started.
    Packages,
    /// What a `--prompt` run left behind.
    OneShot,
}

impl SessionGroup {
    /// Every group, in the order a list draws them. `Mine` is not a fold — the rows are drawn
    /// under it — and the four after it are.
    pub const ALL: [SessionGroup; 5] = [
        SessionGroup::Mine,
        SessionGroup::Subagents,
        SessionGroup::ReviewQueue,
        SessionGroup::Packages,
        SessionGroup::OneShot,
    ];

    /// The word a group's row is labelled with.
    pub fn title(self) -> &'static str {
        match self {
            SessionGroup::Mine => "mine",
            SessionGroup::Subagents => "subagents",
            SessionGroup::ReviewQueue => "review queue",
            SessionGroup::Packages => "packages",
            SessionGroup::OneShot => "one-shots",
        }
    }

    /// **One line saying what the group is**, drawn under its row — a fold whose contents a
    /// reader has to guess at is a fold they will not open.
    pub fn legend(self) -> &'static str {
        match self {
            SessionGroup::Mine => "the conversations you opened",
            SessionGroup::Subagents => "task children, and their children",
            SessionGroup::ReviewQueue => "the review queue's session and its reviewers",
            SessionGroup::Packages => "conversations a package started (letim)",
            SessionGroup::OneShot => "what a `--prompt` run left behind",
        }
    }

    /// **Whether this group is the operator's own** — the one group that is not a fold.
    pub fn is_mine(self) -> bool {
        self == SessionGroup::Mine
    }
}

/// **One session, as the grouping rule reads it** — the facts a row carries that say what it is.
#[derive(Debug, Clone, Copy)]
pub struct GroupRow<'a> {
    pub id: &'a str,
    /// `Some` for a subagent: the session that spawned it.
    pub parent: Option<&'a str>,
    pub origin: SessionOrigin,
    /// **The seat the row recorded**, when the reader has it — `gatekeeper` for a review. `None`
    /// where the list a picker is drawn from does not carry one (`SessionBrief` has no role, which
    /// is the same gap `App::fold_subagents` fills with the title), and then [`GATEKEEPER_SEAT`]'s
    /// mark is simply unavailable and the title's is used instead.
    pub role: Option<&'a str>,
    /// The session's title, for the brief cut that a reviewer's row is recognised by. Empty for an
    /// unnamed session.
    pub title: &'a str,
}

/// How far up a parent chain the rule will walk before it decides the chain loops. A child is
/// always created after its parent, so a real chain is short; this is a store somebody edited.
const MAX_DEPTH: usize = 64;

/// **Which group `id` is drawn in**, from every row in the list.
///
/// `rows` is the whole list because one question cannot be answered from one row: *is this the
/// review queue's work* is a question about its ROOT — and, for a reviewer whose row sits straight
/// on the operator's own conversation (the layout before the queue had a session of its own, and
/// still the shape a store written by an older daemon holds), a question about the row ITSELF.
/// That second shape is why this reads the row's seat and title as well as its id: `s-gk-0` is not
/// `s-gatekeeper-*`, and a rule that only walked the chain left ten reviewers in `subagents` and
/// put them back in the list the operator was told they were gone from.
///
/// A row the list does not hold — a child whose parent is gone — is grouped by what its own row
/// says, which is the honest answer: an orphan is not a reviewer any more than it is a
/// conversation.
pub fn group_of(rows: &[GroupRow<'_>], id: &str) -> SessionGroup {
    // The chain, from this row to its root, looking for the queue on the way.
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Some(row) = rows.iter().find(|r| r.id == cur) else {
            break;
        };
        if is_queue_work(row) {
            return SessionGroup::ReviewQueue;
        }
        match row.parent {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    let Some(row) = rows.iter().find(|r| r.id == id) else {
        return SessionGroup::Mine;
    };
    // **The parent edge before the origin**, because it is the mark the tree already draws:
    // a child a package spawned is still a subagent, and a reviewer whose row predates the
    // column is still a subagent's parent's reader — the queue test above has already taken
    // the reviewers, so what is left here is the operator's own children.
    if row.parent.is_some() {
        return SessionGroup::Subagents;
    }
    match row.origin {
        SessionOrigin::Operator => SessionGroup::Mine,
        SessionOrigin::Package => SessionGroup::Packages,
        SessionOrigin::OneShot => SessionGroup::OneShot,
        SessionOrigin::Queue => SessionGroup::ReviewQueue,
    }
}

/// **Is this row the review queue's work?** — three marks, and a row needs one of them.
///
/// * **The recorded origin** (`SessionOrigin::Queue`) — the fact, for every row written since
///   schema 33 and for the reviewer children a daemon marks.
/// * **The id** ([`is_gatekeeper_session`]) — the queue's own session, recognised by the spelling
///   that is derived from the workspace, which is the fallback for rows that predate the column.
/// * **The seat or the title** — a REVIEWER whose row is parented on something other than the
///   queue's session. Its seat is `gatekeeper` (`Seat::Gatekeeper`'s store spelling, and the
///   mark `harnessd`'s `is_conversation` reads) and its title is cut from the review brief
///   (`GATEKEEPER_TITLE_PREFIX`, the mark a head has when the list it holds carries no role).
///   Either is enough: a row renamed by the operator keeps its seat, and one rebuilt for a head
///   keeps its title.
fn is_queue_work(row: &GroupRow<'_>) -> bool {
    row.origin == SessionOrigin::Queue
        || is_gatekeeper_session(row.id)
        || row.role == Some(crate::GATEKEEPER_SEAT)
        || row.title.starts_with(crate::GATEKEEPER_TITLE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row as the rule reads it, from the three facts most tests here need. The seat and the
    /// title are set by the tests that are about them — `seat` and `titled`.
    struct Seed {
        id: String,
        parent: Option<String>,
        origin: SessionOrigin,
        role: Option<String>,
        title: String,
    }

    fn seed(id: &str, parent: Option<&str>, origin: SessionOrigin) -> Seed {
        Seed {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            origin,
            role: None,
            title: String::new(),
        }
    }

    /// A seeded row with a seat on it — `gatekeeper` for a review.
    fn seated(mut s: Seed, role: &str) -> Seed {
        s.role = Some(role.to_string());
        s
    }

    /// A seeded row with a title on it — a reviewer's is cut from the review brief.
    fn titled(mut s: Seed, title: &str) -> Seed {
        s.title = title.to_string();
        s
    }

    fn rows(all: &[Seed]) -> Vec<GroupRow<'_>> {
        all.iter()
            .map(|s| GroupRow {
                id: &s.id,
                parent: s.parent.as_deref(),
                origin: s.origin,
                role: s.role.as_deref(),
                title: &s.title,
            })
            .collect()
    }

    #[test]
    fn a_conversation_the_operator_opened_is_theirs() {
        let all = [seed("s-a", None, SessionOrigin::Operator)];
        assert_eq!(group_of(&rows(&all), "s-a"), SessionGroup::Mine);
    }

    #[test]
    fn a_task_child_is_a_subagent_at_any_depth() {
        let all = [
            seed("s-a", None, SessionOrigin::Operator),
            seed("s-child", Some("s-a"), SessionOrigin::Operator),
            seed("s-grandchild", Some("s-child"), SessionOrigin::Operator),
        ];
        let rows = rows(&all);
        assert_eq!(group_of(&rows, "s-child"), SessionGroup::Subagents);
        assert_eq!(group_of(&rows, "s-grandchild"), SessionGroup::Subagents);
        assert_eq!(group_of(&rows, "s-a"), SessionGroup::Mine);
    }

    #[test]
    fn the_review_queues_session_and_its_reviewers_are_one_group() {
        let queue = crate::gatekeeper_session_for_key("0123456789ab");
        let reviewer = format!("{queue}-sub-1791759365511364772");
        let all = [
            seed("s-a", None, SessionOrigin::Operator),
            seed(&queue, None, SessionOrigin::Queue),
            seed(&reviewer, Some(&queue), SessionOrigin::Operator),
            // A one-shot's leftover is a conversation of nobody's, not a reviewer's.
            seed("s-shot", None, SessionOrigin::OneShot),
        ];
        let rows = rows(&all);
        assert_eq!(group_of(&rows, &queue), SessionGroup::ReviewQueue);
        assert_eq!(
            group_of(&rows, &reviewer),
            SessionGroup::ReviewQueue,
            "a reviewer is the queue's work, not one of the operator's subagents"
        );
        assert_eq!(group_of(&rows, "s-shot"), SessionGroup::OneShot);
        assert_eq!(group_of(&rows, "s-a"), SessionGroup::Mine);
    }

    /// **A reviewer parented straight on the operator's conversation** — the layout a daemon from
    /// before the queue had a session of its own wrote, and the shape `continue_picks` builds. Its
    /// id is `s-gk-0`, not `s-gatekeeper-*`, and its parent is `mine`, so the only marks that say
    /// what it is are on its own row: the seat, and the brief its title was cut from.
    ///
    /// MEASURED 2026-10-26 on the Linux gate: with the chain test alone these landed in
    /// `subagents`, and `--list-sessions` offered ten reviewers that the operator had been told
    /// were gone from it.
    #[test]
    fn a_reviewer_parented_on_a_conversation_is_the_queues_by_its_own_row() {
        let brief = format!(
            "{} a subagent has finished work on a branch",
            crate::GATEKEEPER_TITLE_PREFIX
        );
        let all = [
            seed("s-mine", None, SessionOrigin::Operator),
            // The seat is the store's mark, and the one `harnessd` reads.
            seated(
                seed("s-gk-0", Some("s-mine"), SessionOrigin::Operator),
                crate::GATEKEEPER_SEAT,
            ),
            // The title is the mark a head has, whose list carries no seat at all.
            titled(
                seed("s-gk-1", Some("s-mine"), SessionOrigin::Operator),
                &brief,
            ),
            // And a plain task child of the same conversation is still a subagent.
            seed("s-task", Some("s-mine"), SessionOrigin::Operator),
        ];
        let rows = rows(&all);
        assert_eq!(
            group_of(&rows, "s-gk-0"),
            SessionGroup::ReviewQueue,
            "a reviewer's SEAT says what it is, wherever it is parented"
        );
        assert_eq!(
            group_of(&rows, "s-gk-1"),
            SessionGroup::ReviewQueue,
            "and so does the brief its title was cut from, which is all a head has"
        );
        assert_eq!(
            group_of(&rows, "s-task"),
            SessionGroup::Subagents,
            "the operator's own children are not swept up with the reviewers"
        );
        assert_eq!(group_of(&rows, "s-mine"), SessionGroup::Mine);
    }

    /// **The fallback, for rows written before the column.** The queue's id is derived from the
    /// workspace and is stable, so a row with no `origin` is still recognised — which is what
    /// makes this an upgrade rather than a flag day.
    #[test]
    fn a_queue_session_written_before_the_column_is_still_the_queue() {
        let queue = crate::gatekeeper_session_for_key("0123456789ab");
        let all = [seed(&queue, None, SessionOrigin::Operator)];
        assert_eq!(
            group_of(&rows(&all), &queue),
            SessionGroup::ReviewQueue,
            "the id convention is the fallback that keeps old rows out of *mine*"
        );
    }

    #[test]
    fn a_packages_conversation_is_grouped_apart_and_its_children_are_subagents() {
        let all = [
            seed("s-pkg", None, SessionOrigin::Package),
            seed("s-pkg-child", Some("s-pkg"), SessionOrigin::Operator),
        ];
        let rows = rows(&all);
        assert_eq!(group_of(&rows, "s-pkg"), SessionGroup::Packages);
        assert_eq!(
            group_of(&rows, "s-pkg-child"),
            SessionGroup::Subagents,
            "the parent edge wins: a child of a package's conversation is a subagent"
        );
    }

    /// A row the list does not hold is decided from what it says, and a chain that loops is not
    /// walked for ever.
    #[test]
    fn an_orphan_and_a_looping_chain_are_answered_rather_than_waited_on() {
        let all = [seed("s-orphan", Some("s-gone"), SessionOrigin::Operator)];
        assert_eq!(group_of(&rows(&all), "s-orphan"), SessionGroup::Subagents);

        let all = [
            seed("s-x", Some("s-y"), SessionOrigin::Operator),
            seed("s-y", Some("s-x"), SessionOrigin::Operator),
        ];
        assert_eq!(group_of(&rows(&all), "s-x"), SessionGroup::Subagents);
    }

    #[test]
    fn the_bridges_identity_is_the_packages_mark_and_nothing_else_is() {
        assert_eq!(
            SessionOrigin::of_opener("remote", MCP_IDENTITY),
            SessionOrigin::Package
        );
        assert_eq!(
            SessionOrigin::of_opener("tui", "dead"),
            SessionOrigin::Operator
        );
        // The head that asks a daemon to stop is `remote` too, and it is not a package.
        assert_eq!(
            SessionOrigin::of_opener("remote", "stop-force"),
            SessionOrigin::Operator
        );
    }

    #[test]
    fn every_origin_round_trips_through_the_stores_spelling() {
        for o in [
            SessionOrigin::Operator,
            SessionOrigin::Package,
            SessionOrigin::Queue,
            SessionOrigin::OneShot,
        ] {
            assert_eq!(SessionOrigin::parse(o.as_str()), Some(o));
        }
        assert_eq!(SessionOrigin::parse("who knows"), None);
    }
}
