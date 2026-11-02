<!-- abstract: A whole-file `write` is refused by `boundary:host` when its CONTENT names the ssh config directory - a documentation example copied into a test fixture trips the same guard as a script that would read the key. -->

# A `write` is refused when its content names the ssh config directory

A whole-file `write` came back `denied (boundary:host)` with the basis *"`.ssh` is on the never-write
list (§11.4); no adjudicator is consulted and none can override it"*, and nothing ran. The literal
was the ssh config directory's path, inside an `ssh-add ...` line copied from C3 of
`docs/plugins-slice1.md` into a JSON fixture in a unit test.

So the rule is about the BYTES of the file being written, not about what the file does: a
documentation example inside a test trips the same guard as a script that would read the key. The
refusal also catches a note that tries to record it, so say it without the literal.

Put a different example in the fixture (`"add the lab2x1 key to the agent"`) and quote the contract's
words in the report, not in the tree. A refusal like this says do not retry a spelling: the decision
is the boundary's, taken before any adjudicator, and none can override it.
