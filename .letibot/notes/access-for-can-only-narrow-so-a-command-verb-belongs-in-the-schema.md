<!-- abstract: `ToolRuntime::run` honours `access_for` only when the answer is LESS consequential than the schema's class, so a tool declaring `Session` and answering `Exec` for one verb runs that verb unasked - the schema must declare the most consequential verb. -->

# `access_for` can only NARROW, so the tool's most consequential verb belongs in the schema

`ToolRuntime::run` (`crates/tools/src/runtime.rs`) takes the tool's own answer only when it is
unattended AND the schema's class is not:

```rust
.filter(|a| a.is_unattended() && !declared.is_unattended())
```

`Access::is_unattended()` is `Read | Session`, so `access_for` can narrow a gated class down to
`Read`/`Session` and nothing else. A tool that declares `Access::Session` and answers
`Some(Access::Exec)` for one verb has that answer filtered out: the effective class stays `Session`,
the call is never gated, and the verb that runs a compiler runs unasked. Nothing warns - the gate
simply never sees it.

The shape that is right: declare the tool's most consequential verb in `ToolSchema` and narrow the
rest in `access_for`. `queue` already does this (`Write` declared, `status` narrowing to `Read`), and
`plugin` is the recorded counter-example - its `build` verb runs `cargo` and was admitted by the
default fail-closed gate.

The test that catches it is a call through a `ToolRuntime` with NO gate attached (the `NoBoundary`
default) asserting the desk never heard it. A test with `crate::testing::allow_all()` proves the
opposite thing and passes either way, which is why both exist.
