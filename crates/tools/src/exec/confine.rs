//! Layer 1's other half: **project-scoped namespaces**, so the boundary bounds a
//! *view* and not only a lifetime.
//!
//! [`super::scope`] owns how long a process lives. This module owns what it can
//! see. `docs/boundary-and-adjudication.md` §4: *"Mount (a project-rooted
//! filesystem view, so the secret is not merely unreadable but **absent**), PID (so
//! the process cannot see or signal the operator's daemon or the model server),
//! network (so egress is a decision rather than a default), and user."*
//!
//! # Absence beats denial, and that is the whole design
//!
//! A denial needs a rule, a rule needs a path list, and §3 shows both directions a
//! path list is wrong in. An **absent** file needs none of it: `~/.ssh/id_rsa` is
//! not protected inside this boundary, it is *not there*, and there is no spelling
//! of `cat` that reaches a file the mount namespace does not contain.
//!
//! # Four rules, and the first one outranks the rest
//!
//! **1. A boundary that silently is not there is worse than none.** So there are
//! three states and never two:
//!
//! | state | what it is | what a spawn does |
//! |---|---|---|
//! | [`Bwrap`] | confinement was asked for and **measured** | runs inside it |
//! | [`NoConfinement`] | confinement was asked for and the kernel, the policy or the helper refused | **refuses, naming what was missing** |
//! | [`Unconfined`] | confinement was **not** asked for | runs, and every disclosure says loudly that it is not confined |
//!
//! There is no fourth, and in particular there is no path from the second to the
//! third: a host that cannot confine does not quietly get the old behaviour.
//!
//! **2. Report the boundary you actually got, read rather than asserted.** This is
//! the rule the repo has already paid for twice — a startup banner claiming
//! `read-only tools` about a session with write tools, and
//! [`crate::backend::HostBackend::describe`] hard-coding `read-only` for the
//! writable backend. So [`Bwrap::probe`] does not assert that it unshared anything.
//! It **runs the real invocation** and reads `/proc/self/ns/*` from inside it, and
//! a namespace whose inode inside equals the harness's own is recorded as
//! [`NsState::NotEntered`] however the arguments read. [`Boundary::describe`]
//! formats that map. A partial boundary therefore describes itself as partial
//! because there is nothing else for it to describe.
//!
//! **3. The mount view is the enforcement point for §3's flow rule** — for files.
//! *Secret bytes may be consumed inside the boundary; they may never enter the
//! transcript and never leave it.* The first half is structural here and the
//! subtle case is `ssh`, which legitimately reads `~/.ssh/id_rsa`. See
//! [`Grant::AgentSocket`] and the `credentials` section below: the resolution is
//! that a credential is made **usable** by forwarding the agent that holds it, and
//! is never made **readable** by binding the key. There is no grant in this module
//! that puts a private key inside the view.
//!
//! **4. Absence must be legible.** A path outside the view produces `ENOENT`, and
//! a bare `ENOENT` is read by a model as *the file does not exist* — an
//! empty-haystack answer. [`Boundary::absence_notes`] turns it into *"not in this
//! session's filesystem view"*. It decides that **lexically**, against the view's
//! roots, and never by stat'ing the host: whether the file exists outside is not a
//! fact this note needs, and not a fact it should disclose.
//!
//! # `/tmp` belongs to a session, not to a command
//!
//! Rule 4 is about a path *outside* the view. `/tmp` is the one that is **inside** it and is
//! still not the host's, and for a long time it was a fresh tmpfs per command: safe, and a
//! trap. An agent writes its scratch there constantly — a heredoc patch in one call and
//! `git apply` in the next, a build log redirected and then read — and the second call answers
//! `No such file or directory` with nothing to say that this is the boundary and not a broken
//! shell (MEASURED 2026-10-27: `echo hello > /tmp/x` then `cat /tmp/x`, with `ls -d /tmp`
//! succeeding and `TMPDIR` empty). So a caller that knows its session may name one directory
//! ([`TmpView::Session`]) and have it bound at `/tmp`; the default stays the fresh tmpfs,
//! because a view that claims nothing about a session is still right for a caller that does not
//! have one. What does not change either way: **the host's `/tmp` is never bound**, at any
//! path, under any name — it holds other programs' sockets and whatever the operator was doing.
//!
//! # Credentials: what is usable without being readable
//!
//! The authorised case is real and a project-only view breaks it. Three mechanisms
//! were available and only one satisfies §3:
//!
//! | mechanism | usable | readable into context | verdict |
//! |---|---|---|---|
//! | bind `~/.ssh/id_rsa` into the view | yes | **yes** — one `cat` and it is in the transcript | rejected |
//! | bind `~/.ssh` read-only | yes | **yes**, and more of it | rejected |
//! | forward `$SSH_AUTH_SOCK` | yes | **no** — the agent signs, outside the boundary; only signatures cross | **this** |
//!
//! [`Grant::AgentSocket`] is the third. The key bytes stay in `ssh-agent`, which
//! is not in the view and not in the namespace; what enters is a unix socket over
//! which a challenge goes out and a signature comes back. That is §3's invariant
//! holding rather than being enforced.
//!
//! And the case that has **no** mechanism is stated rather than papered over: a
//! key that is not loaded into an agent cannot be used from inside this boundary,
//! and the answer is [`Grant::agent_from_env`]'s refusal — *load it into
//! `ssh-agent`; letibot forwards the agent, never the key* — not a bind. `ssh` also
//! needs [`Egress::Host`], which is a second explicit decision. Two declarations,
//! and neither makes a private key readable.
//!
//! # Why bubblewrap and not `unshare(2)` here
//!
//! `crates/tools/Cargo.toml` says *"No `libc`"*, and it means it — the whole crate
//! reaches the kernel through `/proc`, `std::process` and the unix extension
//! traits. A native implementation would need `unshare`, `pivot_root`, `mount` and
//! a uid_map write in the window between `fork` and `exec`, which is `libc` and
//! `unsafe` in the one place in the tree where a mistake is a boundary hole.
//!
//! The measurement that settled it: **this box cannot create a usable
//! unprivileged user namespace at all.** `kernel.apparmor_restrict_unprivileged_userns`
//! is `1`, so an unconfined process that calls `unshare(CLONE_NEWUSER)` is
//! transitioned into the `unprivileged_userns` profile, which denies
//! `CAP_SYS_ADMIN` — and `map_write` in the kernel requires `CAP_SYS_ADMIN` in the
//! new namespace, so the `uid_map` write fails with `EPERM` and the namespace is
//! inert. A native path would have been untestable on the machine it was written
//! on, which `docs/tool-design-brief.md` §3b calls the component that *"reports
//! success while delivering nothing"*.
//!
//! `/usr/bin/bwrap` has an AppArmor profile (`bwrap-userns-restrict`) that permits
//! exactly this, which is how the policy is *meant* to be satisfied on such a
//! host. So the helper is the mechanism, its absence is a named refusal, and its
//! result is measured rather than trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::ExecError;

/// The namespaces this boundary is made of. Ordered as they are declared, so
/// [`Boundary::describe`] lists them in the order the design argues them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Namespace {
    /// The one that makes the rest possible unprivileged.
    User,
    /// The project-rooted view. **The enforcement point.**
    Mount,
    /// The daemon, the model server and a sibling job become invisible.
    Pid,
    /// Egress becomes a decision rather than a default.
    Net,
    /// Shared memory and message queues, which are a channel between jobs.
    Ipc,
    /// Hostname. Cheap, and it stops a command renaming the operator's box.
    Uts,
    /// So `/proc/self/cgroup` does not hand out the path to its own reaper.
    Cgroup,
}

impl Namespace {
    /// The `/proc/self/ns/` entry, which is also the `readlink` target's prefix.
    pub fn proc_name(&self) -> &'static str {
        match self {
            Namespace::User => "user",
            Namespace::Mount => "mnt",
            Namespace::Pid => "pid",
            Namespace::Net => "net",
            Namespace::Ipc => "ipc",
            Namespace::Uts => "uts",
            Namespace::Cgroup => "cgroup",
        }
    }

    /// What entering this namespace buys, in the words a disclosure uses. Not a
    /// category — the sentence, because `describe()` is read by somebody deciding
    /// whether to seat `bash`.
    pub fn buys(&self) -> &'static str {
        match self {
            Namespace::User => "the rest of this is possible without root",
            Namespace::Mount => {
                "a project-rooted filesystem view: a secret outside it is ABSENT, not denied"
            }
            Namespace::Pid => {
                "the command cannot see or signal the daemon, the model server, or a sibling job"
            }
            Namespace::Net => "egress is a decision rather than a default",
            Namespace::Ipc => "no shared-memory channel to another job",
            Namespace::Uts => "the command cannot rename the host",
            Namespace::Cgroup => "`/proc/self/cgroup` does not hand out the path to its own reaper",
        }
    }

    /// The three without which there is no boundary worth the name.
    ///
    /// [`Namespace::Net`] is deliberately **not** here: `Egress::Host` is a
    /// legitimate declared configuration, and a required-namespace check that
    /// refused it would make the declaration unspellable rather than visible.
    pub const REQUIRED: [Namespace; 3] = [Namespace::User, Namespace::Mount, Namespace::Pid];

    /// Every namespace this module asks for, in declaration order.
    pub const ALL: [Namespace; 7] = [
        Namespace::User,
        Namespace::Mount,
        Namespace::Pid,
        Namespace::Net,
        Namespace::Ipc,
        Namespace::Uts,
        Namespace::Cgroup,
    ];
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.proc_name())
    }
}

/// A property of the boundary that is not a namespace but decides whether the
/// namespaces are worth anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SealKind {
    /// **Can a process inside create a new user namespace and step back out?**
    ///
    /// This is the hole a namespace boundary has by default and it is not
    /// theoretical: `unshare(CLONE_NEWUSER)` is permitted to an unprivileged
    /// process, the caller gets full capabilities *in the namespace it just made*,
    /// and `unshare(CLONE_NEWNS)` then succeeds — so a child can build itself a
    /// mount namespace the parent did not authorise. Mount, PID and network
    /// namespaces entered by a parent are not a boundary if the child can leave
    /// them.
    ///
    /// The survey's `grok-build` closes this with a second seccomp-BPF filter over
    /// the mount and namespace API. bubblewrap has it as a flag pair —
    /// `--disable-userns` sets the sandbox userns's `max_user_namespaces` to zero,
    /// and `--assert-userns-disabled` makes the helper **fail** rather than proceed
    /// if that did not take, which is the same fail-closed shape as the reference
    /// launcher's `restrict_self` (`deepseek-harness`
    /// `native/system/packages/entry/src/main.c:229-234`: an unusable mechanism is
    /// a refusal, never an unconfined exec).
    ///
    /// Both are declared, and then the property is **measured anyway** — see
    /// [`READBACK`]. A flag is a request.
    NestedNamespaces,
    /// `PR_SET_NO_NEW_PRIVS`. Without it a setuid binary reachable inside the view
    /// is an escalation, and the whole point of an unprivileged boundary is that
    /// there is nothing to escalate to.
    ///
    /// The reference launcher sets it **before** restricting
    /// (`main.c:248-253`) and the ordering is a correctness fact, not a style: an
    /// unprivileged `landlock_restrict_self` is rejected without it. bubblewrap
    /// does the equivalent itself; this reads the result rather than trusting that.
    NoNewPrivs,
}

impl SealKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SealKind::NestedNamespaces => "nested-namespaces",
            SealKind::NoNewPrivs => "no-new-privs",
        }
    }

    pub fn buys(&self) -> &'static str {
        match self {
            SealKind::NestedNamespaces => {
                "a process inside cannot `unshare` itself back out of the boundary"
            }
            SealKind::NoNewPrivs => "a setuid binary inside cannot raise privilege",
        }
    }

    pub const ALL: [SealKind; 2] = [SealKind::NestedNamespaces, SealKind::NoNewPrivs];
}

impl std::fmt::Display for SealKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a seal holds, **tested rather than requested**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seal {
    /// A process inside tried the thing and could not do it.
    Held { how: String },
    /// It could. A fence with a gate in it is not a fence, and this refuses.
    Open { why: String },
    /// The test could not be run from inside, so the property is unknown — and an
    /// unknown property is reported as unknown, never as held.
    Unverified { why: String },
    /// **This mechanism has no such property to test.** Not `Held` (that would be a claim
    /// about a mechanism that is not there), not `Unverified` (it is not that the test was
    /// missed), and not `Open` (there is nothing to step through). macOS has no
    /// `PR_SET_NO_NEW_PRIVS`, so a policy boundary reports that seal here with what it has
    /// instead.
    NotApplicable { why: String },
}

impl Seal {
    pub fn held(&self) -> bool {
        matches!(self, Seal::Held { .. })
    }
    pub fn line(&self) -> String {
        match self {
            Seal::Held { how } => format!("held ({how})"),
            Seal::Open { why } => format!("**OPEN** — {why}"),
            Seal::Unverified { why } => format!("UNVERIFIED — {why}"),
            Seal::NotApplicable { why } => format!("NOT APPLICABLE on this platform — {why}"),
        }
    }
}

/// **Which family of machinery a measured boundary is made of.**
///
/// The two are not interchangeable and neither is a worse copy of the other: a
/// namespace boundary makes what is outside it **absent**, and a policy boundary
/// makes it **denied**. Both are measured before a command is run inside one, both
/// answer [`Boundary::complete`], and the difference is printed rather than averaged
/// away — see [`Boundary::describe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mechanism {
    /// Linux namespaces, entered through bubblewrap: `/proc/self/ns/*` read from
    /// inside, compared with the harness's own inodes, and the seals functionally
    /// tested. [`super::Bwrap`].
    Namespaces,
    /// A mandatory-access policy the kernel applies to the process: macOS's Seatbelt,
    /// through `sandbox-exec -f <profile>`. [`super::seatbelt::Seatbelt`].
    Policy,
}

impl Mechanism {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mechanism::Namespaces => "namespaces",
            Mechanism::Policy => "policy",
        }
    }
}

impl std::fmt::Display for Mechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// **A property a boundary can have that is not a namespace.**
///
/// The namespace table is the whole answer on Linux and no answer at all on macOS,
/// which has none — so a policy boundary describes itself with this instead. Both
/// families are asked the same questions, which is what stops one platform's
/// disclosure from being a claim the other platform never makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prop {
    /// A file outside the view cannot be reached. On Linux the mount namespace makes
    /// it **absent**; under a policy it is **denied**, and that difference is the one
    /// [`Boundary::describe`] says out loud rather than in a footnote.
    FileView,
    /// A process outside the boundary cannot be reached — a signal, or a lookup that
    /// would find it.
    ProcessIsolation,
    /// A process outside the boundary is **not there at all**. A PID namespace buys
    /// this; nothing on macOS does.
    ProcessAbsence,
    /// Egress is a decision rather than a default.
    Egress,
    /// A child cannot step out of the boundary its parent is in.
    Nested,
}

impl Prop {
    pub fn as_str(&self) -> &'static str {
        match self {
            Prop::FileView => "file-view",
            Prop::ProcessIsolation => "process-isolation",
            Prop::ProcessAbsence => "process-absence",
            Prop::Egress => "egress",
            Prop::Nested => "nested",
        }
    }

    /// What holding this buys, in the words a disclosure uses — the same question
    /// [`Namespace::buys`] answers, so the two tables read alike.
    pub fn buys(&self) -> &'static str {
        match self {
            Prop::FileView => {
                "the operator's credentials and profiles cannot be read, and a write outside \
                 the checkout cannot happen"
            }
            Prop::ProcessIsolation => {
                "the command cannot signal the daemon, the model server, or a sibling job"
            }
            Prop::ProcessAbsence => {
                "a process outside the boundary does not appear in a listing at all"
            }
            Prop::Egress => "egress is a decision rather than a default",
            Prop::Nested => {
                "a child cannot strip the boundary its parent is in, so wrapping a command is \
                 enough"
            }
        }
    }

    /// The three a policy boundary is not a boundary without. [`Prop::ProcessAbsence`]
    /// is deliberately **not** among them: no OS is allowed to be refused for a property
    /// it does not have the kernel for, and refusing it would make the honest answer
    /// (*disclosed, absent*) unspellable on the platform that has to give it.
    pub const REQUIRED_POLICY: [Prop; 3] = [Prop::FileView, Prop::ProcessIsolation, Prop::Nested];

    /// Every property a policy boundary reports, in the order a disclosure lists them.
    pub const ALL: [Prop; 5] = [
        Prop::FileView,
        Prop::ProcessIsolation,
        Prop::ProcessAbsence,
        Prop::Egress,
        Prop::Nested,
    ];
}

impl std::fmt::Display for Prop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one [`Prop`] measured, and the third variant is the important one.
///
/// [`PropState::Absent`] is **not** a failure: it is an OS that cannot give the
/// property at all, named so a reader is not left to infer it from a silence. It is
/// kept apart from [`PropState::Unverified`] for exactly the reason
/// `apparatus::applies` is kept apart from `apparatus::present` — one is *somebody
/// could have done this and did not*, the other is *nobody could*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropState {
    /// Measured holding, from inside, with how it was measured.
    Held { how: String },
    /// This operating system cannot give it, with what the kernel is missing.
    Absent { why: String },
    /// It could have been measured and was not. Reported as unknown, never as held.
    Unverified { why: String },
}

impl PropState {
    pub fn held(&self) -> bool {
        matches!(self, PropState::Held { .. })
    }

    pub fn line(&self) -> String {
        match self {
            PropState::Held { how } => format!("held ({how})"),
            PropState::Absent { why } => format!("**ABSENT — this OS cannot give it**: {why}"),
            PropState::Unverified { why } => format!("UNVERIFIED — {why}"),
        }
    }
}

/// What happened to one namespace, **as read back**, never as requested.
///
/// Three variants and the middle one is the reason there are not two: a namespace
/// shared on purpose and a namespace that failed to be entered look identical from
/// inside (the inode matches the harness's) and are completely different facts. A
/// boundary that collapsed them would report `Egress::Host` as a defect, or —
/// far worse — a failed unshare as a decision somebody made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NsState {
    /// Entered. The inode is the evidence: it is what `readlink /proc/self/ns/<n>`
    /// returned from **inside**, and it differs from the harness's own.
    Entered { inode: String },
    /// Shared with the harness because somebody declared it, with the declaration.
    SharedByDecision { why: String },
    /// Asked for and **not** entered. This is the state that refuses.
    NotEntered { why: String },
}

impl NsState {
    pub fn entered(&self) -> bool {
        matches!(self, NsState::Entered { .. })
    }

    /// One line for a disclosure.
    pub fn line(&self) -> String {
        match self {
            NsState::Entered { inode } => format!("entered ({inode})"),
            NsState::SharedByDecision { why } => {
                format!("SHARED WITH THE HOST by decision — {why}")
            }
            NsState::NotEntered { why } => format!("**NOT ENTERED** — {why}"),
        }
    }
}

/// Whether a confined command may reach the network, and it is a *decision*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// The default. An empty network namespace: no interfaces, no routes, no DNS.
    /// A `curl` inside it fails at the socket, which is what makes a `web_fetch`
    /// seam a seam rather than decoration.
    Denied,
    /// The host's network namespace, shared because somebody said so, with the
    /// sentence they said it in. This is what an authorised `ssh` or `git push`
    /// needs, and it is recorded as [`NsState::SharedByDecision`] so no disclosure
    /// can mistake it for a boundary that failed.
    Host { why: String },
}

impl Egress {
    pub fn allowed(&self) -> bool {
        matches!(self, Egress::Host { .. })
    }
}

/// What `$HOME` is inside the view.
///
/// It cannot be the operator's home — that is the directory the whole boundary
/// exists to leave outside — and it cannot be nothing, because a shell with no
/// writable `$HOME` breaks tools that have nothing to do with credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeView {
    /// A fresh tmpfs each run. The safe default, and the one with a **cost worth
    /// stating**: a build cache under `$HOME` (`~/.cargo/registry`, `~/.rustup`,
    /// `~/.npm`) is empty every time, so a confined `cargo build` re-downloads.
    /// [`Boundary::describe`] says so rather than letting somebody discover it as
    /// a mysterious slowdown.
    Tmpfs,
    /// A letibot-owned directory, bound read-write at its own path. Persistent
    /// across runs and **not** the operator's home.
    Dir(PathBuf),
}

/// What `/tmp` is inside the view.
///
/// **`/tmp` is where an agent's scratch lives**, and a filesystem that forgets it between two
/// calls makes every use of it look like a broken shell. MEASURED 2026-10-27: a session whose
/// tools ran inside this boundary wrote a probe to `/tmp/x` in one call and read it in the
/// next — *No such file or directory* — and the same shape hit a build log redirected to
/// `/tmp/clippy.log` and read back by a later call. Nothing announces it: `ls -d /tmp`
/// succeeds and `TMPDIR` is empty.
///
/// Nothing of the host's `/tmp` may become visible either way — it holds other programs'
/// sockets and whatever the operator was doing — so this is a choice between *a fresh
/// filesystem per command* and *one letibot-owned directory per session*, never a bind of the
/// host's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmpView {
    /// A fresh tmpfs per command: private to it, empty at its start, gone at its end. The
    /// default, because it claims nothing — and the one that silently breaks a two-call
    /// scratch idiom, so a caller that knows its session's directory should say so.
    PerCommand,
    /// One letibot-owned directory, bound read-write at `/tmp`, so a file one call of the
    /// session writes is there for the next call of **that** session and for no other.
    /// Created by [`Bwrap::probe`] if it is not there, like [`HomeView::Dir`].
    Session(PathBuf),
}

/// The sentence a [`Presence::Replaced`] for `/tmp` carries when a session's directory is
/// bound there — see [`TmpView`]. It says the thing an `ENOENT` under `/tmp` has to say, which
/// is no longer *this command started empty* and is still not *the host's*.
const SESSION_TMP_WHAT: &str = "this session's own directory, bound here: it survives between the calls of one session \
     and holds none of the host's files";

/// A path put inside the view on purpose, with the sentence that put it there.
///
/// Every variant carries a `why`, for the same reason [`super::Protected::why`]
/// does: a grant nobody can explain is a grant nobody can revoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// Readable inside the view. **And therefore readable into the transcript** —
    /// §3's second half is not enforced by this module, so a grant is a decision
    /// with that consequence and [`Boundary::describe`] prints the consequence next
    /// to the grant.
    ReadOnly { path: PathBuf, why: String },
    /// Writable inside the view, with the same consequence and one more.
    ReadWrite { path: PathBuf, why: String },
    /// **The credential mechanism.** A unix socket to an agent that holds a key,
    /// bound at [`AGENT_SOCKET`] with `SSH_AUTH_SOCK` pointing at it.
    ///
    /// This is the one shape that makes a credential *usable* without making it
    /// *readable*: the private key never enters the mount namespace, the signing
    /// happens in a process outside the boundary, and what crosses the socket is a
    /// challenge and a signature. `cat $SSH_AUTH_SOCK` returns nothing a
    /// transcript can use.
    AgentSocket { path: PathBuf, why: String },
}

impl Grant {
    pub fn path(&self) -> &Path {
        match self {
            Grant::ReadOnly { path, .. }
            | Grant::ReadWrite { path, .. }
            | Grant::AgentSocket { path, .. } => path,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Grant::ReadOnly { why, .. }
            | Grant::ReadWrite { why, .. }
            | Grant::AgentSocket { why, .. } => why,
        }
    }

    /// One line for a disclosure, and it names the consequence, not only the path.
    pub fn line(&self) -> String {
        match self {
            Grant::ReadOnly { path, why } => format!(
                "{} — read-only, READABLE INTO CONTEXT: {why}",
                path.display()
            ),
            Grant::ReadWrite { path, why } => format!(
                "{} — read-write, READABLE INTO CONTEXT and writable: {why}",
                path.display()
            ),
            Grant::AgentSocket { path, why } => format!(
                "{} — agent socket at {AGENT_SOCKET}, usable but NOT readable (the key never enters the view): {why}",
                path.display()
            ),
        }
    }

    /// The authorised-`ssh` grant, from `$SSH_AUTH_SOCK`, **or the refusal that
    /// names the mechanism nobody built**.
    ///
    /// This function is the whole of this module's answer to §3's subtle case, and
    /// the error path is the important half. If there is no agent, there is no way
    /// to use a key from inside this boundary, and the answer is *not* to bind the
    /// key: binding it is exactly the "quietly make the key readable to get ssh
    /// working" that the rule forbids. So the refusal says what to do instead.
    pub fn agent_from_env(why: impl Into<String>) -> Result<Grant, ExecError> {
        let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
            return Err(ExecError::NoCredentialMechanism(
                "`SSH_AUTH_SOCK` is not set, so there is no agent to forward. A key \
                 file is NOT an alternative: binding `~/.ssh/id_rsa` into the view \
                 would make it readable, and a readable key is one `cat` away from \
                 the transcript — which `docs/boundary-and-adjudication.md` §3 calls \
                 blocked, with no context and no authorisation that promotes \
                 it. Start an agent and load the key (`eval $(ssh-agent); ssh-add \
                 <key>`); letibot forwards the agent, never the key. Note that an \
                 authorised `ssh` needs a SECOND decision as well: `Egress::Host`, \
                 because the default network namespace has no route out."
                    .to_string(),
            ));
        };
        let path = PathBuf::from(sock);
        // Guard the fact, not the proxy: it has to *be* a socket. A regular file
        // called `agent.sock` would be a key by another name, and this is the one
        // place where letting that through defeats the mechanism.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| {
            ExecError::NoCredentialMechanism(format!(
                "`SSH_AUTH_SOCK` points at `{}`, which could not be read ({e}), so \
                 there is nothing to forward. Nothing was bound.",
                path.display()
            ))
        })?;
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return Err(ExecError::NoCredentialMechanism(format!(
                "`SSH_AUTH_SOCK` points at `{}`, which is not a socket ({:?}). Only a \
                 socket is forwardable: a socket is usable without being readable, \
                 and a file is not. Nothing was bound.",
                path.display(),
                meta.file_type()
            )));
        }
        Ok(Grant::AgentSocket {
            path,
            why: why.into(),
        })
    }
}

/// Where a forwarded agent socket appears inside the view. Fixed, so that
/// `SSH_AUTH_SOCK` inside does not leak the operator's `/run/user/1000/...` path.
pub const AGENT_SOCKET: &str = "/run/letibot/agent.sock";

/// Where a [`HomeView::Tmpfs`] `$HOME` appears inside the view.
pub const TMPFS_HOME: &str = "/run/letibot/home";

/// What a host path is when looked at from inside the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Bound in: the host's file *is* the sandbox's file.
    Inside,
    /// The path exists inside the view, but as a **fresh filesystem** — so nothing
    /// the host keeps there is visible, and a write does not reach the host.
    Replaced { what: &'static str },
    /// Not in the view at all. Absent, and no rule is what is stopping it.
    Outside,
}

/// The paths that exist inside the view but are **not** the host's, with what they
/// are instead. See [`Presence::Replaced`].
const REPLACED_ROOTS: &[(&str, &str)] = &[
    (
        "/tmp",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/var/tmp",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/run",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/proc",
        "this command's own procfs, showing only the processes in its PID namespace",
    ),
    ("/dev", "a minimal device set, not the host's `/dev`"),
];

/// Which paths are in the view, and therefore which are absent from it.
///
/// The **project** is the workspace root the daemon was given, bound read-write at
/// its own path. Keeping the path identical inside and out is deliberate: a
/// compiler's `error[E0433]: ... at crates/tools/src/lib.rs:12` has to name the
/// same file the `read` tool names, and rewriting the root to `/workspace` would
/// make every path in every tool result a translation the model has to do and will
/// sometimes get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSpec {
    /// The workspace root. Read-write, at its own path.
    pub project: PathBuf,
    /// The read-only system roots a command needs to be a command at all.
    pub system: Vec<PathBuf>,
    pub home: HomeView,
    /// What `/tmp` is — see [`TmpView`], and note that `/var/tmp` and `/run` stay fresh
    /// tmpfs filesystems either way.
    pub tmp: TmpView,
    pub grants: Vec<Grant>,
}

/// The read-only system paths bound by default.
///
/// `--ro-bind-try`, so a host that does not have one is not a failure: a merged
/// `/usr` host has no real `/lib`, and a host that does has a symlink. This list is
/// **not** a security decision — it is the answer to "what does `/bin/sh` need to
/// exist" — and the security decision is everything *not* on it, which is where
/// `$HOME`, `/root`, `/mnt`, `/srv`, `/opt`, the rest of `/etc` and every other
/// project live.
pub const SYSTEM_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc/alternatives",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/localtime",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/ca-certificates.conf",
    "/etc/nsswitch.conf",
    "/etc/passwd",
    "/etc/group",
    "/etc/terminfo",
];

/// The environment variables a confined command keeps.
///
/// The environment is cleared (`--clearenv`) and these are put back, which is
/// **absence beats denial applied to the environment**: an unset-list would need to
/// name `GITHUB_TOKEN`, `FLOWY_TOKEN`, `AWS_SECRET_ACCESS_KEY`, `ANTHROPIC_API_KEY`
/// and whatever was invented last week, and would be wrong in both directions the
/// same way `NEVER_WRITE` is. A keep-list is wrong in one direction only, and that
/// direction is *a command is missing a variable*, which is a visible failure with
/// a fix rather than a silent leak.
pub const KEEP_ENV: &[&str] = &["PATH", "TERM", "LANG", "LC_ALL", "TZ"];

impl ViewSpec {
    /// The default view: one project, the system roots, a tmpfs home, no grants.
    pub fn project_only(project: impl Into<PathBuf>) -> ViewSpec {
        ViewSpec {
            project: project.into(),
            system: SYSTEM_ROOTS.iter().map(PathBuf::from).collect(),
            home: HomeView::Tmpfs,
            tmp: TmpView::PerCommand,
            grants: Vec::new(),
        }
    }

    pub fn with_home(mut self, home: HomeView) -> ViewSpec {
        self.home = home;
        self
    }

    /// Give this session a `/tmp` that outlives a command — see [`TmpView`].
    pub fn with_tmp(mut self, tmp: TmpView) -> ViewSpec {
        self.tmp = tmp;
        self
    }

    pub fn granting(mut self, grant: Grant) -> ViewSpec {
        self.grants.push(grant);
        self
    }

    /// What a host path is, from inside the view. **Three answers, not two.**
    ///
    /// The middle one was found by a failing test and is the one that would have
    /// been a wrong note: `/tmp/x` is *in* the view — `/tmp` exists inside — but it
    /// is a **fresh tmpfs**, so a file the host has at `/tmp/x` is just as absent as
    /// one in `~/.ssh`, and for a completely different reason. Collapsing that into
    /// "inside" produces a bare `ENOENT` the model reads as an answer about the
    /// world; collapsing it into "outside" produces a note claiming the path cannot
    /// be reached, when in fact it can be created and used.
    ///
    /// **Lexical, on purpose.** Two reasons, and the second is the one that
    /// matters. First, it has to be answerable about a path that is not there, and
    /// a `stat` cannot tell "outside the view" from "does not exist anywhere".
    /// Second, a `stat` on the host would make the note a *disclosure*: it would
    /// say "this exists and you cannot see it", which is a fact about the
    /// operator's disk that the model did not have and does not need. What it needs
    /// is to know that `ENOENT` here means *boundary*, not *empty haystack*.
    pub fn classify(&self, path: &Path) -> Presence {
        if !path.is_absolute() {
            return Presence::Inside;
        }
        // The bound roots first, so a project that happens to live under `/tmp` is
        // Inside rather than Replaced.
        let inside = std::iter::once(self.project.as_path())
            .chain(self.system.iter().map(|p| p.as_path()))
            .chain(self.grants.iter().map(|g| g.path()));
        for root in inside {
            if path == root || path.starts_with(root) {
                return Presence::Inside;
            }
            // The chain of parent directories to a bound path exists inside the
            // view as empty directories, so `/home/dead` is *in* the view in the
            // sense that a listing of it succeeds. Saying it is outside would be a
            // note about a path the command could in fact see.
            if root.starts_with(path) {
                return Presence::Inside;
            }
        }
        if let HomeView::Dir(d) = &self.home
            && (path == d || path.starts_with(d) || d.starts_with(path))
        {
            return Presence::Inside;
        }
        for (p, what) in REPLACED_ROOTS {
            let p = Path::new(p);
            if path == p || path.starts_with(p) || p.starts_with(path) {
                // **`/tmp` is still not the host's, and it is no longer emptied between
                // commands when the session has a directory there.** Both halves matter to
                // the note an `ENOENT` under it produces, so the sentence follows the view
                // rather than the constant.
                let what = match &self.tmp {
                    TmpView::Session(_) if p == Path::new("/tmp") => SESSION_TMP_WHAT,
                    _ => what,
                };
                return Presence::Replaced { what };
            }
        }
        Presence::Outside
    }

    /// Is this absolute path outside the view entirely? A [`Presence::Replaced`]
    /// path is **not** outside: it exists and can be used, it is simply empty.
    pub fn outside(&self, path: &Path) -> bool {
        matches!(self.classify(path), Presence::Outside)
    }

    /// The roots a disclosure lists, in the order it lists them.
    pub fn summary(&self) -> String {
        let mut s = format!("project {} (rw)", self.project.display());
        s.push_str(&format!("; {} read-only system paths", self.system.len()));
        match &self.home {
            HomeView::Tmpfs => s.push_str(&format!(
                "; $HOME is a FRESH TMPFS at {TMPFS_HOME} (so a build cache under \
                 $HOME is empty every run)"
            )),
            HomeView::Dir(d) => s.push_str(&format!("; $HOME is {} (rw)", d.display())),
        }
        match &self.tmp {
            TmpView::PerCommand => {
                // "a new tmpfs", not "FRESH TMPFS": that phrase is the `$HOME` arm's, and a
                // disclosure is read for it to tell a throwaway home from a kept one.
                s.push_str(
                    "; /tmp is a new tmpfs for each command (so a file one call writes is gone \
                     in the next)",
                )
            }
            TmpView::Session(d) => s.push_str(&format!(
                "; /tmp is {} (rw), this session's own, so a file one call writes is there for \
                 the next",
                d.display()
            )),
        }
        if self.grants.is_empty() {
            s.push_str("; no grants");
        } else {
            for g in &self.grants {
                s.push_str(&format!("\n    grant: {}", g.line()));
            }
        }
        s
    }
}

/// The boundary that was **measured**, not the one that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// What did it, with its version. `bubblewrap 0.11.1`, not `namespaces`.
    pub mechanism: String,
    /// Which family of machinery the rest of this record is written in, so that a
    /// reader knows whether the namespace table or the property table is the answer.
    pub platform: Mechanism,
    pub view: ViewSpec,
    pub egress: Egress,
    /// One entry per [`Namespace::ALL`], read back from inside. Empty under
    /// [`Mechanism::Policy`], which has no namespaces to enter.
    pub ns: BTreeMap<Namespace, NsState>,
    /// One entry per [`SealKind::ALL`], **functionally tested** from inside.
    pub seals: BTreeMap<SealKind, Seal>,
    /// One entry per [`Prop::ALL`], measured from inside. Empty under
    /// [`Mechanism::Namespaces`], whose namespace table says the same things by name.
    pub props: BTreeMap<Prop, PropState>,
    pub measured_at: SystemTime,
}

impl Boundary {
    /// **An all-namespace boundary**, with no property table — the shape bubblewrap's
    /// probe fills in. A constructor so that adding a field to this record does not
    /// mean touching every test that builds one.
    pub fn namespaces(
        mechanism: impl Into<String>,
        view: ViewSpec,
        egress: Egress,
        ns: BTreeMap<Namespace, NsState>,
        seals: BTreeMap<SealKind, Seal>,
    ) -> Boundary {
        Boundary {
            mechanism: mechanism.into(),
            platform: Mechanism::Namespaces,
            view,
            egress,
            ns,
            seals,
            props: BTreeMap::new(),
            measured_at: SystemTime::now(),
        }
    }

    /// Every required property of this boundary's own mechanism held, **and no seal
    /// measured open**. The only honest success test, and the mirror of
    /// [`super::Reaping::clean`].
    ///
    /// What "required" means is the mechanism's: three namespaces on Linux, three
    /// properties under a policy. Asking a policy boundary for a PID namespace would
    /// be asking it for something no OS gives it and calling the answer a defect.
    ///
    /// A seal that could not be tested does not fail this — it makes the boundary
    /// *partial*, which is a different sentence in the disclosure. A seal measured
    /// **open** does fail it, because a boundary a child can `unshare` its way out
    /// of is the "silently is not there" case wearing the right flags.
    pub fn complete(&self) -> bool {
        let sealed = !self.seals.values().any(|s| matches!(s, Seal::Open { .. }));
        match self.platform {
            Mechanism::Namespaces => {
                Namespace::REQUIRED
                    .iter()
                    .all(|n| self.ns.get(n).map(|s| s.entered()).unwrap_or(false))
                    && sealed
            }
            Mechanism::Policy => {
                Prop::REQUIRED_POLICY
                    .iter()
                    .all(|p| self.props.get(p).map(|s| s.held()).unwrap_or(false))
                    && sealed
            }
        }
    }

    /// Something was asked for and did not happen, or could not be confirmed.
    /// **Not** the same as [`Boundary::complete`] being false: a boundary can be
    /// partial in a namespace that is not required, or in a seal that could not be
    /// tested from inside, and still confine.
    ///
    /// A [`PropState::Absent`] is deliberately not partial. It is an OS that cannot
    /// give the property, it is printed in full by [`Boundary::describe`], and
    /// counting it here would make "this machine has no PID namespace" read as the
    /// same finding as "the probe could not tell".
    ///
    /// `Egress::Host` is deliberately not partial — that is a decision, and
    /// collapsing the two would report an authorised `git push` as a defect.
    pub fn partial(&self) -> bool {
        self.ns
            .values()
            .any(|s| matches!(s, NsState::NotEntered { .. }))
            || self
                .props
                .values()
                .any(|s| matches!(s, PropState::Unverified { .. }))
            || self
                .seals
                .values()
                .any(|s| matches!(s, Seal::Unverified { .. }))
    }

    /// The namespaces that were asked for and are not there, with why.
    pub fn missing(&self) -> Vec<(Namespace, String)> {
        self.ns
            .iter()
            .filter_map(|(n, s)| match s {
                NsState::NotEntered { why } => Some((*n, why.clone())),
                _ => None,
            })
            .collect()
    }

    /// The disclosure, **read off the map**.
    ///
    /// Nothing in here is a constant string about a property: every line comes
    /// from `self.ns`, so a boundary that lost its PID namespace says so and a
    /// hard-coded `confined` cannot outlive the confinement. That is rule 2, and
    /// it is rule 2 because this repo shipped the opposite twice.
    pub fn describe(&self) -> String {
        let mut s = String::new();
        let head = if self.complete() && !self.partial() {
            "confined"
        } else if self.complete() {
            "confined, PARTIALLY — something that was asked for is missing or could not be confirmed"
        } else if self.platform == Mechanism::Policy {
            "NOT CONFINED — a required property is not held or a seal is open; a command here is refused"
        } else {
            "NOT CONFINED — a required namespace is missing or a seal is open; a command here is refused"
        };
        s.push_str(&format!("{head}; by {}", self.mechanism));
        for k in SealKind::ALL {
            let seal = self.seals.get(&k).cloned().unwrap_or(Seal::Unverified {
                why: "never tested".into(),
            });
            s.push_str(&format!("\n  seal {k}: {} — {}", seal.line(), k.buys()));
        }
        match self.platform {
            // The namespace table, by name — the whole answer on Linux.
            Mechanism::Namespaces => {
                for n in Namespace::ALL {
                    let state = self.ns.get(&n).cloned().unwrap_or(NsState::NotEntered {
                        why: "never measured".into(),
                    });
                    let req = if Namespace::REQUIRED.contains(&n) {
                        " [required]"
                    } else {
                        ""
                    };
                    s.push_str(&format!("\n  {n}{req}: {} — {}", state.line(), n.buys()));
                }
            }
            // The property table, which is what a mechanism without namespaces has to
            // say instead — including the properties it cannot have.
            Mechanism::Policy => {
                for p in Prop::ALL {
                    let state = self
                        .props
                        .get(&p)
                        .cloned()
                        .unwrap_or(PropState::Unverified {
                            why: "never measured".into(),
                        });
                    let req = if Prop::REQUIRED_POLICY.contains(&p) {
                        " [required]"
                    } else {
                        ""
                    };
                    s.push_str(&format!("\n  {p}{req}: {} — {}", state.line(), p.buys()));
                }
            }
        }
        s.push_str(&format!("\n  view: {}", self.view.summary()));
        s.push_str(&format!(
            "\n  egress: {}",
            match &self.egress {
                Egress::Denied => "DENIED — no interfaces, no routes, no DNS".to_string(),
                Egress::Host { why } => format!("the host's network, by decision — {why}"),
            }
        ));
        s
    }

    /// Turn a bare `ENOENT` into *"not in this session's filesystem view"*.
    ///
    /// Rule 4. A model that reads `cat: /home/dead/.ssh/id_rsa: No such file or
    /// directory` concludes the file does not exist, and then reasons from an
    /// answer about an empty haystack. This says what actually happened.
    ///
    /// Bounded, per `docs/tool-design-brief.md` §5 — *cap the corrective body the
    /// way you cap a success body* — because a miss path that can be large is a
    /// miss path that produces more output than a hit.
    /// **The paths this command could not see**, as paths rather than as prose.
    ///
    /// `absence_notes` says the same thing in a sentence for the model; this is the
    /// same finding for a caller that can act on it. The note used to end *"it has
    /// to be granted into the view by whoever opened the session; asking again will
    /// not change it"* — true, and a dead end: nothing could ask that person. Now
    /// the runtime takes these and raises a grant decision, so the operator is
    /// asked instead of the model being left to hand them shell commands.
    pub fn outside_paths(&self, output: &str) -> Vec<PathBuf> {
        let mut outside: Vec<PathBuf> = Vec::new();
        for line in output.lines() {
            if !self.boundary_refusal(line) {
                continue;
            }
            for tok in absolute_paths(line) {
                if matches!(self.view.classify(&tok), Presence::Outside) && !outside.contains(&tok)
                {
                    outside.push(tok);
                }
            }
            if outside.len() >= MAX_ABSENCE_PATHS {
                break;
            }
        }
        outside.truncate(MAX_ABSENCE_PATHS);
        outside
    }

    /// **The words in a command's output that mean *the boundary refused this***, which
    /// are not the same words on both kinds of boundary.
    ///
    /// A path outside a mount view is **absent**, so the command's own answer is
    /// `No such file or directory`. A path outside a policy boundary is **denied**, and
    /// the answer is `Operation not permitted` — so a rule-4 note written only for
    /// `ENOENT` says nothing at all on the Mac, which is where the note is needed most
    /// (the command saw `EPERM` and the model reads *a permission problem in the
    /// checkout*).
    ///
    /// A shell's own missing-binary words are included on both, because `sh` says
    /// `not found` for a program that is not in the view.
    fn boundary_refusal(&self, line: &str) -> bool {
        let absent = line.contains("No such file or directory")
            || line.contains("ENOENT")
            || line.contains("not found")
            || line.contains("cannot find");
        match self.platform {
            Mechanism::Namespaces => absent,
            Mechanism::Policy => {
                absent
                    || line.contains("Operation not permitted")
                    || line.contains("Permission denied")
            }
        }
    }

    /// What the boundary *does* to a path outside the view, in the words the note uses.
    ///
    /// One sentence per mechanism, because the two are different facts: a mount
    /// namespace does not contain the path (no spelling reaches it), a policy denies the
    /// access (the path is there and the kernel refuses). A policy's version also says
    /// how to tell it apart from an ordinary permission problem, which is the thing a
    /// model gets wrong on a Mac.
    fn refusal_clause(&self) -> &'static str {
        match self.platform {
            Mechanism::Namespaces => {
                "a path list is not what is stopping it — the mount namespace does not \
                 contain it"
            }
            Mechanism::Policy => {
                "the policy denies the path rather than hiding it, so this `Operation not \
                 permitted` is the boundary and not a file permission in the checkout"
            }
        }
    }

    pub fn absence_notes(&self, output: &str) -> Vec<String> {
        let mut outside: Vec<PathBuf> = Vec::new();
        let mut replaced: Vec<(PathBuf, &'static str)> = Vec::new();
        for line in output.lines() {
            if !self.boundary_refusal(line) {
                continue;
            }
            for tok in absolute_paths(line) {
                match self.view.classify(&tok) {
                    Presence::Outside if !outside.contains(&tok) => outside.push(tok),
                    Presence::Replaced { what } if !replaced.iter().any(|(p, _)| *p == tok) => {
                        replaced.push((tok, what))
                    }
                    _ => {}
                }
            }
            if outside.len() >= MAX_ABSENCE_PATHS && replaced.len() >= MAX_ABSENCE_PATHS {
                break;
            }
        }
        outside.truncate(MAX_ABSENCE_PATHS);
        replaced.truncate(MAX_ABSENCE_PATHS);

        let mut notes = Vec::new();
        if !outside.is_empty() {
            let list = outside
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect::<Vec<_>>()
                .join(", ");
            notes.push(format!(
                "{list} {} not in this session's filesystem view, so the command saw \
                 {} as absent. That failure is the boundary, NOT an answer about \
                 whether anything is there: this session's view is {}. Nothing \
                 outside it can be reached by another spelling of the path, and {}. \
                 If the work genuinely needs a path outside the view, the operator is \
                 being asked to grant it in — re-running before they answer will not \
                 change it.",
                if outside.len() == 1 { "is" } else { "are" },
                if outside.len() == 1 { "it" } else { "them" },
                self.view.summary().lines().next().unwrap_or("the project"),
                self.refusal_clause(),
            ));
        }
        if !replaced.is_empty() {
            let list = replaced
                .iter()
                .map(|(p, what)| format!("`{}` ({what})", p.display()))
                .collect::<Vec<_>>()
                .join("; ");
            notes.push(format!(
                "{list} — {} inside this session's filesystem view, but {} not the \
                 host's, so anything the host keeps there is absent and anything \
                 written there does not reach it. This `ENOENT` is the boundary too, \
                 in a different way from a path that is simply outside: {}",
                if replaced.len() == 1 {
                    "this is"
                } else {
                    "these are"
                },
                if replaced.len() == 1 {
                    "it is"
                } else {
                    "they are"
                },
                // **The session's `/tmp` is the one replaced root that does not start empty**
                // — it holds what earlier calls of this session left. Saying *it just starts
                // empty* there would be a note about a file the model itself wrote.
                if replaced.iter().any(|(_, what)| *what == SESSION_TMP_WHAT) {
                    "the directory exists and can be used, and what it holds is this \
                     session's own, written by an earlier call and not by the host."
                } else {
                    "the directory exists and can be used, it just starts empty."
                },
            ));
        }
        notes
    }
}

/// At most this many paths in one absence note.
const MAX_ABSENCE_PATHS: usize = 5;

/// Absolute-path-shaped tokens in a line of command output.
///
/// Deliberately crude and deliberately *bounded*: this feeds a note, not a
/// decision, so a false positive costs a sentence and a false negative costs the
/// note. Trailing punctuation a diagnostic adds (`:`, `,`, `'`, `"`, `)`) is
/// stripped, which is the part that would otherwise make every path miss.
fn absolute_paths(line: &str) -> Vec<PathBuf> {
    line.split(|c: char| c.is_whitespace())
        .filter_map(|t| {
            let t = t.trim_matches(|c: char| "\"'`(),;:".contains(c));
            if t.len() > 1 && t.starts_with('/') {
                Some(PathBuf::from(t))
            } else {
                None
            }
        })
        .collect()
}

/// What a spawn needs from the confinement, so that the confinement decides how
/// to express it rather than the caller guessing.
#[derive(Debug, Clone)]
pub struct ConfinePlan<'a> {
    /// Where the command runs. An absolute host path inside the view.
    pub cwd: &'a Path,
    /// The additions to the environment the caller asked for. Under
    /// `--clearenv` these have to be re-declared inside, so they come through
    /// here rather than through `Command::env`.
    pub env: &'a [(String, String)],
}

/// **Whether the wrapped command gets a session of its own.**
///
/// One flag, two callers, and each one's reason is different — which is why it is a
/// type with two variants rather than a bool (a bool called `controlling_terminal`
/// was the first spelling, and it named only the pty's reason):
///
/// | variant | flag | who needs it |
/// |---|---|---|
/// | [`Session::Detached`] | `--new-session` | a job or a command this box spawns, so `TIOCSTI` cannot push characters back into the operator's shell |
/// | [`Session::Kept`] | none | a program whose pty is its own (a `!term` pane), and **a command on another machine**, whose remote process group is what an interrupt kills |
///
/// The second row's remote half is the one that is not obvious: a group kill reaches a
/// process that is in the group, and `--new-session` moves the sandbox (and everything it
/// starts) out of it. `docs/remote-hosts.md`'s rule 6 is *an interrupt leaves nothing
/// running there*, so the remote argv must not take the session away. Nothing is given up:
/// a remote command's stdio is pipes and files, so there is no terminal for `TIOCSTI` to
/// reach.
///
/// What it buys locally is [`SealKind`]'s and the pane's business, not this type's: the
/// probe measures the namespaces and the two seals, and `setsid` is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Detached,
    Kept,
}

/// **One element of a command line, kept typed until it is joined.**
///
/// A local spawn is an `argv` — every element a literal, no shell in between. A spawn on
/// another machine is a *shell script*: the far side's `bash -lc` splits it. The two are the
/// same policy and must not become two builders (a second spelling of the binds is how a
/// policy drifts), so the builder emits pieces and each caller joins them for the medium it
/// has.
///
/// The distinction matters for exactly one thing, and getting it wrong is silent:
/// `--setenv PATH "$PATH"` must reach the *far* shell as an expansion, and a literal
/// `--setenv PATH '$PATH'` puts the five characters `$PATH` in the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// An ordinary argument: quoted for whichever shell joins it.
    Literal(String),
    /// A fragment the **far** shell must expand — the keep-list's `"$PATH"`. Already
    /// spelled for the shell it is going into, so a joiner passes it through.
    Shell(String),
}

impl Piece {
    fn literal(s: impl Into<String>) -> Piece {
        Piece::Literal(s.into())
    }
}

/// **Where the keep-list's values come from.**
///
/// [`KEEP_ENV`] is a keep-list rather than an unset-list, so its *values* have to be read
/// somewhere, and the somewhere is not the same on both sides of an ssh connection: this
/// Mac's `PATH` inside a Linux sandbox is a list of directories that do not exist there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    /// This process's own environment — a command this box spawns.
    Process,
    /// The **far** shell's, expanded there just before the helper runs.
    Remote,
}

/// Join pieces into a POSIX shell command line: literals quoted, shell fragments as
/// written.
pub fn join(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Literal(s) => crate::remote::quote(s),
            Piece::Shell(s) => s.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The seam, and the mirror of [`super::ScopeTree`]: lifetime is one mechanism,
/// view is another, and a backend can have either without the other.
pub trait Confinement: Send + Sync {
    /// One line — or several — for `EXPLAIN` and for the head. **Must say what it
    /// does not confine**, because a disclosure that lists only the guarantees
    /// reads as a claim about the rest.
    fn describe(&self) -> String;

    /// What was measured, if anything was.
    fn boundary(&self) -> Option<&Boundary>;

    /// The argv that goes in front of the shell, or the refusal.
    ///
    /// An empty vector is a legitimate answer and means *nothing wraps this*; only
    /// [`Unconfined`] returns it, and [`Unconfined::describe`] says so in the same
    /// breath.
    fn wrap(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError>;

    /// **[`Confinement::wrap`] as typed pieces**, for a caller whose command is a shell
    /// script rather than an `argv` — a command on another machine.
    ///
    /// The default is [`Confinement::wrap`] with every element a [`Piece::Literal`], which
    /// is what a confinement with no far side means.
    fn wrap_pieces(
        &self,
        plan: &ConfinePlan<'_>,
        _env: EnvSource,
    ) -> Result<Vec<Piece>, ExecError> {
        Ok(self.wrap(plan)?.into_iter().map(Piece::Literal).collect())
    }

    /// Is this path outside the view? `false` when there is no view, because
    /// "outside" is meaningless then and answering `true` would produce a note
    /// claiming a boundary that is not there.
    fn outside_view(&self, _path: &Path) -> bool {
        false
    }

    /// Rule 4's notes for one command's output.
    fn absence_notes(&self, _output: &str) -> Vec<String> {
        Vec::new()
    }

    /// Rule 4's same finding as PATHS, for a caller that can act on it rather than
    /// only report it — the runtime raises a grant decision from these. Empty
    /// where there is no view, because "outside" is meaningless then.
    fn outside_paths(&self, _output: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    /// **Did the LAUNCHER fail, rather than the command?**
    ///
    /// F5: *never let a component's "I did not do this" be reported upward as
    /// success* — and the exec path's version of that is subtler, because the
    /// wrong answer is not `ok` but *"the command ran and exited non-zero, which is
    /// the command's answer"*. A boundary that failed to set up produces an exit
    /// code that looks exactly like a command's, and reporting it as the command's
    /// answer tells the model something false about the world it is reasoning over.
    ///
    /// The reference launcher solves this at the source: it exits **125**, *"a code
    /// the wrapped command itself is unlikely to use, so the executor can tell
    /// launcher failures from command failures"* (`deepseek-harness`
    /// `native/system/packages/entry/src/main.c:105-111`) — the same trick, and the
    /// same number, as [`super::scope::EXIT_NOT_SCOPED`]. We do not control
    /// bubblewrap's exit codes, so this reads its stderr instead, which is weaker
    /// and is stated as such rather than presented as equivalent.
    fn launcher_failure(&self, _output: &str) -> Option<String> {
        None
    }

    /// **[`Confinement::wrap`] for a program that keeps a terminal of its own** — the `!term`
    /// pane (R60 item 3), whose pty is its controlling terminal and must stay so inside the
    /// boundary.
    ///
    /// The one difference is the controlling terminal. [`Bwrap`]'s ordinary argv starts the
    /// command in a new session (`--new-session`), which takes the terminal away so `TIOCSTI`
    /// cannot push characters into the operator's shell — right for a job, whose stdio is pipes
    /// and whose session would otherwise be the daemon's. A pane's pty is **its own**: a pty
    /// the daemon opened for this one program, with nothing on it but the program and the
    /// keystrokes the operator sends it. Without its controlling terminal a screen program
    /// loses `/dev/tty` (`ENXIO`), job control and `SIGWINCH` — the whole pane — and what
    /// `TIOCSTI` could reach is the program's own input, which the program can already write.
    ///
    /// The default is `wrap` itself: a confinement with no session to drop has no difference
    /// to make, and [`Unconfined`] and [`NoConfinement`] answer the same way for both.
    fn wrap_terminal(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap(plan)
    }
}

/// **Confinement was asked for and is not available.** Every spawn refuses.
///
/// The pattern is [`super::NoScopes`], [`crate::builtins::retrieval::Unavailable`]
/// and [`crate::adjudicate::NoAdjudicator`]: the default refuses and says why. What
/// this one must never do is degrade into [`Unconfined`], and it cannot — there is
/// no constructor, method or flag on it that yields a runnable command.
#[derive(Debug, Clone)]
pub struct NoConfinement {
    pub why: String,
}

impl NoConfinement {
    pub fn new(why: impl Into<String>) -> Self {
        NoConfinement { why: why.into() }
    }
}

impl Confinement for NoConfinement {
    fn describe(&self) -> String {
        format!(
            "NONE, and this session refuses to exec because of it — {}",
            self.why
        )
    }
    fn boundary(&self) -> Option<&Boundary> {
        None
    }
    fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Err(ExecError::NoConfinement(self.why.clone()))
    }
}

/// **Confinement was not asked for.** The status quo, and it is loud about it.
///
/// This is not a fallback: nothing constructs it in response to a failure. It is
/// what [`crate::backend::HostBackend::executable`] has always had, named, so that
/// the difference between *not asked for* and *asked for and missing* is a type
/// and not a comment.
#[derive(Debug, Clone)]
pub struct Unconfined {
    pub why: String,
}

impl Unconfined {
    pub fn because(why: impl Into<String>) -> Self {
        Unconfined { why: why.into() }
    }
}

impl Confinement for Unconfined {
    fn describe(&self) -> String {
        format!(
            "NOT CONFINED — no namespaces: a command here reads this user's whole \
             filesystem, sees every process, and reaches the network. {}",
            self.why
        )
    }
    fn boundary(&self) -> Option<&Boundary> {
        None
    }
    fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Ok(Vec::new())
    }
}

/// The bubblewrap boundary, **measured at construction**.
#[derive(Debug, Clone)]
pub struct Bwrap {
    helper: PathBuf,
    boundary: Boundary,
}

/// The environment variable that overrides where `bwrap` is looked for. Set to a
/// path that is not there to exercise the refusal, which is what
/// `tests/confine.rs` does.
pub const BWRAP_ENV: &str = "LETIBOT_BWRAP";

impl Bwrap {
    /// Find the helper, build the real invocation, **run it**, and read back which
    /// namespaces it actually entered.
    ///
    /// Refuses rather than degrading, at four points, and each names what was
    /// missing: no helper; the helper would not run; the readback could not be
    /// parsed; a required namespace was not entered.
    pub fn probe(view: ViewSpec, egress: Egress) -> Result<Bwrap, ExecError> {
        // **No bubblewrap on macOS, and no installing one**: it is a front end to Linux
        // namespaces, which Darwin does not have. Said by name, with what a Mac does have,
        // rather than through the Linux sentence below and its `apt install`.
        #[cfg(target_os = "macos")]
        {
            let _ = (&view, egress);
            return Err(ExecError::NoConfinement(
                "macOS has no namespace boundary: bubblewrap is a front end to Linux \
                 namespaces and cannot be installed here. A Mac's boundary is a policy one \
                 (`super::seatbelt::Seatbelt`, `sandbox-exec -f <profile>`), which \
                 `confine::probe` chooses on this platform; ask for that rather than for \
                 this. Nothing was run, and nothing was run unconfined."
                    .to_string(),
            ));
        }
        #[allow(unreachable_code)]
        let helper = find_helper()?;
        let mechanism = version_of(&helper);
        let view = prepared(view)?;

        // Read the harness's own namespace inodes FIRST. These are the denominator:
        // without them, an inode read from inside proves nothing, because the
        // number that says "confined" and the number that says "shared" are both
        // just numbers.
        let mine = own_namespaces();

        let argv = build_argv(
            &helper,
            &view,
            &egress,
            &ConfinePlan {
                cwd: &view.project,
                env: &[],
            },
        );
        let probe = READBACK;
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg("/bin/sh")
            .arg("-c")
            .arg(probe)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                ExecError::NoConfinement(format!(
                    "`{}` could not be run ({e}), so nothing was measured and no \
                     command will be run confined",
                    helper.display()
                ))
            })?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() && text.is_empty() {
            return Err(ExecError::NoConfinement(format!(
                "`{}` exited {} without entering anything. Its own words: {}",
                helper.display(),
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "on a signal".into()),
                if stderr.trim().is_empty() {
                    "(it said nothing)".to_string()
                } else {
                    stderr.trim().to_string()
                }
            )));
        }

        let inside = parse_readback(&text);
        if inside.is_empty() {
            return Err(ExecError::NoConfinement(format!(
                "`{}` ran but the namespace readback could not be parsed, so what it \
                 entered is unknown — and an unknown boundary is treated as no \
                 boundary. It printed: {}",
                helper.display(),
                snippet(&text)
            )));
        }

        let mut ns = BTreeMap::new();
        for n in Namespace::ALL {
            ns.insert(
                n,
                state_of(
                    n,
                    inside.get(n.proc_name()),
                    mine.get(n.proc_name()),
                    &egress,
                ),
            );
        }

        let boundary = Boundary::namespaces(mechanism, view, egress, ns, parse_seals(&text));
        judged(helper, boundary)
    }

    /// The default confinement for a workspace root: project-only view, tmpfs
    /// `$HOME`, egress denied.
    pub fn project(root: impl Into<PathBuf>) -> Result<Bwrap, ExecError> {
        Bwrap::probe(ViewSpec::project_only(root), Egress::Denied)
    }

    /// **A boundary that was measured on ANOTHER machine**, out of what
    /// [`probe_script`] printed there.
    ///
    /// A remote host's probe cannot be [`Bwrap::probe`]: the helper is on that machine, the
    /// namespaces it enters are that kernel's, and the denominator the readback is compared
    /// against is that box's own inodes rather than this one's. So the script prints both —
    /// the outer `/proc/self/ns/*` of the shell that runs the helper, and the readback from
    /// inside — and this reads them the same way, through [`state_of`] and the same three
    /// refusals.
    ///
    /// **What is checked here and what is not.** The view's paths are checked at spawn and not
    /// here: `Bwrap::probe` stats the project because a local view is built from this box's
    /// paths, and a `--bind` of a path that is not there makes the helper fail with `bwrap: `
    /// on stderr, which [`Confinement::launcher_failure`] turns into *the boundary failed, this
    /// is not the command's answer*. A view whose root is missing is therefore refused as a
    /// launcher failure naming the bind, which is the honest sentence — a client cannot stat
    /// the far machine.
    pub fn measured_on_a_host(helper: PathBuf, boundary: Boundary) -> Result<Bwrap, ExecError> {
        judged(helper, boundary)
    }
}

/// **One namespace's state**, from the inode read inside, the denominator read outside, and
/// what the egress decision says.
///
/// Shared by the local probe and the remote one so that the two cannot disagree about what
/// *entered* means — the comparison against the caller's own inode is the whole point of the
/// readback, and a second spelling of it would be a second answer.
fn state_of(
    n: Namespace,
    inside: Option<&String>,
    mine: Option<&String>,
    egress: &Egress,
) -> NsState {
    let want_shared = n == Namespace::Net && egress.allowed();
    match inside {
        None => NsState::NotEntered {
            why: format!(
                "`readlink /proc/self/ns/{}` printed nothing from inside, so \
                 this namespace could not be confirmed and is treated as absent",
                n.proc_name()
            ),
        },
        Some(inode) if Some(inode) == mine => {
            if want_shared {
                NsState::SharedByDecision {
                    why: match egress {
                        Egress::Host { why } => why.clone(),
                        Egress::Denied => "unreachable".into(),
                    },
                }
            } else {
                NsState::NotEntered {
                    why: format!(
                        "the inode inside ({inode}) is the harness's own, so \
                         `--unshare-{}` did not take effect however the \
                         arguments read",
                        unshare_flag(n)
                    ),
                }
            }
        }
        Some(inode) => NsState::Entered {
            inode: inode.clone(),
        },
    }
}

/// **The three refusals a measurement has to survive**, for both sides of an ssh
/// connection: a seal measured open, a required namespace missing, and the boundary that is
/// left.
fn judged(helper: PathBuf, boundary: Boundary) -> Result<Bwrap, ExecError> {
    // A seal measured **open** is the case the coordinator's survey caught and
    // this brief did not originally ask for: mount, PID and network namespaces
    // entered by a parent are not a boundary if the child can `unshare` its own.
    // It refuses rather than describing itself as partial, because "partial"
    // implies the part that is there still holds, and this is the part that
    // makes the others hold.
    if let Some((k, why)) = boundary.seals.iter().find_map(|(k, s)| match s {
        Seal::Open { why } => Some((*k, why.clone())),
        _ => None,
    }) {
        return Err(ExecError::NoConfinement(format!(
            "the namespaces were entered but the boundary is not sealed: {k} is \
             OPEN — {why}. A process inside could step back out of the view its \
             parent built, which makes every namespace above it decoration. \
             `--disable-userns`/`--assert-userns-disabled` were passed to \
             `{}` and did not take. What was measured: {}",
            helper.display(),
            boundary.describe()
        )));
    }

    // Fail closed on the three that are the boundary. A partial boundary in a
    // non-required namespace is kept and *described* as partial; a missing
    // required one is not a boundary at all, and running a command inside it
    // while calling it confined is the defect this whole module is written
    // against.
    if !boundary.complete() {
        let missing = Namespace::REQUIRED
            .iter()
            .filter(|n| !boundary.ns.get(n).map(|s| s.entered()).unwrap_or(false))
            .map(|n| format!("{n} ({})", n.buys()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ExecError::NoConfinement(format!(
            "the helper ran but did not enter every required namespace: {missing}. \
             Measured, not assumed — the inode read from inside matched the \
             shell's own. On a host with \
             `kernel.apparmor_restrict_unprivileged_userns=1` this is what a \
             direct `unshare` looks like; `/usr/bin/bwrap` needs its \
             `bwrap-userns-restrict` AppArmor profile loaded. What was measured: \
             {}",
            boundary.describe()
        )));
    }
    Ok(Bwrap { helper, boundary })
}

/// **What a probe of another machine found**, before any view is applied to it.
///
/// The parts are kept apart because they answer different questions and are cached
/// together: *can this box confine at all* ([`HostProbe::helper`], [`HostProbe::version`],
/// [`HostProbe::outer`], [`HostProbe::inside`], [`HostProbe::seals`]) and *what is there to
/// put in the view* ([`HostProbe::home`], [`HostProbe::toolchains`]). The second half is why
/// the paths in a remote [`ViewSpec`] can be absolute: `~/.cargo` is a fact about that
/// machine's home, and a `~` in a bind is a literal `~`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostProbe {
    /// Where the helper is **there**, as that shell spelled it. Empty means it is not there.
    pub helper: String,
    /// `bwrap --version`, first line.
    pub version: String,
    /// That machine's `$HOME`, from its own shell.
    pub home: String,
    /// The toolchain paths that exist there, absolute — the candidates [`GRANT_CANDIDATES`]
    /// names, reduced to the ones that are really there.
    pub toolchains: Vec<String>,
    /// `/proc/self/ns/*` of the shell that ran the helper: the denominator.
    pub outer: BTreeMap<String, String>,
    /// The same, read from **inside** the boundary.
    pub inside: BTreeMap<String, String>,
    pub seals: BTreeMap<SealKind, Seal>,
    /// Everything the helper and the readback said, for a refusal that has to quote it.
    pub said: String,
}

/// **The paths a remote view holds read-only, by default** — the toolchain, and the one
/// config file a commit cannot happen without.
///
/// Each entry is a path **under that machine's `$HOME`**, expanded there by the probe, and
/// none of them is the home itself: a view that bound `$HOME` would put the operator's mail,
/// their browser profile and their `.ssh` inside a sandbox whose whole purpose is to have none
/// of that. `~/.ssh` is not on this list and cannot be: it is not a toolchain, and
/// `docs/remote-hosts.md` RH5's refusal is about letibot never reading it.
///
/// * `.cargo` and `.rustup` — the Rust toolchain, and the registry that makes a build
///   possible without the network. **Read-only, and the cost is stated**: a *new* dependency
///   cannot be fetched into it from inside the boundary, so a build that needs one says so
///   and the answer is a wider view, not a writable operator cache.
/// * `.local/bin`, `.local/share/mise`, `.local/state/mise` — the tools the operator's
///   machines install through mise, and the shims `PATH` already names.
/// * `Project/llama.cpp/build*` — the one build tree a session that links against a local
///   llama.cpp needs, and only when it is there. A glob with a fixed prefix, so it cannot
///   reach anything but that.
/// * `.gitconfig` — a commit made inside the boundary is recorded under the identity in this
///   file; without it every `git commit` on a remote session fails with *Author identity
///   unknown*, and the branch that comes back is the whole deliverable.
/// * `Projects/llama.cpp` and `models` — what this tree's own build and its vocabulary tests
///   need on a build machine: the llama.cpp source and headers `letibot-llama`'s build script
///   compiles against, and the GGUF vocabularies the tests load. MEASURED 2026-10-27: without
///   them a package placed on lab2x1 could not build `letibot-harnessd` with default features,
///   and the vocabulary tests returned early — the vacuous green `LETIBOT_REQUIRE_APPARATUS`
///   exists to refuse. Read-only, and bound only where they exist.
pub const GRANT_CANDIDATES: &[&str] = &[
    "$HOME/.cargo",
    "$HOME/.rustup",
    "$HOME/.local/bin",
    "$HOME/.local/share/mise",
    "$HOME/.local/state/mise",
    "$HOME/bin",
    "$HOME/.gitconfig",
    "$HOME/Projects/llama.cpp",
    "$HOME/models",
];

/// The globbed candidate, kept separate because it is one: a fixed prefix and a `*`, never a
/// walk.
pub const GRANT_GLOBS: &[&str] = &[
    "$HOME/Projects/llama.cpp/build",
    "$HOME/Projects/llama.cpp/build-*",
];

/// **The one script a remote probe runs** — the same readback the local probe runs, plus the
/// two facts a view needs (where that home is, which toolchain directories exist) and the
/// denominator read *outside* the boundary.
///
/// The denominator is the whole reason this is a script rather than one command: an inode read
/// from inside proves nothing without the number to compare it against, and on a remote that
/// number is **that shell's**, not this daemon's.
///
/// `key=value` and `outer …` lines, as [`crate::remote::probe`] does, so the parser here has no
/// opinions about the far side's locale. Nothing here writes: the placement has already made
/// the checkout and the home directory, and a probe is still a question.
pub fn probe_script(view: &ViewSpec, egress: &Egress) -> String {
    let mut s = String::new();
    s.push_str("b=$(command -v bwrap 2>/dev/null || true)\n");
    s.push_str("echo \"helper=$b\"\n");
    s.push_str("echo \"home=$HOME\"\n");
    s.push_str("for d in");
    for c in GRANT_CANDIDATES {
        s.push_str(&format!(" \"{c}\""));
    }
    s.push_str("; do [ -e \"$d\" ] && echo \"have=$d\"; done\n");
    s.push_str("for d in");
    for c in GRANT_GLOBS {
        s.push_str(&format!(" {c}"));
    }
    s.push_str("; do [ -d \"$d\" ] && echo \"have=$d\"; done\n");
    s.push_str("if [ -n \"$b\" ]; then echo \"version=$(\"$b\" --version 2>&1 | head -1)\"; fi\n");
    s.push_str(
        "for n in user mnt pid net ipc uts cgroup; do \
         printf 'outer %s %s\\n' \"$n\" \"$(readlink /proc/self/ns/$n)\"; done\n",
    );
    // The readback, inside the same argv a command will be wrapped in — the probe and the
    // spawn share [`pieces_of`], so what was measured is what runs.
    s.push_str("if [ -n \"$b\" ]; then\n");
    let pieces = pieces_of(
        Piece::Shell("\"$b\"".to_string()),
        view,
        egress,
        &ConfinePlan {
            cwd: &view.project,
            env: &[],
        },
        Session::Detached,
        EnvSource::Remote,
    );
    s.push_str(&join(&pieces));
    s.push_str(&format!(" /bin/sh -c {}\n", crate::remote::quote(READBACK)));
    s.push_str("else echo 'bwrap: no helper on this host' >&2; fi\n");
    s
}

/// **Read [`probe_script`]'s output** — or the sentence that says why it is not an answer.
///
/// A helper that is not there is a **fact** and is returned as [`HostProbe::helper`] empty;
/// a helper that ran and did not enter the namespaces is a refusal from
/// [`Bwrap::measured_on_a_host`], not from here, because that judgement is [`judged`]'s and
/// must not exist twice.
pub fn read_probe(said: &str) -> HostProbe {
    let mut p = HostProbe {
        said: said.to_string(),
        seals: parse_seals(said),
        ..HostProbe::default()
    };
    let mut inside_text = String::new();
    for line in said.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("ns ") {
            // The readback's own lines, collected for `parse_readback` — the one parser
            // for them, so the remote readback is read by the same code as the local one.
            inside_text.push_str(line);
            inside_text.push('\n');
            continue;
        }
        if let Some(rest) = line.strip_prefix("outer ") {
            let mut it = rest.split_whitespace();
            if let (Some(n), Some(inode)) = (it.next(), it.next()) {
                p.outer.insert(n.to_string(), inode.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "helper" => p.helper = value.to_string(),
            "version" => p.version = value.to_string(),
            "home" => p.home = value.to_string(),
            "have" if !value.is_empty() => p.toolchains.push(value.to_string()),
            _ => {}
        }
    }
    p.inside = parse_readback(&inside_text);
    p
}

/// The boundary a [`HostProbe`] measured, applied to the view this session needs.
///
/// The refusals are [`judged`]'s, so a remote host that cannot enter the namespaces is
/// refused with the same sentence a local one is — and the fallback to *unconfined, and the
/// card says why* is the caller's ([`crate::remote`]), not this function's.
pub fn measured_from_probe(
    probe: &HostProbe,
    view: ViewSpec,
    egress: Egress,
) -> Result<Bwrap, ExecError> {
    if probe.helper.trim().is_empty() {
        return Err(ExecError::NoConfinement(
            "`bwrap` is not on `PATH` there. Layer 1's namespaces are entered through \
             bubblewrap rather than a direct `unshare` because this crate has no `libc`, and \
             because a host with `kernel.apparmor_restrict_unprivileged_userns=1` denies \
             `CAP_SYS_ADMIN` in a freshly created user namespace — `/usr/bin/bwrap` has the \
             `bwrap-userns-restrict` profile that permits it. Install it there (`apt install \
             bubblewrap`) and ask again. Nothing was run confined, and nothing was run in a \
             way that claimed to be."
                .to_string(),
        ));
    }
    let mut ns = BTreeMap::new();
    for n in Namespace::ALL {
        ns.insert(
            n,
            state_of(
                n,
                probe.inside.get(n.proc_name()),
                probe.outer.get(n.proc_name()),
                &egress,
            ),
        );
    }
    // **The egress as asked for, and the network namespace as measured.** The two are
    // kept apart on purpose: if a run asked for the host's network and came back with a
    // network namespace of its own, the [`NsState`] says `entered` and a reader sees that
    // the decision did not take — where rewriting the egress here would hide it.
    let boundary = Boundary::namespaces(
        if probe.version.is_empty() {
            "bubblewrap (version unknown)".to_string()
        } else {
            probe.version.clone()
        },
        view,
        egress,
        ns,
        probe.seals.clone(),
    );
    Bwrap::measured_on_a_host(PathBuf::from(probe.helper.trim()), boundary)
}

/// **Prepare a view for any mechanism**: the project canonicalised, and the directories a
/// view may only have asked for abstractly made real.
///
/// Shared by both renderings, which is the point: a `Tmpfs` `$HOME` is a directory on macOS
/// and a tmpfs in a mount namespace, but *whether the directory exists* and *whether the
/// project root is a directory at all* are the same questions on both, and a second copy of
/// them would be a second answer.
pub(crate) fn prepared(mut view: ViewSpec) -> Result<ViewSpec, ExecError> {
    // Canonicalise the project root before anything compares against it.
    // [`ViewSpec::outside`] is lexical, and a lexical comparison against a path
    // holding a symlink is wrong in the direction that matters: `/tmp/x` when
    // `/tmp` is a link to `/private/tmp` would put the whole project outside its
    // own view and refuse every command with a boundary message.
    if let Ok(c) = view.project.canonicalize() {
        view.project = c;
    }

    if !view.project.is_absolute() {
        return Err(ExecError::NoConfinement(format!(
            "the project root `{}` is not absolute; a view is built from absolute host \
             paths and a relative root would bind whatever the daemon's cwd happens to be",
            view.project.display()
        )));
    }
    if !view.project.is_dir() {
        return Err(ExecError::NoConfinement(format!(
            "the project root `{}` is not a directory, so there is nothing to root the \
             view at",
            view.project.display()
        )));
    }
    if let HomeView::Dir(d) = &view.home
        && let Err(e) = std::fs::create_dir_all(d)
    {
        return Err(ExecError::NoConfinement(format!(
            "`$HOME` was declared as `{}` and it could not be created ({e}). A confined \
             command needs a writable `$HOME` that is NOT the operator's home.",
            d.display()
        )));
    }
    // **The session's `/tmp` is made here, once, because a `--bind` of a path that is not
    // there is a helper failure at every spawn and not a setup error at the one place that
    // could explain it.** It is not created empty-and-then-forgotten: this is the directory
    // a session's scratch files live in, and [`TmpView::Session`] is why it exists.
    if let TmpView::Session(d) = &view.tmp
        && let Err(e) = std::fs::create_dir_all(d)
    {
        return Err(ExecError::NoConfinement(format!(
            "this session's `/tmp` was declared as `{}` and it could not be created ({e}). \
             Without it every command would fail at the helper's own bind, which is a \
             boundary failure and not the command's answer.",
            d.display()
        )));
    }
    Ok(view)
}

/// **The boundary this platform can measure for `view`, or the refusal that says what
/// is missing.**
///
/// The one place a caller asks for *a boundary* rather than for a particular mechanism.
/// Linux answers with [`Bwrap`]; macOS answers with [`super::seatbelt::Seatbelt`], which
/// is a mandatory-access policy rather than a set of namespaces. What the two share is
/// the whole contract: the view is the same [`ViewSpec`], the egress decision is the same
/// [`Egress`], and the result is a [`Boundary`] that was measured from inside rather than
/// asserted.
///
/// **A boxed trait object, deliberately.** The callers that widen a view
/// (`HostBackend::with_session_tmp`, `HostBackend::grant_into_view_impl`) hold whatever
/// the first probe produced and replace it with whatever the next one produces; naming
/// [`Bwrap`] in those places is what would make a Mac session silently lose its boundary
/// on the second probe.
pub fn probe(view: ViewSpec, egress: Egress) -> Result<Box<dyn Confinement>, ExecError> {
    #[cfg(target_os = "macos")]
    {
        super::seatbelt::Seatbelt::probe(view, egress).map(|s| Box::new(s) as Box<dyn Confinement>)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Bwrap::probe(view, egress).map(|b| Box::new(b) as Box<dyn Confinement>)
    }
}

/// Can this platform build a boundary at all, without asking what the view is?
///
/// `false` on a platform that has no mechanism to offer, so that a caller which has to
/// behave differently when there is none (the daemon, on a Mac, seats an unconfined
/// session and says so) can ask before it tries. The answer is a compile-time fact, not a
/// probe: whether `sandbox-exec` is *there* is what [`probe`] answers.
pub fn available() -> bool {
    cfg!(target_os = "macos") || cfg!(target_os = "linux")
}

impl Confinement for Bwrap {
    fn describe(&self) -> String {
        self.boundary.describe()
    }

    fn boundary(&self) -> Option<&Boundary> {
        Some(&self.boundary)
    }

    fn wrap(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap_with(plan, Session::Detached)
    }

    fn wrap_terminal(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap_with(plan, Session::Kept)
    }

    fn wrap_pieces(&self, plan: &ConfinePlan<'_>, env: EnvSource) -> Result<Vec<Piece>, ExecError> {
        match env {
            // The far side's own shell supplies the keep-list; the session is the
            // remote one, so a group kill still reaches the sandbox — see [`Session`].
            EnvSource::Remote => self.remote_pieces(plan, Session::Kept),
            EnvSource::Process => Ok(self
                .wrap_with(plan, Session::Detached)?
                .into_iter()
                .map(Piece::Literal)
                .collect()),
        }
    }

    fn outside_view(&self, path: &Path) -> bool {
        self.boundary.view.outside(path)
    }

    fn absence_notes(&self, output: &str) -> Vec<String> {
        self.boundary.absence_notes(output)
    }

    fn outside_paths(&self, output: &str) -> Vec<PathBuf> {
        self.boundary.outside_paths(output)
    }

    /// bubblewrap's own diagnostics are prefixed `bwrap: `. That is the only
    /// signal it gives, so this is a heuristic and the note says so: a command
    /// that itself printed a line starting `bwrap: ` would be misattributed. The
    /// alternative — treating a boundary failure as the command's answer — is
    /// worse, because it is a false statement about what ran.
    fn launcher_failure(&self, output: &str) -> Option<String> {
        let line = output
            .lines()
            .find(|l| l.trim_start().starts_with("bwrap: "))?
            .trim()
            .to_string();
        Some(format!(
            "the confinement helper failed to set up the boundary, so this is NOT the \
             command's answer — the command may never have run. Its own words: `{line}`. \
             The boundary asked for was: {}",
            self.boundary
                .view
                .summary()
                .lines()
                .next()
                .unwrap_or("the project")
        ))
    }
}

impl Bwrap {
    /// [`Confinement::wrap`] and [`Confinement::wrap_terminal`] (and a command on
    /// another machine), which differ by one flag: [`Session`].
    ///
    /// **The probe measured the argv WITH `--new-session`**, and every other argv is that argv
    /// less the one flag. What the probe reads — which namespaces were entered, and the two
    /// seals — does not depend on the session: `setsid` is not a namespace and neither seal is
    /// granted or withheld by it. So the boundary [`Boundary::describe`] reports is the one every
    /// spawn runs in; the difference is stated here rather than measured twice.
    fn wrap_with(
        &self,
        plan: &ConfinePlan<'_>,
        session: Session,
    ) -> Result<Vec<String>, ExecError> {
        self.check(plan)?;
        Ok(build_argv_with(
            &self.helper,
            &self.boundary.view,
            &self.boundary.egress,
            plan,
            session,
        ))
    }

    /// **[`Confinement::wrap_pieces`], the far side's rendering**: the same view, the same
    /// egress and the same session flag, with the keep-list taken from the shell that runs
    /// the helper there rather than from this process.
    ///
    /// This is the one function a command on another machine is wrapped by. It is `Bwrap`'s
    /// and not the backend's so that there is exactly one spelling of the binds.
    pub fn remote_pieces(
        &self,
        plan: &ConfinePlan<'_>,
        session: Session,
    ) -> Result<Vec<Piece>, ExecError> {
        self.check(plan)?;
        Ok(pieces_of(
            Piece::literal(self.helper.display().to_string()),
            &self.boundary.view,
            &self.boundary.egress,
            plan,
            session,
            EnvSource::Remote,
        ))
    }

    /// The two refusals every wrap makes, for both renderings.
    fn check(&self, plan: &ConfinePlan<'_>) -> Result<(), ExecError> {
        // Belt and braces against the one mistake that matters: a `Bwrap` whose
        // boundary lost a required namespace between the probe and now cannot
        // produce an argv. There is no code path that mutates it, which is exactly
        // why the check is cheap enough to keep.
        if !self.boundary.complete() {
            return Err(ExecError::NoConfinement(format!(
                "this boundary is missing a required namespace, so nothing runs \
                 inside it: {}",
                self.boundary.describe()
            )));
        }
        if self.boundary.view.outside(plan.cwd) {
            return Err(ExecError::NotInView {
                path: plan.cwd.display().to_string(),
                view: self.boundary.view.summary(),
            });
        }
        Ok(())
    }

    /// **The helper this boundary was measured with**, as the far side spells it.
    pub fn helper(&self) -> &Path {
        &self.helper
    }
}

/// The `--unshare-` flag for a namespace, for the refusal to name.
fn unshare_flag(n: Namespace) -> &'static str {
    match n {
        Namespace::User => "user",
        Namespace::Mount => "mount (implied: bwrap always makes a mount namespace)",
        Namespace::Pid => "pid",
        Namespace::Net => "net",
        Namespace::Ipc => "ipc",
        Namespace::Uts => "uts",
        Namespace::Cgroup => "cgroup",
    }
}

/// The shell fragment the probe runs **inside** the boundary.
///
/// # This is a functional probe, and that is taken from the survey
///
/// `deepseek-harness`'s launcher has a `--probe` mode that *"build[s] and enforce[s]
/// a maximal ruleset in THIS short-lived process"* and reports what it got —
/// `native/system/packages/entry/src/main.c:262-278`, whose comment states the
/// reason exactly: *"`--version` style checks would miss a kernel that has the
/// syscalls but refuses enforcement; actually restricting is the only honest
/// signal."* That is *guard the fact, not the proxy* applied to picking a sandbox,
/// and it is the right idea. This is the same move in a different mechanism.
///
/// So nothing here is a version number or a feature flag:
///
/// - the namespaces are read from `/proc/self/ns/*` **inside** the boundary,
///   which with `--proc /proc` is the sandbox's own procfs, and compared against
///   the harness's own inodes;
/// - [`SealKind::NestedNamespaces`] is tested by **trying it**: the fragment runs
///   `unshare -U true`, and a success is the escape hatch measured open. A read of
///   `/proc/sys/user/max_user_namespaces` was the obvious proxy and it **lies** —
///   it reported `2147483647` from inside a sandbox where the escape was in fact
///   sealed, because the value is namespaced and the sandbox's procfs shows a
///   different one. The proxy and the fact disagreed on the first run.
/// - [`SealKind::NoNewPrivs`] is read from `/proc/self/status`, which is the
///   kernel's own answer rather than the helper's.
const READBACK: &str = "\
for n in user mnt pid net ipc uts cgroup; do printf 'ns %s %s\\n' \"$n\" \"$(readlink /proc/self/ns/$n)\"; done; \
if command -v unshare >/dev/null 2>&1; then \
  if unshare -U true >/dev/null 2>&1; then printf 'seal nested open\\n'; \
  else printf 'seal nested held\\n'; fi; \
else printf 'seal nested unverified\\n'; fi; \
printf 'seal nnp %s\\n' \"$(sed -n 's/^NoNewPrivs:[[:space:]]*//p' /proc/self/status 2>/dev/null)\"";

/// The invocation, built once and used both by the probe and by every spawn, so
/// that what was measured is what runs. A probe that measured a different argv
/// from the one used would be the banner defect with extra steps.
fn build_argv(
    helper: &Path,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
) -> Vec<String> {
    pieces_of(
        Piece::literal(helper.display().to_string()),
        view,
        egress,
        plan,
        Session::Detached,
        EnvSource::Process,
    )
    .into_iter()
    .map(|p| match p {
        Piece::Literal(s) => s,
        // Unreachable by construction: [`EnvSource::Process`] never emits a shell
        // fragment, and a fragment in an argv would be a literal `$PATH` in the
        // environment rather than a bug worth panicking over.
        Piece::Shell(s) => s,
    })
    .collect()
}

/// [`build_argv`], and for a pane the same argv without `--new-session` — see
/// [`Confinement::wrap_terminal`].
fn build_argv_with(
    helper: &Path,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
    session: Session,
) -> Vec<String> {
    pieces_of(
        Piece::literal(helper.display().to_string()),
        view,
        egress,
        plan,
        session,
        EnvSource::Process,
    )
    .into_iter()
    .map(|p| match p {
        Piece::Literal(s) => s,
        Piece::Shell(s) => s,
    })
    .collect()
}

/// **The one builder**, in pieces: the local argv, the pane's argv and a command on
/// another machine all come out of this function.
///
/// `helper` is a [`Piece`] rather than a path because a remote probe has to spell it
/// `"$(command -v bwrap)"` — the helper's location is a fact of the far machine and is not
/// known here until that script has run.
fn pieces_of(
    helper: Piece,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
    session: Session,
    env: EnvSource,
) -> Vec<Piece> {
    let mut a: Vec<Piece> = vec![helper];
    fn arg(a: &mut Vec<Piece>, s: impl Into<String>) {
        a.push(Piece::literal(s));
    }

    arg(&mut a, "--unshare-user");
    arg(&mut a, "--unshare-pid");
    arg(&mut a, "--unshare-ipc");
    arg(&mut a, "--unshare-uts");
    arg(&mut a, "--unshare-cgroup");
    if !egress.allowed() {
        arg(&mut a, "--unshare-net");
    }
    // **Seal the boundary against a child stepping out of it.** See
    // [`SealKind::NestedNamespaces`]: without this, `unshare(CLONE_NEWUSER)` is
    // available to an unprivileged process inside, and full capabilities in the
    // namespace it just made are enough to `unshare(CLONE_NEWNS)` past the mount
    // view the parent built. `--assert-userns-disabled` makes the helper refuse
    // rather than run if the seal did not take, which is the fail-closed half — and
    // the probe measures it anyway, because a flag is a request.
    arg(&mut a, "--disable-userns");
    arg(&mut a, "--assert-userns-disabled");
    // If the harness dies, this dies. A third mechanism on top of the cgroup and
    // the join script, and it is free.
    arg(&mut a, "--die-with-parent");
    // No controlling terminal, so `TIOCSTI` cannot push characters back into the
    // operator's shell. Safe here because a job's stdio is pipes — kept off for a
    // pane, whose pty is its own, and for a remote command, whose process group an
    // interrupt has to be able to kill: see [`Session`].
    if session == Session::Detached {
        arg(&mut a, "--new-session");
    }

    // The system roots, read-only and `-try`: a host without `/lib64` is not a
    // failure, and a merged-`/usr` host has symlinks where a split one has
    // directories.
    for p in &view.system {
        arg(&mut a, "--ro-bind-try");
        arg(&mut a, p.display().to_string());
        arg(&mut a, p.display().to_string());
    }
    // The kernel interfaces a process needs to be a process. `--proc` is the one
    // that makes the PID namespace visible as isolation rather than as a lie:
    // without it the sandbox would read the host's `/proc` and see every pid.
    arg(&mut a, "--proc");
    arg(&mut a, "/proc");
    arg(&mut a, "--dev");
    arg(&mut a, "/dev");
    // **`/tmp` is the session's, not the command's**, when the caller has a directory for it.
    // A fresh tmpfs here is the safe default and it silently eats a two-call scratch idiom —
    // see [`TmpView`]. `/var/tmp` stays a fresh tmpfs either way: nothing in an agent's idiom
    // writes there, and the one thing `/var/tmp` means on a host is *survives a reboot*, which
    // no sandbox directory of ours may claim. `/run` stays fresh for the reason it exists in
    // this list at all — it holds the agent socket and the tmpfs home, both bound after this.
    match &view.tmp {
        TmpView::PerCommand => {
            arg(&mut a, "--tmpfs");
            arg(&mut a, "/tmp");
        }
        TmpView::Session(dir) => {
            arg(&mut a, "--bind");
            arg(&mut a, dir.display().to_string());
            arg(&mut a, "/tmp");
        }
    }
    arg(&mut a, "--tmpfs");
    arg(&mut a, "/var/tmp");
    arg(&mut a, "--tmpfs");
    arg(&mut a, "/run");

    // The project, read-write, **at its own path**.
    arg(&mut a, "--bind");
    arg(&mut a, view.project.display().to_string());
    arg(&mut a, view.project.display().to_string());

    for g in &view.grants {
        match g {
            Grant::ReadOnly { path, .. } => {
                arg(&mut a, "--ro-bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, path.display().to_string());
            }
            Grant::ReadWrite { path, .. } => {
                arg(&mut a, "--bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, path.display().to_string());
            }
            Grant::AgentSocket { path, .. } => {
                // Bound at a fixed path so `SSH_AUTH_SOCK` inside does not carry
                // the operator's `/run/user/<uid>/…` out into the transcript.
                arg(&mut a, "--bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, AGENT_SOCKET);
            }
        }
    }

    let home = match &view.home {
        HomeView::Tmpfs => TMPFS_HOME.to_string(),
        HomeView::Dir(d) => {
            arg(&mut a, "--bind");
            arg(&mut a, d.display().to_string());
            arg(&mut a, d.display().to_string());
            d.display().to_string()
        }
    };
    if matches!(view.home, HomeView::Tmpfs) {
        // `/run` is already a tmpfs above; this is the directory inside it.
        arg(&mut a, "--dir");
        arg(&mut a, TMPFS_HOME);
    }

    // **Clear, then put back a keep-list.** See `KEEP_ENV`: absence beats denial
    // applied to the environment.
    arg(&mut a, "--clearenv");
    for k in KEEP_ENV {
        match env {
            EnvSource::Process => {
                if let Ok(v) = std::env::var(k) {
                    arg(&mut a, "--setenv");
                    arg(&mut a, *k);
                    arg(&mut a, v);
                }
            }
            // Expanded by the shell that runs the helper — the far one — because the
            // value has to be that machine's.
            EnvSource::Remote => {
                arg(&mut a, "--setenv");
                arg(&mut a, *k);
                a.push(Piece::Shell(format!("\"${k}\"")));
            }
        }
    }
    arg(&mut a, "--setenv");
    arg(&mut a, "HOME");
    arg(&mut a, home);
    if view
        .grants
        .iter()
        .any(|g| matches!(g, Grant::AgentSocket { .. }))
    {
        arg(&mut a, "--setenv");
        arg(&mut a, "SSH_AUTH_SOCK");
        arg(&mut a, AGENT_SOCKET);
    }
    // The caller's own environment last, so an explicit request wins over the
    // keep-list.
    for (k, v) in plan.env {
        arg(&mut a, "--setenv");
        arg(&mut a, k);
        arg(&mut a, v);
    }

    arg(&mut a, "--chdir");
    arg(&mut a, plan.cwd.display().to_string());
    // Everything after this is the command, never an option, whatever it is
    // spelled like. A command beginning `--` would otherwise be read by the helper.
    arg(&mut a, "--");
    a
}

/// Where the helper is. `LETIBOT_BWRAP` first, so a test can point it at nothing.
fn find_helper() -> Result<PathBuf, ExecError> {
    if let Ok(p) = std::env::var(BWRAP_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(ExecError::NoConfinement(format!(
            "`{BWRAP_ENV}` names `{}`, which is not a file, so there is no \
             confinement helper and nothing will be run",
            p.display()
        )));
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        if dir.is_empty() {
            continue;
        }
        let c = Path::new(dir).join("bwrap");
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(ExecError::NoConfinement(
        "`bwrap` is not on `PATH`. Layer 1's namespaces are entered through \
         bubblewrap rather than a direct `unshare` because this crate has no `libc` \
         and because a host with \
         `kernel.apparmor_restrict_unprivileged_userns=1` denies `CAP_SYS_ADMIN` in \
         a freshly created user namespace, which makes the `uid_map` write fail and \
         the namespace inert — `/usr/bin/bwrap` has the `bwrap-userns-restrict` \
         profile that permits it. Install it (`apt install bubblewrap`) or point \
         `LETIBOT_BWRAP` at it. Nothing was run, and nothing was run unconfined."
            .to_string(),
    ))
}

/// The helper's version, for the record. A mechanism named `namespaces` is not a
/// mechanism anybody can reproduce.
fn version_of(helper: &Path) -> String {
    std::process::Command::new(helper)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} (version unknown)", helper.display()))
}

/// The harness's own namespace inodes: the denominator for every readback.
fn own_namespaces() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for n in Namespace::ALL {
        if let Ok(l) = std::fs::read_link(format!("/proc/self/ns/{}", n.proc_name())) {
            m.insert(n.proc_name().to_string(), l.display().to_string());
        }
    }
    m
}

/// `ns <name> <inode>` lines from [`READBACK`].
fn parse_readback(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some("ns") {
            continue;
        }
        let (Some(name), Some(inode)) = (it.next(), it.next()) else {
            continue;
        };
        m.insert(name.to_string(), inode.to_string());
    }
    m
}

/// `seal <name> <verdict>` lines from [`READBACK`].
///
/// A seal that produced no line at all is [`Seal::Unverified`] and never
/// [`Seal::Held`]: an absent measurement is not a passing one, which is the same
/// rule `grep`'s zero denominator follows one layer up.
fn parse_seals(text: &str) -> BTreeMap<SealKind, Seal> {
    let mut got: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some("seal") {
            continue;
        }
        if let (Some(name), Some(v)) = (it.next(), it.next()) {
            got.insert(name.to_string(), v.to_string());
        } else if let Some(name) = it.next() {
            // `seal nnp` with nothing after it: the file was unreadable.
            got.insert(name.to_string(), String::new());
        }
    }
    let mut out = BTreeMap::new();
    out.insert(
        SealKind::NestedNamespaces,
        match got.get("nested").map(String::as_str) {
            Some("held") => Seal::Held {
                how: "`unshare -U true` inside the boundary failed, so a child cannot \
                      obtain the capabilities it would need to leave"
                    .into(),
            },
            Some("open") => Seal::Open {
                why: "`unshare -U true` inside the boundary SUCCEEDED, so a child can \
                      make itself a user namespace, gain full capabilities in it, and \
                      `unshare` a mount namespace out of this view"
                    .into(),
            },
            Some("unverified") => Seal::Unverified {
                why: "`unshare` is not in this view, so the escape could not be tried. \
                      `--assert-userns-disabled` is an independent mechanism and would \
                      have made the helper refuse, but that is the helper's word and \
                      not a measurement"
                    .into(),
            },
            _ => Seal::Unverified {
                why: "the readback printed no verdict for this seal".into(),
            },
        },
    );
    out.insert(
        SealKind::NoNewPrivs,
        match got.get("nnp").map(String::as_str) {
            Some("1") => Seal::Held {
                how: "`/proc/self/status` reports `NoNewPrivs: 1` inside the boundary".into(),
            },
            Some("0") => Seal::Open {
                why: "`/proc/self/status` reports `NoNewPrivs: 0` inside the boundary, \
                      so a setuid binary reachable in the view is an escalation"
                    .into(),
            },
            _ => Seal::Unverified {
                why: "`/proc/self/status` could not be read from inside".into(),
            },
        },
    );
    out
}

fn snippet(s: &str) -> String {
    let t: String = s.chars().take(200).collect();
    if t.trim().is_empty() {
        "(nothing)".to_string()
    } else {
        format!("`{}`", t.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary_with(states: Vec<(Namespace, NsState)>) -> Boundary {
        let mut ns = BTreeMap::new();
        for n in Namespace::ALL {
            ns.insert(
                n,
                NsState::Entered {
                    inode: format!("{n}:[1000{}]", n as u8),
                },
            );
        }
        for (n, s) in states {
            ns.insert(n, s);
        }
        let mut seals = BTreeMap::new();
        for k in SealKind::ALL {
            seals.insert(
                k,
                Seal::Held {
                    how: format!("{k} was tested from inside the fixture"),
                },
            );
        }
        Boundary {
            mechanism: "bubblewrap 0.0.0 (fixture)".into(),
            platform: Mechanism::Namespaces,
            view: ViewSpec::project_only("/proj"),
            egress: Egress::Denied,
            ns,
            seals,
            props: BTreeMap::new(),
            measured_at: SystemTime::now(),
        }
    }

    #[test]
    fn a_partial_boundary_describes_itself_as_partial() {
        // **Rule 2, as the test the brief asks for by name.** `HostBackend::describe`
        // shipped saying `read-only` for the writable backend; the fix is not a
        // better constant, it is that the sentence is computed from the state. So
        // the assertion is that changing one entry in the map changes the
        // disclosure — a hard-coded `confined` cannot pass this.
        let full = boundary_with(vec![]);
        assert!(full.complete());
        assert!(!full.partial());
        assert!(
            full.describe().starts_with("confined;"),
            "{}",
            full.describe()
        );

        let partial = boundary_with(vec![(
            Namespace::Ipc,
            NsState::NotEntered {
                why: "the inode inside is the harness's own".into(),
            },
        )]);
        // Required namespaces are all there, so it still confines — and it must
        // NOT describe itself as if nothing were missing.
        assert!(partial.complete());
        assert!(partial.partial());
        let d = partial.describe();
        assert!(d.contains("PARTIALLY"), "{d}");
        assert!(d.contains("NOT ENTERED"), "{d}");
        assert!(d.contains("ipc"), "{d}");
        assert_eq!(partial.missing().len(), 1);
        assert_eq!(partial.missing()[0].0, Namespace::Ipc);
    }

    #[test]
    fn a_boundary_missing_a_required_namespace_says_a_command_is_refused() {
        let broken = boundary_with(vec![(
            Namespace::Pid,
            NsState::NotEntered {
                why: "`--unshare-pid` did not take effect".into(),
            },
        )]);
        assert!(!broken.complete());
        let d = broken.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("refused"), "{d}");
        // And the one that is missing is named with what it would have bought, so
        // the reader knows what they do not have.
        assert!(d.contains("cannot see or signal the daemon"), "{d}");
    }

    #[test]
    fn a_shared_namespace_is_a_decision_and_not_a_failure() {
        // The two look identical from inside — same inode as the harness — and are
        // completely different facts. Collapsing them would report a declared
        // egress as a broken boundary, and a broken boundary as a decision.
        let b = Boundary {
            egress: Egress::Host {
                why: "the operator authorised `git push` to origin".into(),
            },
            ..boundary_with(vec![(
                Namespace::Net,
                NsState::SharedByDecision {
                    why: "the operator authorised `git push` to origin".into(),
                },
            )])
        };
        assert!(b.complete(), "net is not required");
        assert!(!b.partial(), "a decision is not a missing namespace");
        let d = b.describe();
        assert!(d.contains("SHARED WITH THE HOST by decision"), "{d}");
        assert!(d.contains("git push"), "{d}");
        assert!(!d.contains("NOT ENTERED"), "{d}");
    }

    #[test]
    fn the_default_disclosure_states_the_tmpfs_home_cost() {
        // A surprise slowdown nobody can explain is a worse disclosure than a
        // sentence in `describe()`.
        let d = boundary_with(vec![]).describe();
        assert!(d.contains("FRESH TMPFS"), "{d}");
        assert!(d.contains("build cache"), "{d}");
        assert!(d.contains("DENIED"), "{d}");
    }

    #[test]
    fn no_confinement_refuses_and_names_what_is_missing() {
        let c = NoConfinement::new("`bwrap` is not on `PATH`");
        let e = c
            .wrap(&ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            })
            .unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("bwrap"), "{m}");
        assert!(m.contains("was NOT run"), "{m}");
        assert!(c.boundary().is_none());
        assert!(c.describe().contains("refuses to exec"));
    }

    #[test]
    fn unconfined_says_so_rather_than_saying_nothing() {
        let c = Unconfined::because("HostBackend::executable asks for no boundary");
        assert!(
            c.wrap(&ConfinePlan {
                cwd: Path::new("/"),
                env: &[]
            })
            .unwrap()
            .is_empty()
        );
        let d = c.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("whole filesystem"), "{d}");
        // And it claims no view, so nothing produces an absence note that would
        // blame a boundary that is not there.
        assert!(!c.outside_view(Path::new("/home/dead/.ssh/id_rsa")));
        assert!(
            c.absence_notes("cat: /etc/shadow: No such file or directory")
                .is_empty()
        );
    }

    #[test]
    fn the_view_is_lexical_and_the_parent_chain_is_inside_it() {
        let v = ViewSpec::project_only("/home/dead/Projects/letibot");
        assert!(!v.outside(Path::new("/home/dead/Projects/letibot/crates/tools")));
        assert!(v.outside(Path::new("/home/dead/.ssh/id_rsa")));
        assert!(v.outside(Path::new("/home/dead/Projects/other")));
        assert!(v.outside(Path::new("/root/.ssh/id_rsa")));
        assert!(v.outside(Path::new("/etc/shadow")));
        // `/usr` is a system root, so it is in the view and must not produce a note.
        assert!(!v.outside(Path::new("/usr/bin/cc")));
        // The parent chain to the project exists inside the view as empty
        // directories, so a listing of it succeeds and calling it absent would be
        // a note about something the command CAN see.
        assert!(!v.outside(Path::new("/home/dead")));
        assert!(!v.outside(Path::new("/home")));
        // A relative path is not this function's question.
        assert!(!v.outside(Path::new("crates/tools")));
    }

    #[test]
    fn a_granted_path_is_in_the_view_and_its_consequence_is_printed() {
        let v = ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
            path: PathBuf::from("/home/dead/.cargo"),
            why: "the crate registry cache, so a build does not re-download".into(),
        });
        assert!(!v.outside(Path::new("/home/dead/.cargo/registry")));
        assert!(v.outside(Path::new("/home/dead/.ssh")));
        let s = v.summary();
        assert!(s.contains("READABLE INTO CONTEXT"), "{s}");
        assert!(s.contains("re-download"), "{s}");
    }

    #[test]
    fn an_absent_path_is_reported_as_absent_from_the_view_and_not_as_missing() {
        // **Rule 4.** A bare ENOENT is read as "the file does not exist", which is
        // an empty-haystack answer.
        let b = boundary_with(vec![]);
        let notes = b.absence_notes("cat: /home/dead/.ssh/id_rsa: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert!(n.contains("not in this session's filesystem view"), "{n}");
        assert!(n.contains("/home/dead/.ssh/id_rsa"), "{n}");
        assert!(n.contains("another spelling"), "{n}");
        // The note must not claim the path exists on the host. It is not stat'ed
        // and the sentence does not assert it either way.
        assert!(!n.contains("exists on"), "{n}");
    }

    #[test]
    fn a_session_tmp_is_bound_at_tmp_and_nothing_replaces_it() {
        // **The two-call scratch idiom.** A command's `/tmp` was a fresh tmpfs, so a file one
        // call wrote was gone in the next — and nothing announces it (`ls -d /tmp` succeeds).
        // The replacement is a bind of the session's own directory, and this test is the argv
        // half: `--tmpfs /tmp` must be gone and `--bind <dir> /tmp` must be there.
        let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone())),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        let joined = a.join(" ");
        assert!(!joined.contains("--tmpfs /tmp"), "{joined}");
        assert!(
            joined.contains(&format!("--bind {} /tmp", dir.display())),
            "the session's own directory, bound at /tmp: {joined}"
        );
        // `/var/tmp` and `/run` are still fresh filesystems: the one thing `/var/tmp` means on
        // a host is *survives a reboot*, which no sandbox directory of ours may claim, and
        // `/run` holds the agent socket and the tmpfs home.
        assert!(joined.contains("--tmpfs /var/tmp"), "{joined}");
        assert!(joined.contains("--tmpfs /run"), "{joined}");

        // And the default view is unchanged: one fresh tmpfs at `/tmp`, bound to nothing.
        let d = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj"),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        )
        .join(" ");
        assert!(d.contains("--tmpfs /tmp"), "{d}");
    }

    #[test]
    fn a_replaced_tmp_says_the_session_owns_it_and_not_that_it_starts_empty() {
        // The disclosure and the absence note both read off the view, so a `/tmp` that outlives
        // a command must not describe itself as one that does not.
        let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
        let v = ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone()));
        let Presence::Replaced { what } = v.classify(Path::new("/tmp/host-file")) else {
            panic!("a session /tmp is still not the host's /tmp");
        };
        assert!(what.contains("this session's own"), "{what}");
        assert!(what.contains("survives between the calls"), "{what}");
        assert!(
            v.summary().contains(&dir.display().to_string()),
            "{}",
            v.summary()
        );
        assert!(
            v.summary().contains("there for the next"),
            "{}",
            v.summary()
        );
        // The note an `ENOENT` under it produces follows the same fact.
        let b = Bwrap {
            helper: PathBuf::from("/usr/bin/bwrap"),
            boundary: Boundary {
                view: v,
                ..boundary_with(vec![])
            },
        };
        let notes = b.absence_notes("cat: /tmp/gone.txt: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("survives between the calls"),
            "{}",
            notes[0]
        );
        assert!(!notes[0].contains("starts empty"), "{}", notes[0]);
    }

    #[test]
    fn a_replaced_root_is_its_own_answer_and_not_either_of_the_other_two() {
        // **Found by a failing test, and it is the note that would have been
        // wrong.** `/tmp` exists inside the view, so calling it outside would claim
        // it cannot be reached; but it is a fresh tmpfs, so calling it inside leaves
        // a bare `ENOENT` about a host file that really is invisible.
        let v = ViewSpec::project_only("/proj");
        assert_eq!(v.classify(Path::new("/proj/src")), Presence::Inside);
        assert_eq!(v.classify(Path::new("/opt/thing")), Presence::Outside);
        assert!(matches!(
            v.classify(Path::new("/tmp/host-file")),
            Presence::Replaced { .. }
        ));
        assert!(matches!(
            v.classify(Path::new("/proc/1234/cmdline")),
            Presence::Replaced { .. }
        ));
        // A project that lives under `/tmp` is Inside, not Replaced: the bound roots
        // are checked first, and getting that order wrong would tell every test
        // fixture in this crate that its own workspace was a tmpfs.
        let under_tmp = ViewSpec::project_only("/tmp/letibot-fixture");
        assert_eq!(
            under_tmp.classify(Path::new("/tmp/letibot-fixture/src")),
            Presence::Inside
        );

        let b = boundary_with(vec![]);
        let notes = b.absence_notes("cat: /tmp/host-file: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("fresh tmpfs"), "{}", notes[0]);
        assert!(notes[0].contains("starts empty"), "{}", notes[0]);
        // And it must NOT say the path is outside the view, because it is not.
        assert!(
            !notes[0].contains("not in this session's filesystem view"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn a_miss_inside_the_view_gets_no_boundary_note() {
        // The false positive that would matter: a genuinely missing project file
        // blamed on the boundary would send the model looking for a grant it does
        // not need.
        let b = boundary_with(vec![]);
        assert!(
            b.absence_notes("cat: /proj/src/nope.rs: No such file or directory")
                .is_empty()
        );
        assert!(b.absence_notes("hello, nothing is missing here").is_empty());
        // And a relative path, which is what most tools print.
        assert!(
            b.absence_notes("cat: src/nope.rs: No such file or directory")
                .is_empty()
        );
    }

    #[test]
    fn the_absence_note_is_bounded_like_a_success_body() {
        // §5: do not let a miss produce an unbounded reply.
        let b = boundary_with(vec![]);
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("cat: /opt/secret{i}: No such file or directory\n"));
        }
        let notes = b.absence_notes(&text);
        assert_eq!(notes.len(), 1);
        let listed = notes[0].matches("/opt/secret").count();
        assert!(listed <= MAX_ABSENCE_PATHS, "listed {listed}");
    }

    #[test]
    fn an_agent_socket_is_forwardable_and_a_key_file_is_not() {
        // §3's subtle case, as a test. The mechanism accepts a socket and refuses
        // anything else, because "usable without being readable" is a property of
        // the socket and not of the name.
        let dir = std::env::temp_dir().join(format!("letibot-agent-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let key = dir.join("id_rsa");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").expect("fixture key");

        // Not a socket: refused, with the mechanism named.
        // SAFETY: single-threaded test scope; the variable is restored below.
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &key) };
        let e = Grant::agent_from_env("authorised ssh").unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("not a socket"), "{m}");
        assert!(m.contains("Nothing was bound"), "{m}");

        // Absent: refused, and the refusal names ssh-agent rather than a bind.
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        let e = Grant::agent_from_env("authorised ssh").unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("ssh-add"), "{m}");
        assert!(m.contains("never the key"), "{m}");
        assert!(m.contains("blocked"), "{m}");
        // And it names the SECOND decision the authorised case needs.
        assert!(m.contains("Egress::Host"), "{m}");

        // A real socket: accepted, and it is bound at a fixed path so the
        // operator's `/run/user/<uid>` path does not travel.
        let sock = dir.join("agent.sock");
        let _ = std::fs::remove_file(&sock);
        let l = std::os::unix::net::UnixListener::bind(&sock).expect("bind agent socket");
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
        let g = Grant::agent_from_env("the operator said `ssh build-box`").expect("socket");
        assert!(matches!(g, Grant::AgentSocket { .. }));
        assert!(g.line().contains("NOT readable"), "{}", g.line());
        assert!(g.line().contains(AGENT_SOCKET), "{}", g.line());
        drop(l);
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_argv_puts_the_project_in_and_leaves_the_home_directory_out() {
        let view = ViewSpec::project_only("/home/dead/Projects/letibot");
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/home/dead/Projects/letibot/crates"),
                env: &[("CARGO_TERM_COLOR".into(), "never".into())],
            },
        );
        let joined = a.join(" ");
        for f in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(joined.contains(f), "missing {f} in {joined}");
        }
        // The project is bound read-write at its own path.
        let i = a.iter().position(|x| x == "--bind").expect("a --bind");
        assert_eq!(a[i + 1], "/home/dead/Projects/letibot");
        assert_eq!(a[i + 2], "/home/dead/Projects/letibot");
        // `$HOME` is NOT the operator's home, and the operator's home is not bound.
        let home = a.iter().position(|x| x == "HOME").expect("HOME is set");
        assert_eq!(a[home + 1], TMPFS_HOME);
        assert!(
            !a.iter().any(|x| x == "/home/dead"),
            "the operator's home must never be an argument: {joined}"
        );
        // The caller's env survives `--clearenv`, because it is re-declared.
        assert!(joined.contains("CARGO_TERM_COLOR never"), "{joined}");
        // And the command is separated from the options by `--`, so a command
        // beginning with a dash is a command.
        assert_eq!(a.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn declared_egress_drops_the_network_unshare_and_nothing_else() {
        let view = ViewSpec::project_only("/proj");
        let denied = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        let host = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Host {
                why: "authorised".into(),
            },
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        assert!(denied.iter().any(|a| a == "--unshare-net"));
        assert!(!host.iter().any(|a| a == "--unshare-net"));
        // The one difference, and only that one: everything else is identical.
        let d: Vec<&String> = denied.iter().filter(|a| *a != "--unshare-net").collect();
        let h: Vec<&String> = host.iter().collect();
        assert_eq!(d, h);
    }

    #[test]
    fn a_boundary_a_child_can_unshare_out_of_is_not_confined() {
        // **The hole the survey caught.** Mount, PID and network namespaces entered
        // by a parent are decoration if the child can make its own. So an open seal
        // fails `complete()` — it does not merely make the boundary partial, because
        // "partial" implies the rest still holds and this is what makes the rest
        // hold.
        let mut b = boundary_with(vec![]);
        assert!(b.complete());
        b.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Open {
                why: "`unshare -U true` inside the boundary SUCCEEDED".into(),
            },
        );
        assert!(!b.complete(), "an open seal is not a boundary");
        let d = b.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("seal is open"), "{d}");
        assert!(d.contains("**OPEN**"), "{d}");
        assert!(d.contains("cannot `unshare` itself back out"), "{d}");
    }

    #[test]
    fn a_seal_that_could_not_be_tested_is_partial_and_never_held() {
        // An absent measurement is not a passing one. It still confines — the
        // namespaces were measured — but the disclosure must not read as though
        // everything was checked.
        let mut b = boundary_with(vec![]);
        b.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Unverified {
                why: "`unshare` is not in this view".into(),
            },
        );
        assert!(b.complete(), "unknown is not open");
        assert!(b.partial(), "and unknown is not held either");
        let d = b.describe();
        assert!(d.contains("PARTIALLY"), "{d}");
        assert!(d.contains("UNVERIFIED"), "{d}");
    }

    #[test]
    fn the_seals_are_tested_and_a_missing_verdict_is_unknown_not_ok() {
        // The parse, which is where "an absent measurement counts as a pass" would
        // creep in.
        let held = parse_seals("ns user user:[1]\nseal nested held\nseal nnp 1\n");
        assert!(held[&SealKind::NestedNamespaces].held());
        assert!(held[&SealKind::NoNewPrivs].held());

        let open = parse_seals("seal nested open\nseal nnp 0\n");
        assert!(matches!(
            open[&SealKind::NestedNamespaces],
            Seal::Open { .. }
        ));
        assert!(matches!(open[&SealKind::NoNewPrivs], Seal::Open { .. }));

        // Nothing at all: unknown, both of them.
        let silent = parse_seals("ns user user:[1]\n");
        for k in SealKind::ALL {
            assert!(
                matches!(silent[&k], Seal::Unverified { .. }),
                "{k} must be unknown, not held"
            );
        }
        // `unshare` absent from the view is its own reason, and it names the
        // independent mechanism without claiming that mechanism as a measurement.
        let no_tool = parse_seals("seal nested unverified\nseal nnp 1\n");
        let Seal::Unverified { why } = &no_tool[&SealKind::NestedNamespaces] else {
            panic!("expected unverified");
        };
        assert!(why.contains("assert-userns-disabled"), "{why}");
        assert!(why.contains("not a measurement"), "{why}");
    }

    #[test]
    fn the_argv_seals_the_boundary_and_asserts_the_seal_took() {
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj"),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        assert!(a.iter().any(|x| x == "--disable-userns"), "{a:?}");
        assert!(
            a.iter().any(|x| x == "--assert-userns-disabled"),
            "the request must be checked by the helper too: {a:?}"
        );
    }

    #[test]
    fn a_helper_setup_failure_is_not_reported_as_the_commands_answer() {
        // F5 in the shape that is easy to miss: the wrong answer here is not `ok`,
        // it is "the command ran and exited non-zero".
        let dir = std::env::temp_dir();
        let b = Bwrap {
            helper: PathBuf::from("/usr/bin/bwrap"),
            boundary: Boundary {
                view: ViewSpec::project_only(&dir),
                ..boundary_with(vec![])
            },
        };
        let n = b
            .launcher_failure("bwrap: Can't bind mount /nope on /nope: No such file\n")
            .expect("a bwrap diagnostic is a launcher failure");
        assert!(n.contains("NOT the command's answer"), "{n}");
        assert!(n.contains("may never have run"), "{n}");
        // And ordinary output is not misread as one.
        assert!(
            b.launcher_failure("error: could not compile `x`\n")
                .is_none()
        );
        assert!(b.launcher_failure("").is_none());
    }

    #[test]
    fn a_readback_that_matches_the_harness_is_not_a_boundary() {
        // The parse and the comparison, without needing a helper: this is the
        // logic that decides `Entered` from `NotEntered`, and it is the logic that
        // would let a hard-coded `confined` through if it were wrong.
        let inside = parse_readback("ns user user:[42]\nns mnt mnt:[43]\nnoise\nns pid\n");
        assert_eq!(inside.get("user"), Some(&"user:[42]".to_string()));
        assert_eq!(inside.get("mnt"), Some(&"mnt:[43]".to_string()));
        assert_eq!(
            inside.get("pid"),
            None,
            "a line with no inode is not a reading"
        );
        assert!(parse_readback("total nonsense").is_empty());
    }

    #[test]
    fn a_missing_helper_is_a_refusal_that_names_the_kernel_policy_too() {
        // The refusal has to be actionable. On this box the interesting half is
        // not "install bubblewrap" but *why* a direct unshare is not the answer.
        // SAFETY: single-threaded test scope; restored below.
        unsafe { std::env::set_var(BWRAP_ENV, "/nonexistent/bwrap") };
        let e = find_helper().unwrap_err();
        unsafe { std::env::remove_var(BWRAP_ENV) };
        let m = format!("{e}");
        assert!(m.contains("/nonexistent/bwrap"), "{m}");
        assert!(m.contains("was NOT run"), "{m}");
    }
}
