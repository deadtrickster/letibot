//! The shadow gate: build, load, shadow, replay, report — §2.4, the mechanical check every change
//! to a stack takes before it answers.
//!
//! > *"before applying we must check compilation etc on a shadow session - do a dry run when a
//! > temporary session copy has changes applied"*
//!
//! It is the merge gate's role for layers, and like the merge gate it is run by the daemon and not
//! by an agent: nobody who can be asked to verify their own fix is asked. The five steps run in
//! order and the first failure stops, because a report that mixes a build error into a replay diff
//! is read by a model that then edits the wrong thing.
//!
//! 1. **build** — the layer's source is built, offline, for `wasm32-unknown-unknown`, and the
//!    result is copied over `plugin.wasm`. What is applied is then what the source says: a
//!    hand-copied module that disagreed with its source has already been overwritten by the time
//!    step 2 reads it.
//! 2. **load** — compiled through the stack's own [`Loader`], then admitted: this host's contract,
//!    and a manifest whose `hooks` are exactly what the module exports.
//! 3. **shadow** — a **clone** of the stack with the candidate applied. Not the live one: a
//!    candidate the gate refuses must leave the session exactly as it was, which is the whole of
//!    *"a bad candidate never replaces the good one"* (§2.3). The shadow has no effects, and it
//!    never runs a look a hook declares: a look is the host's act on the world, and §2.4 step 3
//!    says a dry run runs only what is read-only. This crate does not run looks at all — the
//!    `look` field in an answer is data a caller acts on later.
//! 4. **replay** — every recorded input of every hook the candidate exports, through the live
//!    stack and the shadow stack. A trap, exhausted fuel or malformed output that the live stack
//!    did not also produce is a failure; the answers are diffed.
//! 5. **report** — the build log, the load result, the fuel per hook, and the diff. A change meant
//!    to alter behaviour has a diff and the diff is the point; a squash or a refactor must have
//!    none.
//!
//! The module the gate loaded is kept **on the candidate**, and [`crate::Stack::apply`] swaps in
//! exactly that one — so the module that answers is the module that was replayed, not a second
//! compile of the same bytes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::Value;

use crate::manifest::Manifest;
use crate::{Candidate, Module, Recorder, Stack, WASM, sha256_file};

/// How the gate's step 1 is performed, so tests never run cargo.
pub trait Builder: Send + Sync {
    /// Build `dir`'s source, install the result as `<dir>/plugin.wasm`, and return the build log.
    ///
    /// The log comes back whether the build worked or not: it is the report's first field and the
    /// thing the model fixes the source from. An error also returns it, because *"it failed"* with
    /// no compiler output is a report nobody can act on.
    fn build(&self, dir: &Path) -> Result<String, BuildError>;
}

/// A build that did not produce a module, and the log it produced on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    pub why: String,
    pub log: String,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for BuildError {}

/// The production builder: `cargo build --offline --release --target wasm32-unknown-unknown` in
/// the layer's directory, then the crate's own `.wasm` becomes `plugin.wasm`.
///
/// `--offline` is not a preference. A plugin build must not be the first thing in a session to
/// reach the network, and a dependency that is not in the local cache is then a refusal a person
/// can read instead of a build that stalls behind a registry.
///
/// This is the one place the crate starts a process, and it is not covered by a test — a test that
/// ran it would need the wasm target installed and a real crate to build, which is exactly what
/// this crate's tests exist not to need. The `Loader` seam is what is tested; this is the thin end
/// that production hangs on it.
pub struct CargoBuilder;

impl Builder for CargoBuilder {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        let out = Command::new("cargo")
            .current_dir(dir)
            // **The plugin's build is its own crate, in its own target directory.**
            //
            // MEASURED 2026-10-26, building a scaffolded plugin from a session whose daemon was
            // started with `CARGO_TARGET_DIR` set: cargo put the guest at
            // `$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/<crate>.wasm` — outside the layer
            // this function then looks in — so a build that SUCCEEDED (`Finished release profile`)
            // was reported as *"cargo succeeded but there is no built module"*. The inheritance is
            // also wrong on its own terms: a guest is a different crate for a different target, and
            // sharing the host's target directory makes it contend for that directory's lock with
            // every build the session runs.
            .env_remove("CARGO_TARGET_DIR")
            .args([
                "build",
                "--offline",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .output()
            .map_err(|e| BuildError {
                why: format!("cargo: {e}"),
                log: String::new(),
            })?;
        let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
        log.push_str(&String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            return Err(BuildError {
                why: format!("cargo build failed ({})", out.status),
                log,
            });
        }
        let artifact = artifact(dir).ok_or_else(|| BuildError {
            why: format!(
                "cargo succeeded but there is no built module under {}",
                dir.join("target").display()
            ),
            log: log.clone(),
        })?;
        let wasm = dir.join(WASM);
        std::fs::copy(&artifact, &wasm).map_err(|e| BuildError {
            why: format!("{}: {e}", wasm.display()),
            log: log.clone(),
        })?;
        Ok(log)
    }
}

/// Where cargo put the module: `<crate name with dashes as underscores>.wasm` under
/// `target/wasm32-unknown-unknown/release/`, in the layer directory or — for a layer that lives
/// inside a workspace — in the nearest ancestor that has a `target`.
fn artifact(dir: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let table: toml::Table = toml::from_str(&manifest).ok()?;
    let name = table
        .get("package")?
        .get("name")?
        .as_str()?
        .replace('-', "_");
    let relative = Path::new("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(format!("{name}.wasm"));
    dir.ancestors()
        .map(|root| root.join(&relative))
        .find(|candidate| candidate.is_file())
}

/// One input answered differently on the shadow than on the live stack: §2.4 step 5's *"these
/// inputs now answer differently, like this"*.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayDiff {
    pub hook: String,
    pub input: Value,
    /// What the live stack answers now — `None` for a mask or for nothing answering.
    pub live: Option<Value>,
    pub shadow: Option<Value>,
    /// The layer that answered each side, so a diff says which layer moved and not only what.
    pub live_by: Option<String>,
    pub shadow_by: Option<String>,
}

/// What the gate found. Every step's own result is here even when the gate passed, because the
/// report is what the reviewer reads instead of redoing the work.
#[derive(Debug, Clone)]
pub struct GateReport {
    pub passed: bool,
    pub build_log: String,
    pub load: Result<(), String>,
    /// The fuel the candidate burned on its last call for each hook it exports and was replayed
    /// on: the cost per hook, which is the number that says whether a layer is about to make a
    /// pane expensive.
    pub fuel: Vec<(String, u64)>,
    pub diffs: Vec<ReplayDiff>,
    /// The first step that failed, named — `build: …`, `load: …`, `shadow: …`, `replay: …`.
    pub failure: Option<String>,
}

impl GateReport {
    /// Stop at this step. The report keeps what the steps before it found, which is what a person
    /// reading a refusal needs: a build log with no load result says exactly what happened.
    fn stop(mut self, why: String) -> GateReport {
        self.passed = false;
        self.failure = Some(why);
        self
    }
}

/// Run the gate over a candidate. See the module doc for the five steps and their order.
pub fn gate(stack: &Stack, rec: &Recorder, cand: &Candidate, builder: &dyn Builder) -> GateReport {
    let mut report = GateReport {
        passed: false,
        build_log: String::new(),
        load: Err("not run: the gate stopped before the load".to_string()),
        fuel: Vec::new(),
        diffs: Vec::new(),
        failure: None,
    };

    // 1. build — the source is what is applied, so the module read below is the one just built.
    match builder.build(&cand.dir) {
        Ok(log) => report.build_log = log,
        Err(e) => {
            report.build_log = e.log;
            return report.stop(format!("build: {}", e.why));
        }
    }

    // 2. load — fresh from disk, and the manifest fresh with it: the build may have moved the
    //    source and the plugin.toml beside it since the poll that made this candidate.
    let manifest = match Manifest::read(&cand.dir) {
        Ok(manifest) => manifest,
        Err(why) => {
            report.load = Err(why.clone());
            return report.stop(format!("load: {why}"));
        }
    };
    let wasm = cand.dir.join(WASM);
    let module: Arc<dyn Module> = match stack.loader().load(&wasm, &manifest.capabilities) {
        Ok(module) => Arc::from(module),
        Err(why) => {
            report.load = Err(why.clone());
            return report.stop(format!("load: {why}"));
        }
    };
    // `admit` is the contract and the export list together. Every export being a *known hook kind*
    // is already implied: the manifest's hooks were checked against [`crate::HOOK_PREFIXES`] when
    // the manifest was parsed, and this is the check that the two lists are the same list.
    let hooks = module.hooks();
    if let Err(why) = manifest.admit(&hooks) {
        report.load = Err(why.clone());
        return report.stop(format!("load: {why}"));
    }
    let sha = match sha256_file(&wasm) {
        Ok(sha) => sha,
        Err(why) => {
            report.load = Err(why.clone());
            return report.stop(format!("load: {why}"));
        }
    };
    cand.remember(&sha, module.clone());
    report.load = Ok(());

    // 3. shadow — a clone with the candidate applied. The live stack is not touched, so a
    //    candidate refused below leaves the session answering exactly as it was.
    let mut shadow = stack.clone();
    if let Err(why) = shadow.apply(cand.clone()) {
        return report.stop(format!("shadow: {why}"));
    }

    // 4. replay — the same inputs through both stacks, side by side.
    for hook in module.hooks() {
        let inputs = rec.inputs(&hook);
        if inputs.is_empty() {
            continue;
        }
        for input in inputs {
            let live = stack.call(&hook, input);
            let after = shadow.call(&hook, input);
            // A failure the live stack did not also record was introduced by this change. One it
            // did is the session's existing bug, and a candidate is not refused for inheriting it.
            if let Some(fresh) = after.failures.iter().find(|f| !live.failures.contains(f)) {
                return report.stop(format!(
                    "replay: `{hook}` fails where it did not before — {fresh}"
                ));
            }
            if live.answer != after.answer {
                report.diffs.push(ReplayDiff {
                    hook: hook.clone(),
                    input: input.clone(),
                    live: live.answer,
                    shadow: after.answer,
                    live_by: live.by,
                    shadow_by: after.by,
                });
            }
        }
        report.fuel.push((hook.clone(), module.fuel()));
    }

    report.passed = true;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Scope;
    use crate::testing::{
        Act, FakeBuilder, FakeLoader, TempDir, layer, script, write_wasm, write_wasm_bytes,
    };
    use serde_json::json;

    /// A live stack with one session layer that answers `pane_dashboard`, and that layer's
    /// directory.
    fn live(t: &TempDir, act: Act) -> (Stack, PathBuf) {
        let session = t.path().join("session");
        layer(&session, "a", &["pane_dashboard"], act);
        let stack = Stack::discover(Some(&session), None, None, Arc::new(FakeLoader::new()));
        (stack, session.join("a"))
    }

    fn recorder() -> Recorder {
        let mut rec = Recorder::new();
        rec.record(
            "pane_dashboard",
            &json!({"v": 1, "size": {"w": 80, "h": 24}}),
        );
        rec
    }

    #[test]
    fn a_build_failure_streams_the_log_and_stops_the_gate() {
        let t = TempDir::new("gate-build");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::failing("cannot find value `plan`"),
        );

        assert!(!report.passed);
        assert!(report.failure.as_deref().unwrap().starts_with("build:"));
        assert!(report.failure.unwrap().contains("plan"));
        assert!(
            report.build_log.contains("E0425"),
            "the compiler's own words are the report"
        );
        assert!(report.load.is_err(), "a step that never ran says so");
        assert!(report.diffs.is_empty());
    }

    #[test]
    fn a_load_failure_stops_the_gate_before_the_shadow() {
        let t = TempDir::new("gate-load");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing_bytes("this is not a module"),
        );

        assert!(!report.passed);
        let why = report.failure.clone().unwrap();
        assert!(why.starts_with("load:"), "{why}");
        assert!(report.load.is_err());
        assert!(report.diffs.is_empty());
    }

    #[test]
    fn an_export_the_manifest_never_declared_is_a_load_failure() {
        let t = TempDir::new("gate-exports");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[
                ("pane_dashboard", Act::answer(json!({"title": "one"}))),
                ("on_tool_call", Act::answer(json!({"pass": true}))),
            ]),
        );
        assert!(!report.passed);
        let why = report.failure.unwrap();
        assert!(why.starts_with("load:"), "{why}");
        assert!(why.contains("on_tool_call"), "{why}");
    }

    #[test]
    fn a_trap_in_replay_fails_the_gate_even_though_the_module_loaded() {
        let t = TempDir::new("gate-trap");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::trap("fuel exhausted"))]),
        );
        assert!(!report.passed);
        assert!(
            report.load.is_ok(),
            "the module loads; it is the answer that fails"
        );
        let why = report.failure.unwrap();
        assert!(why.starts_with("replay:"), "{why}");
        assert!(why.contains("fuel exhausted"), "{why}");
        assert!(why.contains("s1"), "the failing layer is named: {why}");
    }

    #[test]
    fn a_behaviour_change_is_a_diff_and_the_live_stack_is_untouched() {
        let t = TempDir::new("gate-diff");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::answer(json!({"title": "two"})))]),
        );

        assert!(report.passed, "{:?}", report.failure);
        assert!(report.load.is_ok());
        assert_eq!(
            report.build_log,
            "Finished `release` profile [optimized] target(s)\n"
        );
        assert_eq!(report.fuel, vec![("pane_dashboard".to_string(), 7)]);
        assert_eq!(report.diffs.len(), 1);
        let diff = &report.diffs[0];
        assert_eq!(diff.hook, "pane_dashboard");
        assert_eq!(diff.input, json!({"v": 1, "size": {"w": 80, "h": 24}}));
        assert_eq!(diff.live, Some(json!({"title": "one"})));
        assert_eq!(diff.shadow, Some(json!({"title": "two"})));
        assert_eq!(diff.live_by.as_deref(), Some("s1"));
        assert_eq!(diff.shadow_by.as_deref(), Some("s1"));

        // The shadow was a clone: the live stack still answers what it answered.
        assert_eq!(
            stack.call("pane_dashboard", &json!({})).answer,
            Some(json!({"title": "one"})),
        );
    }

    #[test]
    fn an_identical_module_is_no_diff_even_though_its_bytes_moved() {
        let t = TempDir::new("gate-identical");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();

        // Same answers, different bytes: what a rebuilt module that changed nothing is.
        let mut rebuilt = script(&[("pane_dashboard", Act::answer(json!({"title": "one"})))]);
        rebuilt.push(b'\n');
        write_wasm_bytes(&dir, &rebuilt);
        let rebuilt = String::from_utf8(rebuilt).unwrap();

        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing_bytes(&rebuilt),
        );
        assert!(report.passed, "{:?}", report.failure);
        assert!(
            report.diffs.is_empty(),
            "a refactor must not diff: {:?}",
            report.diffs
        );
        assert_eq!(report.fuel.len(), 1);
    }

    #[test]
    fn a_hook_with_no_recorded_inputs_replays_nothing_and_diffs_nothing() {
        let t = TempDir::new("gate-empty");
        let (stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &Recorder::new(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::answer(json!({"title": "two"})))]),
        );
        assert!(report.passed);
        assert!(report.diffs.is_empty());
        assert!(report.fuel.is_empty(), "no replay, no fuel to report");
    }

    #[test]
    fn the_gated_module_is_the_one_apply_swaps_in() {
        let t = TempDir::new("gate-apply");
        let (mut stack, dir) = live(&t, Act::answer(json!({"title": "one"})));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::answer(json!({"title": "two"})))]),
        );
        assert!(report.passed);
        assert!(
            cand.gated(),
            "the gate kept the module it replayed on the candidate"
        );
        stack
            .apply(cand)
            .expect("the bytes are the ones the gate replayed");
        assert_eq!(
            stack.call("pane_dashboard", &json!({})).answer,
            Some(json!({"title": "two"})),
        );
    }

    #[test]
    fn a_change_that_fixes_a_failure_is_not_refused_for_it() {
        let t = TempDir::new("gate-fix");
        // The live layer traps; the candidate answers. A failure the live stack also recorded is
        // the session's existing bug, so the fix must pass.
        let (stack, dir) = live(&t, Act::trap("unreachable"));
        let cand = stack.candidate(&dir, Scope::Session).unwrap();
        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::answer(json!({"title": "fixed"})))]),
        );
        assert!(report.passed, "{:?}", report.failure);
        assert_eq!(
            report.diffs.len(),
            1,
            "answering where it used to trap is a difference"
        );
    }

    #[test]
    fn a_rebuilt_layer_keeps_its_id_and_its_place_after_the_gate_passes() {
        let t = TempDir::new("gate-reload");
        let session = t.path().join("session");
        layer(
            &session,
            "a",
            &["pane_dashboard"],
            Act::answer(json!({"title": "old"})),
        );
        layer(&session, "b", &["pane_dashboard"], Act::pass());
        let mut stack = Stack::discover(Some(&session), None, None, Arc::new(FakeLoader::new()));
        write_wasm(&session.join("a"), &["pane_dashboard"], Act::mask());
        let cand = stack
            .poll_changes()
            .into_iter()
            .next()
            .expect("a rewritten module is a candidate");
        assert_eq!(cand.layer, "s1");

        let report = gate(
            &stack,
            &recorder(),
            &cand,
            &FakeBuilder::writing(&[("pane_dashboard", Act::mask())]),
        );
        assert!(report.passed, "{:?}", report.failure);
        stack.apply(cand).unwrap();
        assert_eq!(
            stack
                .layers()
                .iter()
                .map(|l| l.id.as_str())
                .collect::<Vec<_>>(),
            vec!["s2", "s1"],
        );
        assert_eq!(
            stack.call("pane_dashboard", &json!({})).answer,
            None,
            "s2 passes down to the mask"
        );
    }
}
