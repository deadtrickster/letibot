<!-- abstract: X11's auto-repeat is one knob with two numbers, so setting either half must carry the other's current value; and "refused before anything runs" is a claim about the order of the acts. -->

# A host knob can be a pair, and "refused before anything runs" is an order, not an adjective

`xset q` prints `auto repeat delay: 660 repeat rate: 25`, and the write is `xset r rate <delay>
<rate>` - both at once. So `key-repeat-delay` and `key-repeat-rate` are two halves of one command, and a
`set` of either must read the other half first and pass both. A platform whose write needs a sibling
read is a shape the `read`/`apply` pair does not imply, and macOS's two keys are genuinely independent,
which is what makes it easy to miss.

`defaults read` exits 1 and says `does not exist` on stderr for a key never set. That is a fact -
`before: ""`, there was no such setting - not a failure, and a read that treats a non-zero exit as
unreadable refuses to change a setting macOS is happily running on its default. The distinction is the
error text.

The value check must come before the `before` read. A first cut validated inside `apply`, after `set`
had read the current value, so a test asserting nothing was applied failed with
`["defaults read -g KeyRepeat"]` - true that nothing was applied, false that nothing ran. The rake:
only a test asserting the runner recorded no command holds "refused before anything runs"; one
asserting the setting did not change passes either way.
