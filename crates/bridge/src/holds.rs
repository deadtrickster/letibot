//! `holds` and `gc_plan`: what an owner holds and what would be reaped, for EVERY agent.
//!
//! The operator's complaint is that cleanups have to be chased: a Claude Code seat cannot see its
//! own leftovers and the person ends up asking. So these are bridge tools, served over MCP to
//! whatever drives this folder, and they read the same index, the same discovery walk and the same
//! verdicts `letibot holds` and `letibot gc` print. Both are read-only: nothing here removes, stops
//! or writes anything, which is G1's whole contract (`docs/lifecycle.md` d.6 step 4).

use letibot_tools::lifecycle::{self, Places, Plan};
use serde_json::{Value, json};

use crate::tools::Bridge;

/// The store this bridge reads its folder's owners from: the one a test named, else the store of
/// the daemon serving the working directory - resolved the way every other bridge read does.
pub(crate) fn store_of(bridge: &Bridge) -> Result<std::path::PathBuf, String> {
    if let Some((store, _)) = bridge.names_source() {
        return Ok(store);
    }
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(crate::daemon::resolve(&here)?.store)
}

/// The plan for this folder's owners, read from the store and the disk. `only` narrows it to one
/// owner (a session id, or a package's id).
fn plan_of(bridge: &Bridge, only: Option<&str>) -> Result<Plan, String> {
    let store_path = store_of(bridge)?;
    if !crate::store::exists(&store_path) {
        return Err(format!(
            "there is no store at {}: this folder's daemon has not run here, so nothing is recorded \
             to hold anything",
            store_path.display()
        ));
    }
    let store = crate::store::open(&store_path, "owner", None)?;
    let places = Places::here();
    let now = letibot_tools::lifecycle::now_ms();
    lifecycle::plan(&store, &places, only, now).map_err(|e| e.to_string())
}

/// `holds`: every owner in this folder's store with what it holds, its sizes and its state - the
/// `holds:` line the operator reads, and the same facts as data so a model can act on them.
pub fn holds(bridge: &Bridge, owner: Option<&str>) -> Result<Value, String> {
    let plan = plan_of(bridge, owner)?;
    let mut out = plan.to_json();
    out["read_only"] = json!(true);
    out["store"] = json!(store_of(bridge)?.display().to_string());
    Ok(out)
}

/// `gc_plan`: what would be reaped and why, and when - the verdicts only, with the read-only
/// promise in the answer so a caller never has to assume it.
pub fn gc_plan(bridge: &Bridge, owner: Option<&str>) -> Result<Value, String> {
    let plan = plan_of(bridge, owner)?;
    let mut out = plan.to_json();
    out["plan"] = json!(plan.render());
    out["read_only"] = json!(true);
    out["acts"] = json!(
        "nothing: G1 is a plan. Reaping compute is G2, work is G3, caches and records are G4"
    );
    Ok(out)
}

/// The one line `leticode_status` carries for a session: its own `holds:` line, or `None` when the
/// owner was never recorded - a session opened before this build, which is not a failure.
pub fn status_line(bridge: &Bridge, session: &str) -> Option<String> {
    let plan = plan_of(bridge, Some(session)).ok()?;
    plan.owners.first().map(|o| o.holds_line(plan.now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{OwnerKind, OwnerRow, Store};

    /// A store of its own per test, under a directory that goes when it does.
    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let p = std::env::temp_dir()
                .join(format!("letibot-bridge-holds-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Fixture(p)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Both tools answer for a store holding one live owner, and neither changes it: the file's
    /// bytes are the same after each call.
    #[test]
    fn holds_and_the_plan_answer_what_the_owner_holds_and_change_nothing() {
        let f = Fixture::new("answer");
        let path = f.0.join("shared.db");
        let store = Store::open(&path).expect("a store");
        let owner = OwnerRow::live("s-42", OwnerKind::Package, Some("/w".into()), 1_000);
        store.put_owner(&owner).expect("an owner");
        store
            .put_holding(
                &letibot_tokencore::store::HoldingRow::new(
                    "s-42",
                    "target",
                    letibot_tokencore::store::HoldingClass::Cache,
                    "/w/target",
                    "owner.json",
                    1_000,
                )
                .measured(Some(4096)),
            )
            .expect("a holding");
        drop(store);
        let before = std::fs::read(&path).expect("the file");
        let bridge = Bridge::new().with_names(f.0.clone(), "/w");

        let answered = holds(&bridge, Some("s-42")).expect("holds answers");
        assert_eq!(answered["owners"][0]["owner"], "s-42");
        assert_eq!(answered["owners"][0]["kind"], "package");
        assert_eq!(answered["owners"][0]["holds"][0]["kind"], "target");
        assert_eq!(answered["read_only"], true);
        assert!(
            answered["owners"][0]["holds_line"]
                .as_str()
                .unwrap_or_default()
                .starts_with("holds:"),
            "{answered}"
        );

        let plan = gc_plan(&bridge, Some("s-42")).expect("the plan answers");
        assert_eq!(plan["read_only"], true);
        assert!(
            plan["plan"]
                .as_str()
                .unwrap_or_default()
                .contains("this is a plan"),
            "{plan}"
        );
        assert_eq!(
            std::fs::read(&path).expect("the file"),
            before,
            "a read-only tool changed the store"
        );
    }

    /// A folder with no store is a refusal that says why, not an empty list.
    #[test]
    fn no_store_is_said_rather_than_answered_with_nothing() {
        let f = Fixture::new("nostore");
        let bridge = Bridge::new().with_names(f.0.clone(), "/w");
        let e = holds(&bridge, None).expect_err("no store");
        assert!(e.contains("no store"), "{e}");
    }
}
