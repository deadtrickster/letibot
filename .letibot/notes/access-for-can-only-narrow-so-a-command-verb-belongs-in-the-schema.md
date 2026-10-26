<!-- abstract: MEASURED 2026-10-26 (P2): `ToolRuntime::run` honours `access_for` only when the answer is LESS consequential than the schema's class, so a tool that declares `Session` and answers `Exec` for one verb runs that verb with nobody asked — the gate never sees it. The schema must declare the most consequential verb and `access_for` narrows the rest. -->

# `access_for` can only NARROW, so the tool's most consequential verb belongs in the schema

MEASURED 2026-10-26, package P2 (`crates/tools/src/builtins/plugin.rs`), first
run of `a_build_reaches_the_gate_before_the_desk`: a tool whose `build` verb runs
`cargo` was **admitted** by the default, fail-closed `NoBoundary` gate — which
refuses everything — and the desk ran the build.

## The mechanism, exactly

`ToolRuntime::run` (`crates/tools/src/runtime.rs`, ~line 2160):

```rust
let declared = schema.access;
let effective = self.registry.get(&call.name)
    .and_then(|t| t.access_for(&args))
    .filter(|a| a.is_unattended() && !declared.is_unattended())
    .unwrap_or(declared);
if gated && !effective.is_unattended() { /* the gate decides */ }
```

`Access::is_unattended()` is `Read | Session`. So the tool's own answer is used
ONLY when it is unattended **and** the schema is not — i.e. only to narrow a gated
class down to `Read`/`Session`. A tool that declares `Access::Session` and answers
`Some(Access::Exec)` for one verb has that answer FILTERED OUT: the effective class
stays `Session`, the call is never gated, and the verb that runs a compiler runs
unasked. Nothing warns.

## The shape that is right

Declare the tool's **most consequential verb** in `ToolSchema`, and narrow every
other verb in `access_for`:

```rust
ToolSchema::new(TOOL, "…", json!({…}), Access::Exec)   // `build`/`load` are commands
fn access_for(&self, args: &Value) -> Option<Access> {  // everything else narrows
    match args["action"].as_str() {
        Some("stack") | Some("test") => Some(Access::Read),
        Some("scaffold") | Some("disable") | Some("enable") => Some(Access::Session),
        _ => None,                                       // build/load: Exec stands
    }
}
```

`queue` already had this shape (`Write` declared, `status` narrowing to `Read`);
`flowy` is the tree's recorded example of the opposite mistake, and this is the
same defect one step further in — a tool whose gated verb is invisible to the
gate rather than to the operator.

**The test that catches it** is a call through a `ToolRuntime` with NO
`with_gate` (the `NoBoundary` default) asserting the desk never heard it:
`crates/tools/src/builtins/plugin.rs`'s `a_build_reaches_the_gate_before_the_desk`.
A test with `crate::testing::allow_all()` attached proves the opposite thing and
passes either way — which is why both exist.
