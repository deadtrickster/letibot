<!-- abstract: An `R9x.n` in a comment at the fix site is how to tell a review item is already done - grep before working one - and a `TODO.md` entry marked OPEN is not evidence that anything in it is still broken. -->

# An `R9x.n` in the tree is how to tell a review item is already done

Every item this tree has fixed carries its number in a comment at the fix site, usually with a test
named after the defect, so the census is one command:

```sh
grep -rn 'R89\|R91\.[0-9]\|R92\.[0-9]' --include=*.rs crates | grep -o 'R9[12]\.[0-9]*' | sort -u -V
```

A `TODO.md` entry marked OPEN is not evidence that anything in it is still broken: the entries were
filed against a tree whose line numbers have drifted by thousands, and most are done. Grep first.

Three that were genuinely open, and what each taught. A threshold measured to the wrong clock - the
restore window was measured from the stop to the daemon's START while the rule names the stop to the
OPEN (`Stopped::restores`), which is hours apart for a session opened later; and a harness test
cannot backdate `Registry::started_ms()`, so stage the divergence in miniature: the old daemon stops
at the instant the new one started, the session opens 20 ms later, the threshold is 1 ms. A review
row for a branch with nothing on it - `enqueue_finished` read the head with `unwrap_or_default()`, so
a branch this host does not have became a row nothing can act on, which is a refusal rather than a
row. And a pane that hid a waiting row's reason - the detail drew `failure` only under
`attempts > 0`, while the rows that most need a reason carry one with `attempts` still 0.
