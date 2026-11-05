<!-- abstract: A census test that greps source for one spelling goes blind the moment the call site changes spelling, and it fails green: `warning_codes.rs` kept passing with 64 of 160 codes until its vacuity guard fired, and the call shape it had to learn reads two arguments because one door names its session first. -->

# A census that greps a call shape goes green when the call moves

`crates/sessionlog/tests/warning_codes.rs` scans every `crates/**/*.rs` for the severity codes
named at their emission sites and fails until each has a row in `warning.rs`'s `TABLE`. It knew
`SessionEvent::Warning { code: "…" }`, `Warned { … }` (the head's own) and `import_note("code",
detail)`. Routing ~100 sites to `hub.warn` moved their codes into a fourth shape, and until the
scan learned it the census kept PASSING — with 64 codes instead of 160. The only thing that caught
it was its own vacuity guard, which names five codes the scan must see (`slash_refused` among them).

The shape to look for is `.warn(` and `.warn_with(` (a METHOD: a dot before the name) and
`::warning(` / `::warning_with(` (the constructor: a double colon), reading the FIRST TWO
arguments, because a `Sessions::warn` call names the session before the code. The leading character
is load-bearing: a bare `warn(` is `letibot_sessionlog::testing::warn(detail)`, whose argument is a
DETAIL, and reading it as a code failed the census with `careful`, `marker`, `a`, `b` and `w` from
`testing.rs`, `log.rs` and `scrub.rs` — one-word details all pass `looks_like_a_code`. A test's own
throwaway code is the same trap; write `let the_code = "some_code";` and the scan does not read it.

Two codes had been below the old horizon with no row, drawn as failures by default and decided by
nobody: `embeddings_unavailable` (a tier that stopped answering mid-session — the code
`docs/embeddings.md` names, beside `embedder_unavailable`, its start-up sibling) and
`memory_bound_broken` (memwatch's bound assertion, emitted through a local `fn warning(code,
detail)` wrapper, which is a shape no scan reads). Both are Failure rows now; `TABLE` is 185
(75 routine, 15 refused, 95 failures), pinned by
`the_table_is_75_routine_15_refused_and_95_failures`.
