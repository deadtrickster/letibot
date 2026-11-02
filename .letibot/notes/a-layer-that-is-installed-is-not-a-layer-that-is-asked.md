<!-- abstract: A lazily built plugin host that only the session picker built left `/todo` with no stack, and the head's own verbs were a second list; a layer that spoke is the whole policy for its line. -->

# A plugin layer that is installed is not a layer that is asked - two ways a seam is silently dead

`App::plugin_host()` (`crates/tui/src/app/sessions_pane.rs`) is the one door that builds the plugin stack if it is stale. Before it, only the session picker's first draw built the host, so `/todo` in a head that never opened the picker found none and answered in the head's own words - with every test green, because the fallback draws a row too. Give a lazily built resource one door when a second caller appears.

The head's own verbs are one constant, `OWN_VERBS` (`crates/tui/src/app/commands.rs`), read by the gate that keeps them back from a layer; a `match` beside it would be a second list, and `app/tests/todo_layer.rs` is what keeps them one.

Only `pass` falls through to the head: an act is performed, any other answer performs nothing and says which act it refused, and a trap or fuel says so and the layer below answers. A layer that spoke is the whole policy for that line, so an unknown act must not fall through to the head's own add - that would turn a refusal into a row.
