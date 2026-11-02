<!-- abstract: A built-in's tool is installed with a `plugin.toml` and no `plugin.wasm`, and discovery used to record that as two failures and warn `plugin_layer` on every session; the third state is a declaration, not a failure. -->

# A documented install was, to `Stack::discover`, a broken layer

`docs/plugins-plan.md` §4.8 installs a built-in's TOOL with one file: the layer directory holds a copy
of `plugin.toml` and no `plugin.wasm`, because the module that answers it is the one the binary carries.
`Stack::discover_scope` had no arm for that state - it ran `fingerprint(<dir>/plugin.wasm)` and
`loader.load(...)`, both of which answered the same `No such file or directory` - and the daemon
announced each as `plugin_layer` on every session in the project. The tool worked, because `Stack::run`
skips a `module: None` layer: the alarm was the only wrong thing.

`Stack::poll_changes` already called a layer with no `plugin.wasm` silent. The fix is `has_module()`: a
manifest with no module is a DECLARATION - listed, empty sha256, `module: None`, no failure, no warning
- while a `plugin.wasm` that exists and will not load is still a failure.

The rake: a state a doc calls correct must not be a `Failure`, and a test of a documented install has to
look at what the install SAYS about itself, not only at what it answers. A warning for a correct
install spends the one code that means this layer is installed and exploding.
