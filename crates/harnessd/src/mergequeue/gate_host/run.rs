//! The record a gate run leaves in its run directory on the machine that runs it: who started it,
//! which tree it is judging, which step it is on. One repository runs ONE gate at a time on one
//! host, whatever daemon started it - so a daemon that takes the queue reads this record and waits
//! for that run's verdict (or takes the directory from an owner that is gone) instead of starting a
//! second suite beside it. It is also what a pane and `letibot holds` on that machine list.
//!
//! The format is `key=value`, one per line, because `scripts/gate-at.sh` writes the same file: a
//! shape only this build can produce would say nothing about a hand-started gate.

/// The directory inside a gate's `<root>/gate/<repo>-<hex>/run` that a run holds - one `mkdir`,
/// which is atomic on every host, so two daemons cannot both think they made it.
pub const LOCK: &str = "lock";
/// The record itself, inside the lock.
pub const OWNER: &str = "owner";
/// The run's heartbeat, poked by every poll - a file rather than a field, because a step can run
/// for half an hour between two writes of the record.
pub const BEAT: &str = "beat";

/// How long a run may be unheard of before its directory is anyone's to take. The gate pokes every
/// poll (one round trip each), so a silence this long is a daemon that is gone, not a slow step.
pub const STALE_MS: u64 = 90 * 1000;

/// The owner a hand-started gate records, and the run id it carries.
pub const HAND: &str = "hand";

/// One run, as the machine running it records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owner {
    /// `<host>/<owner>` of the daemon, or [`HAND`] - empty for a lock left by nobody this build can
    /// name, which is a run that died between its `mkdir` and its first write.
    pub daemon: String,
    /// This run's own name in the daemon's `gate_live` row.
    pub run_id: String,
    /// The `<repo>-<hex>` the run directory is named after, which is the repository.
    pub repository: String,
    /// The branch under the gate.
    pub target: String,
    /// The commit it is judging.
    pub sha: String,
    /// 1-based, as a person counts; `0` until the first step starts.
    pub step: usize,
    pub of_step: usize,
    pub command: String,
    pub started_ms: u64,
    /// When the record was last written. Staleness is read off the [`BEAT`] file's own clock on the
    /// host instead - two machines' clocks are not the same clock.
    pub beat_ms: u64,
}

impl Owner {
    /// The record, as the file holds it. Newlines are flattened: one line per field or a command
    /// carrying one would split the record.
    pub fn render(&self) -> String {
        let flat = |s: &str| s.replace(['\n', '\r'], " ");
        let mut out = String::new();
        for (k, v) in [
            ("daemon", self.daemon.clone()),
            ("run_id", self.run_id.clone()),
            ("repository", self.repository.clone()),
            ("target", self.target.clone()),
            ("sha", self.sha.clone()),
            ("step", self.step.to_string()),
            ("of_step", self.of_step.to_string()),
            ("command", flat(&self.command)),
            ("started_ms", self.started_ms.to_string()),
            ("beat_ms", self.beat_ms.to_string()),
        ] {
            out.push_str(&format!("{k}={v}\n"));
        }
        out
    }

    /// The record a run left, or `None` when the file is not one - a truncated write, or somebody
    /// else's file. `None` is not *no run*: the lock is the answer to that.
    pub fn parse(text: &str) -> Option<Owner> {
        let field = |k: &str| {
            text.lines().find_map(|l| {
                l.strip_prefix(k)
                    .and_then(|rest| rest.strip_prefix('='))
                    .map(str::to_string)
            })
        };
        let daemon = field("daemon")?;
        Some(Owner {
            daemon,
            run_id: field("run_id").unwrap_or_default(),
            repository: field("repository").unwrap_or_default(),
            target: field("target").unwrap_or_default(),
            sha: field("sha").unwrap_or_default(),
            step: field("step").and_then(|n| n.parse().ok()).unwrap_or(0),
            of_step: field("of_step").and_then(|n| n.parse().ok()).unwrap_or(0),
            command: field("command").unwrap_or_default(),
            started_ms: field("started_ms")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            beat_ms: field("beat_ms").and_then(|n| n.parse().ok()).unwrap_or(0),
        })
    }

    /// The record with the step it is on now, and the instant it was written.
    pub fn at_step(&self, step: usize, command: &str, now: u64) -> Owner {
        Owner {
            step,
            of_step: self.of_step,
            command: command.to_string(),
            beat_ms: now,
            ..self.clone()
        }
    }

    /// What this run is, as one line a person reads: `letibot-1f2a@e74362d9 · step 4 of 6 ·
    /// lab2x1/gate-3` - the repository and tip, the step, and who started it.
    pub fn words(&self) -> String {
        let short: String = self.sha.chars().take(10).collect();
        let at = dir_tail(&self.repository);
        let step = match (self.step, self.of_step) {
            (n, of) if n > 0 && of > 0 => format!("step {n} of {of}"),
            _ => "not on a step yet".to_string(),
        };
        let daemon = match self.daemon.is_empty() {
            true => "an owner nobody recorded".to_string(),
            false => self.daemon.clone(),
        };
        format!("{at}@{short} · {step} · {daemon}")
    }
}

/// A run directory is named `<repo>-<hex>`; the tail of it is what a person reads as the repository.
pub fn dir_tail(dir: &str) -> String {
    dir.rsplit('/').next().unwrap_or(dir).to_string()
}

/// What a daemon may do with a repository's run directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// Nobody is gating this repository here: this run makes the directory its own.
    Ours,
    /// Another run is alive in it: wait for its verdict rather than start a second suite beside it.
    Wait(Owner),
    /// A run nobody is driving: end whatever it left behind and take the directory.
    Stale(Owner),
}

/// The decision, as a value: what the run directory held, and how long ago the host last heard from
/// it (`age_ms`, measured ON the host - two machines' clocks are not the same clock).
pub fn claim_of(found: Option<Owner>, age_ms: u64) -> Claim {
    match found {
        None => Claim::Ours,
        // A lock nobody wrote a record in, or one beaten at fifteen minutes ago: the run is gone.
        Some(o) if o.daemon.is_empty() || age_ms >= STALE_MS => Claim::Stale(o),
        Some(o) => Claim::Wait(o),
    }
}

/// Take the run directory, or say who has it: one `mkdir` (atomic everywhere), and when it is taken
/// already, the record and the age of the last beat. The age is computed on the host, so a skewed
/// clock on either side changes nothing.
pub fn claim_script(lock: &str, beat: &str) -> String {
    format!(
        "if mkdir '{lock}' 2>/dev/null; then echo claim=mine; else echo claim=held; \
         b=$(cat '{beat}' 2>/dev/null); n=$(date +%s%3N); \
         echo age_ms=$((n - ${{b:-0}})); cat '{lock}/{OWNER}' 2>/dev/null; echo record-end; fi"
    )
}

/// The same question, read-only - what a pane and `holds` ask about a run they did not start.
pub fn read_script(lock: &str, beat: &str) -> String {
    format!(
        "b=$(cat '{beat}' 2>/dev/null); n=$(date +%s%3N); \
         echo age_ms=$((n - ${{b:-0}})); cat '{lock}/{OWNER}' 2>/dev/null; echo record-end"
    )
}

/// Give the directory back: the lock goes with its record, so the next run starts from nothing.
pub fn release_script(lock: &str) -> String {
    format!("rm -rf '{lock}'; echo released")
}

/// Write the run's heartbeat. The poll is already one round trip; this is the line in it that says
/// the daemon driving the gate is still here.
pub fn beat_line(beat: &str) -> String {
    format!("date +%s%3N > '{beat}' 2>/dev/null")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_owner(tag: &str) -> Owner {
        Owner {
            daemon: format!("lab2x1/{tag}"),
            run_id: "e74362d9-170".into(),
            repository: "letibot-1f2a".into(),
            target: "main".into(),
            sha: "e74362d9ae61c8f0".into(),
            step: 4,
            of_step: 6,
            command: "cargo nextest run".into(),
            started_ms: 1_000,
            beat_ms: 2_000,
        }
    }

    /// The record survives the round trip through its file, and its words name what a person needs:
    /// the repository, the tip, the step and the owner.
    #[test]
    fn the_record_round_trips_and_says_what_a_person_reads() {
        let o = an_owner("gate-1");
        let back = Owner::parse(&o.render()).expect("its own record parses");
        assert_eq!(back, o, "every field, both ways");
        assert_eq!(
            o.words(),
            "letibot-1f2a@e74362d9ae · step 4 of 6 · lab2x1/gate-1"
        );
        // A command with a newline in it stays one field: the record is one line per key.
        let bad = Owner {
            command: "cargo test\n--workspace".into(),
            ..o.clone()
        };
        assert_eq!(
            Owner::parse(&bad.render()).unwrap().command,
            "cargo test --workspace"
        );
        // A file that is not this record parses to nothing - which is not *no run*: the lock is.
        assert_eq!(Owner::parse(""), None);
        assert_eq!(Owner::parse("nonsense\n"), None);
        // And the heartbeat is the step's own record, so nothing goes stale while a step runs.
        assert_eq!(o.at_step(5, "cargo build", 9_000).step, 5);
        assert_eq!(o.at_step(5, "cargo build", 9_000).beat_ms, 9_000);
        assert_eq!(o.at_step(5, "cargo build", 9_000).started_ms, 1_000);
    }

    /// The decision a daemon makes at a run directory, as values: free, wait for a live owner, or
    /// take it from one that is gone.
    #[test]
    fn a_free_directory_is_taken_a_live_owner_is_waited_for_and_a_stale_one_is_taken_from() {
        assert_eq!(claim_of(None, 0), Claim::Ours, "no lock: nobody is gating");
        let live = an_owner("gate-1");
        assert_eq!(
            claim_of(Some(live.clone()), 5_000),
            Claim::Wait(live.clone()),
            "a run heard from five seconds ago is one to wait for"
        );
        assert_eq!(
            claim_of(Some(live.clone()), STALE_MS),
            Claim::Stale(live.clone()),
            "a run nobody has heard from in a minute and a half is gone"
        );
        assert_eq!(
            claim_of(Some(Owner::default()), 0),
            Claim::Stale(Owner::default()),
            "a lock with no record in it is a run that died before it started, not a live one"
        );
    }

    /// The scripts: one atomic `mkdir` for the claim, the host's own clock for the age, the record
    /// inside the lock, and a release that takes it with it.
    #[test]
    fn the_scripts_make_one_lock_and_read_the_age_on_the_host() {
        let claim = claim_script(
            "/h/gate/letibot-1f2a/run/lock",
            "/h/gate/letibot-1f2a/run/beat",
        );
        assert!(
            claim.contains("mkdir '/h/gate/letibot-1f2a/run/lock' 2>/dev/null"),
            "{claim}"
        );
        assert!(claim.contains("echo claim=mine"), "{claim}");
        assert!(claim.contains("echo claim=held"), "{claim}");
        assert!(
            claim.contains("n=$(date +%s%3N)"),
            "the age is the host's clock: {claim}"
        );
        assert!(
            claim.contains("cat '/h/gate/letibot-1f2a/run/lock/owner'"),
            "{claim}"
        );
        assert_eq!(
            release_script("/h/gate/x/run/lock"),
            "rm -rf '/h/gate/x/run/lock'; echo released"
        );
        assert_eq!(
            beat_line("/h/gate/x/run/beat"),
            "date +%s%3N > '/h/gate/x/run/beat' 2>/dev/null"
        );
        // The read-only spelling asks the same question and writes nothing.
        let read = read_script("/h/gate/x/run/lock", "/h/gate/x/run/beat");
        assert!(!read.contains("mkdir") && !read.contains("rm "), "{read}");
    }
}
