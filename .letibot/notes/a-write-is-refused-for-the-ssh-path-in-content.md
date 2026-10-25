<!-- abstract: `write` (and `notes add`) is refused by `boundary:host` when the CONTENT contains the ssh config directory's path — even inside a Rust test fixture quoting docs/plugins-slice1.md's C3 example fix; nothing runs and no grant overrides it. -->

# A `write` is refused when its content names the ssh config directory

MEASURED 2026-10-12, writing `crates/sessionlog/src/pane.rs`: the whole-file `write` came back
`denied (boundary:host)`, basis *"`.ssh` is on the never-write list (§11.4); no adjudicator is
consulted and none can override it"* — and nothing was executed. The same refusal then caught
the note that was going to record it, so this one says it without the literal.

The literal was the ssh config directory's path, in an `ssh-add …` line copied straight from
**C3 of `docs/plugins-slice1.md`** into a JSON fixture in a unit test. So the rule is about the
BYTES of the file being written, not about what the file does: a documentation example inside a
test trips the same guard as a script that would read the key.

What to do instead: put a different example in the fixture (`"add the lab2x1 key to the
agent"`) and quote the contract's words in the report, not in the tree. A refusal says *do not
retry a spelling* — the decision is the boundary's, before any adjudicator, and none can
override it.
