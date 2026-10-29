<!-- abstract: MEASURED 2026-10-29: the documented install of a built-in's tool (a `plugin.toml` and no `plugin.wasm`) made `Stack::discover` record TWO identical failures and the daemon announce them as `plugin_layer` warnings on every session in the project; `poll_changes` and `Layer::sha256` had already declared the same absent file harmless. The third state is now `has_module()` — declaration, silent, `module: None`. -->

# A documented install was, to `Stack::discover`, a broken layer

MEASURED 2026-10-29, branch `agent/answer-the-tool-mount-rejection`, answering a review rejection of
`agent/fix-tool-mount-landing` (`docs/plugins-plan.md` §4.8, `crates/plugins/src/stack.rs`).

## The install the docs describe, and what discovery did with it

§4.8 says a project installs a built-in's TOOL with **one file**:

```sh
mkdir -p .letibot/plugins/claude-transcript
cp plugins/builtin/claude-transcript/plugin.toml .letibot/plugins/claude-transcript/
```

The manifest is the ask and the schema the model is handed; the module that answers it is the one
the binary carries, which `PluginHost::production` puts at the bottom of that session's stack for
exactly the sessions that declared a tool from it. `Stack::discover_scope` had no arm for that
state. For a layer directory with a manifest and no `plugin.wasm` it ran

* `fingerprint(<dir>/plugin.wasm)` → `Failure { why: "…/plugin.wasm: No such file or directory (os
  error 2)" }`, and then
* `loader.load(<dir>/plugin.wasm, …)` → **the same sentence again** (a second `Failure`),

and `Sessions::open_plugins` → `PluginHost::announce` → `say_failures_now` turned each into
`SessionEvent::Warning { code: "plugin_layer" }` plus a notice queued for the model — on every
session in that project, for a correct install. The tool still worked, because `Stack::run` skips a
`module: None` layer: the alarm was the only wrong thing.

## Silence was already the tree's own rule for the same file

Two places already said the missing module was not news:

* `Layer::sha256`'s doc — *"The SHA-256 of `plugin.wasm` — or empty when there is no module to
  hash."*
* `Stack::poll_changes`'s doc — *"A layer with no `plugin.wasm` at all is silent — that is a build in
  progress, and a failure recorded every poll would be noise, not news."*

Discovery was the third place and the one that disagreed. The fix is `has_module()` (a
`symlink_metadata` that is `NotFound`, and nothing else): a layer directory with a manifest and no
module is a **declaration** — listed, `sha256` empty, `module: None`, no failure, no warning. A
`plugin.wasm` that exists and will not load is still a failure; so is a directory of that name; so
is a `plugin.toml` that does not parse. A module written beside the manifest later comes back from
`poll_changes` as a candidate, so a declaration is not a layer that can never answer — that half is
pinned by `a_manifest_with_no_module_is_a_declaration_and_not_a_failure` (stack.rs), and the whole
documented install by `the_builtin_transcript_layer_is_on_the_daemons_stack_and_answers_its_hook`
(harnessd/src/plugins.rs), which now looks at `stack.failures()`, at `hub.retained()` for
`plugin_layer` and at `take_notices()` — it asserted the answer and nothing else before.

## The rake, generally

**A state a doc calls correct must not be a `Failure`.** The test that pinned this path asserted the
tool's answer and never looked at the failure list or the notices, so the false alarm was invisible
to a green suite — *a test of a documented install has to look at what the install SAYS about
itself, not only at what it answers.* And a warning that fires for a correct install is worse than
no warning: it spends the one code that means *this layer is installed and exploding*.
