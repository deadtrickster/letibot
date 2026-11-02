<!-- abstract: A one-line citation fence was never filled: the daemon glued the closer onto the label and scanned to the end of the reply, and the head had a second scanner keyed on the whole label. -->

# A one-line citation fence was never filled, and the two halves scanned differently

An empty fence labelled `path:start-end` is filled with those lines, in two spellings: the empty block, and the one-liner on a line of its own. Only the block worked.

Two causes. `label = t[fence.len()..].trim()` on a one-liner leaves the closing run glued onto the label, so the loop looked for a separate closer and never found one. And the body scan had no stopping rule, so an unclosed fence swallowed the rest of the reply and could take a later block's closer for its own.

The head had a second scanner (`crates/tui/src/ui/transcript/cite.rs`) with the same blind spot, and it matched a fence to a citation by the whole label while the daemon keys the label's first word - so a label saying more than the range was cited by one and never filled by the other.

Both now use `crates/transcript/src/citation.rs`: `fence_at(&lines, i)` and `range(label)`, where `Fence` carries the block's last line so a consumer skips it whole. An unclosed fence ends on the line before the next line that begins a fence, and reports `closed: false`.

Rake: a citation label that is an ABSOLUTE path can read the operator's disk, so a test must assert the fill's parts and not its bytes.
