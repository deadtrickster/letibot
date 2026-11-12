//! A search is bounded in two ways, and both are about the SUBSTRATE rather than the pattern: the
//! round trips one call costs, and the wall clock it may spend (R194).
//!
//! The fixture is a repository-sized tree - 300 directories, 1 500 files - because the defect is a
//! count, and no four-file fixture can show one. The backend is a real tree behind a meter, so the
//! count is of the calls a tool actually made and not of what it meant to make.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use letibot_tools::backend::{
    BackendError, Command, DirEntry, ExecBackend, HostBackend, Output, tempdir::TempDir,
};
use letibot_tools::builtins::retrieval::Unavailable;
use letibot_tools::events::RecordingToolSink;
use letibot_tools::runtime::{Limits, ToolRuntime};
use letibot_tools::{ProcessHost, ToolResult};
use letibot_transcript::{ToolCall, ToolOutcome};

/// 30 directories of 10, five files each.
const DIRS: usize = 300;
const FILES: usize = 1_500;
/// The one file a search is meant to find, so a healthy run stops at the rung the model asked for.
const HIT: &str = "d07/s03/f2.rs";

/// Every call the backend answered, by kind. The total is the round trips one tool call cost, which
/// is the number that turned a search into a hung turn: at ten seconds a call, 1 500 is four hours.
#[derive(Default)]
struct Meter {
    stats: AtomicUsize,
    trees: AtomicUsize,
    pages: AtomicUsize,
    reads: AtomicUsize,
}

impl Meter {
    fn round_trips(&self) -> usize {
        self.stats.load(Ordering::SeqCst)
            + self.trees.load(Ordering::SeqCst)
            + self.pages.load(Ordering::SeqCst)
            + self.reads.load(Ordering::SeqCst)
    }
    fn trees(&self) -> usize {
        self.trees.load(Ordering::SeqCst)
    }
    fn stats(&self) -> usize {
        self.stats.load(Ordering::SeqCst)
    }
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
    /// The census as one tuple: total round trips, and the four kinds.
    fn counts(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.round_trips(),
            self.trees(),
            self.pages.load(Ordering::SeqCst),
            self.reads(),
            self.stats(),
        )
    }
}

/// A real tree behind a channel that can be slow, can lose a page, and can lose every read - the
/// three shapes R194 was filed about - counting every call it answers.
struct Channel {
    inner: HostBackend,
    meter: Arc<Meter>,
    /// What one round trip costs. Zero for a healthy substrate.
    sleep: Duration,
    /// The page call fails and says nothing about any path: the fallback is chosen.
    fail_page: bool,
    /// Every single-file read fails: the channel is gone, not the files.
    fail_reads: bool,
}

impl Channel {
    fn new(root: &Path) -> Self {
        Channel {
            inner: HostBackend::new(root).expect("fixture root"),
            meter: Arc::new(Meter::default()),
            sleep: Duration::ZERO,
            fail_page: false,
            fail_reads: false,
        }
    }
    fn sleeping(mut self, sleep: Duration) -> Self {
        self.sleep = sleep;
        self
    }
    fn failing_page(mut self) -> Self {
        self.fail_page = true;
        self
    }
    fn failing_reads(mut self) -> Self {
        self.fail_reads = true;
        self
    }
    /// One round trip, priced: the sleep is what a sick channel costs per call, and the count is
    /// what the tool spent.
    fn paid(&self, counter: &AtomicUsize) -> Result<(), BackendError> {
        counter.fetch_add(1, Ordering::SeqCst);
        if !self.sleep.is_zero() {
            std::thread::sleep(self.sleep);
        }
        Ok(())
    }
}

impl ExecBackend for Channel {
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        self.inner.run(cmd)
    }
    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        self.paid(&self.meter.reads)?;
        if self.fail_reads {
            return Err(BackendError::Io("the VM went away".into()));
        }
        self.inner.read(path)
    }
    fn read_many(
        &self,
        paths: &[&str],
    ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
        self.paid(&self.meter.pages)?;
        if self.fail_page {
            return Err(BackendError::Io("the VM went away mid-page".into()));
        }
        Ok(paths.iter().map(|p| self.inner.read(p)).collect())
    }
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        self.inner.write(path, bytes)
    }
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
        self.inner.list(path)
    }
    /// One call for the whole tree, as `firecode`'s single `find` is: a backend that answered this
    /// per directory would be the loop `list_tree` exists to replace.
    fn list_tree(
        &self,
        root: &str,
        limit: usize,
        skip: &dyn Fn(&DirEntry) -> bool,
    ) -> Result<(Vec<DirEntry>, bool), BackendError> {
        self.paid(&self.meter.trees)?;
        self.inner.list_tree(root, limit, skip)
    }
    fn stat(&self, path: &str) -> Option<DirEntry> {
        let _ = self.paid(&self.meter.stats);
        self.inner.stat(path)
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn processes_arc(&self) -> Option<Arc<dyn ProcessHost>> {
        None
    }
}

/// A tree the size of a repository, with the needle in one file and nothing else to find.
fn repository_sized_tree(root: &Path) {
    for d in 0..(DIRS / 10) {
        for s in 0..10 {
            let dir = root.join(format!("d{d:02}/s{s:02}"));
            std::fs::create_dir_all(&dir).expect("fixture directory");
            for f in 0..(FILES / DIRS) {
                let body = if format!("d{d:02}/s{s:02}/f{f}.rs") == HIT {
                    "fn set_timeout() {}\nlet x = needle_here;\n".to_string()
                } else {
                    format!("fn filler_{d}_{s}_{f}() -> u8 {{ 0 }}\n")
                };
                std::fs::write(dir.join(format!("f{f}.rs")), body).expect("fixture file");
            }
        }
    }
}

/// One `grep` call over the tree, and the meter it left.
fn over_a_repository(
    channel: impl FnOnce(&Path) -> Channel,
    limits: Limits,
    arguments: &str,
) -> (ToolResult, Arc<Meter>, Duration) {
    let dir = TempDir::new();
    repository_sized_tree(dir.path());
    let channel = channel(dir.path());
    let meter = Arc::clone(&channel.meter);
    let registry = letibot_tools::read_only_tools(Arc::new(Unavailable)).expect("built-ins");
    let rt = ToolRuntime::new(registry, Box::new(channel)).with_limits(limits);
    let mut sink = RecordingToolSink::new();
    let call = ToolCall {
        id: "call_1".into(),
        name: "grep".into(),
        arguments: arguments.to_string(),
    };
    let started = Instant::now();
    let result = rt.invoke("turn_1", &call, &mut sink);
    (result, meter, started.elapsed())
}

fn healthy(root: &Path) -> Channel {
    Channel::new(root)
}

/// The count a tree-wide search costs when the substrate can batch: ONE call for the whole tree and
/// ONE per page of 64 files. 25 round trips for 1 500 files is the shape; a per-directory walk and a
/// per-file read were 1 800.
#[test]
fn a_tree_wide_search_is_one_walk_and_one_call_per_page() {
    let (result, meter, _) =
        over_a_repository(healthy, Limits::default(), r#"{"pattern":"needle_here"}"#);
    let seen = result.render();
    assert!(
        seen.contains("needle_here"),
        "the search found nothing:\n{seen}"
    );
    let pages = FILES.div_ceil(64);
    assert_eq!(
        (
            meter.trees(),
            meter.pages.load(Ordering::SeqCst),
            meter.reads(),
            meter.stats()
        ),
        (1, pages, 0, 0),
        "1500 files cost {} round trips: one walk, {pages} pages, no per-file read and one stat \
         for the whole call\n{seen}",
        meter.round_trips()
    );
}

/// A miss runs the whole ladder, and the ladder is where a walk got paid for three times: the rungs
/// that share a scope share one listing, and only the widened rung walks the root again.
#[test]
fn a_miss_walks_each_scope_once_and_not_once_per_rung() {
    let (result, meter, _) = over_a_repository(
        healthy,
        Limits::default(),
        r#"{"pattern":"quokka_sentinel","path":"d07"}"#,
    );
    let seen = result.render();
    assert!(seen.contains("relaxations tried"), "not a miss:\n{seen}");
    assert_eq!(
        meter.trees(),
        2,
        "the ladder walked {} time(s); `d07` is one listing and the widened root is the second\n{seen}",
        meter.trees()
    );
}

/// The channel that fails the page: the retry is bounded, so 1 500 candidates cost a handful of
/// round trips and a result that says the substrate stopped answering - not 1 500 reads at 120 s.
#[test]
fn a_page_that_fails_does_not_become_a_read_per_file() {
    let (result, meter, spent) = over_a_repository(
        |root| Channel::new(root).failing_page().failing_reads(),
        Limits::default(),
        r#"{"pattern":"needle_here"}"#,
    );
    let seen = result.render();
    assert!(
        meter.reads() <= letibot_tools::builtins::paging::FALLBACK_FAILED_READS,
        "the fallback read {} files one at a time:\n{seen}",
        meter.reads()
    );
    assert!(
        meter.round_trips() < 15,
        "a dying channel cost {} round trips:\n{seen}",
        meter.round_trips()
    );
    assert_eq!(
        result.outcome,
        ToolOutcome::Failed {
            reason: "the search could not read its tree: the VM went away".into()
        },
        "a search that could not read its tree is a failure, not a miss:\n{seen}"
    );
    assert!(seen.contains("stopped answering"), "{seen}");
    assert!(
        seen.contains("THE SEARCH DID NOT FINISH"),
        "the result does not say it is partial:\n{seen}"
    );
    assert!(spent < Duration::from_secs(20), "it took {spent:?}");
}

/// The bound itself: every call sleeps, the page call works, and the call stops at its wall clock
/// with what it had. The number asserted is the bound plus ONE call - the call in flight is the
/// only thing a cooperative stop cannot interrupt, and it is bounded by the substrate's own timeout.
#[test]
fn a_slow_channel_returns_a_partial_result_inside_the_bound() {
    let bound = Duration::from_millis(120);
    let (result, meter, spent) = over_a_repository(
        |root| Channel::new(root).sleeping(Duration::from_millis(40)),
        Limits {
            call_timeout: bound,
            ..Limits::default()
        },
        r#"{"pattern":"needle_here"}"#,
    );
    let seen = result.render();
    assert_eq!(
        result.outcome,
        ToolOutcome::Timeout,
        "a call past its bound must say it timed out:\n{seen}"
    );
    assert!(seen.contains("THE SEARCH DID NOT FINISH"), "{seen}");
    assert!(seen.contains("wall clock"), "{seen}");
    assert!(
        seen.contains("Narrow `path`"),
        "a timed-out search must say how to finish:\n{seen}"
    );
    assert!(
        spent < bound + Duration::from_millis(400),
        "the bound is {bound:?} and the call took {spent:?} over {} round trips",
        meter.round_trips()
    );
    // It stopped early rather than reading all 1 500 files: the point of the bound.
    assert!(
        meter.pages.load(Ordering::SeqCst) < FILES.div_ceil(64),
        "the call read every page before giving up"
    );
}

/// And the turn goes on: a call that hit its bound leaves the runtime able to run the next one.
#[test]
fn the_next_call_runs_after_one_has_timed_out() {
    let dir = TempDir::new();
    repository_sized_tree(dir.path());
    let registry = letibot_tools::read_only_tools(Arc::new(Unavailable)).expect("built-ins");
    let rt = ToolRuntime::new(
        registry,
        Box::new(Channel::new(dir.path()).sleeping(Duration::from_millis(40))),
    )
    .with_limits(Limits {
        call_timeout: Duration::from_millis(250),
        ..Limits::default()
    });
    let mut sink = RecordingToolSink::new();
    let slow = |id: &str, arguments: &str| ToolCall {
        id: id.into(),
        name: "grep".into(),
        arguments: arguments.into(),
    };
    let first = rt.invoke(
        "turn_1",
        &slow("call_1", r#"{"pattern":"needle_here"}"#),
        &mut sink,
    );
    assert_eq!(first.outcome, ToolOutcome::Timeout, "{}", first.render());
    // Narrow enough to finish inside the same bound, which is what the timed-out result advises.
    let second = rt.invoke(
        "turn_1",
        &slow("call_2", r#"{"pattern":"needle_here","path":"d07/s03"}"#),
        &mut sink,
    );
    assert_eq!(second.outcome, ToolOutcome::Ok, "{}", second.render());
    assert!(
        second.render().contains("needle_here"),
        "{}",
        second.render()
    );
}

/// The round-trip census, printed rather than asserted: the numbers in the package report are the
/// ones a change to the walk or the page has to move, and a test that only said "under 30" would
/// let a per-directory walk back in.
#[test]
fn the_round_trip_census() {
    let (hit, hit_meter, hit_spent) =
        over_a_repository(healthy, Limits::default(), r#"{"pattern":"needle_here"}"#);
    assert_eq!(hit.outcome, ToolOutcome::Ok);
    let (miss, miss_meter, miss_spent) = over_a_repository(
        healthy,
        Limits::default(),
        r#"{"pattern":"quokka_sentinel","path":"d07"}"#,
    );
    assert!(miss.render().contains("relaxations tried"));
    // The two channels R194 was filed about, at the cost the guest's own channel had: 10 ms a call.
    let (sick, sick_meter, sick_spent) = over_a_repository(
        |root| Channel::new(root).failing_page().failing_reads(),
        Limits::default(),
        r#"{"pattern":"needle_here"}"#,
    );
    assert!(sick.render().contains("THE SEARCH DID NOT FINISH"));
    let (slow, slow_meter, slow_spent) = over_a_repository(
        |root| Channel::new(root).sleeping(Duration::from_millis(10)),
        Limits::default(),
        r#"{"pattern":"needle_here"}"#,
    );
    assert_eq!(slow.outcome, ToolOutcome::Ok);
    eprintln!(
        "ROUND TRIPS over {FILES} files in {DIRS} directories (total, trees, pages, reads, stats):
  hit            {:?} in {hit_spent:?}
  scoped miss    {:?} in {miss_spent:?}
  dying channel  {:?} in {sick_spent:?}
  slow channel   {:?} in {slow_spent:?} (10 ms a call)",
        hit_meter.counts(),
        miss_meter.counts(),
        sick_meter.counts(),
        slow_meter.counts(),
    );
}
