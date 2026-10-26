<!-- abstract: MEASURED 2026-10-26 (RH5): the never-write precheck split every string argument on '/' AND the backslash, so a source file whose CONTENT quoted the operator's ssh directory was refused as if it were writing there — three refusals opened the write breaker for the whole session; the check now skips content/pattern arguments and asks the resolved path instead. -->

<!-- abstract: MEASURED 2026-10-26 (RH5): the never-write precheck split every string argument on '/' AND the backslash, so a source file whose CONTENT quoted the operator's ssh directory was refused as if it were writing there — three refusals opened the write breaker for the whole session; the check now skips content/pattern arguments and asks the resolved path instead. -->

# The never-write list refused prose, and cost a session its write tool

MEASURED 2026-10-26, package RH5 (`crates/tools/src/adjudicate.rs`), while trying to create one
source file whose subject *is* the operator's ssh directory.

## The mechanism, exactly

`never_hit(args)` took every string value of a tool call, split it on the two path separators, and
refused when a SEGMENT equalled an entry of `NEVER_WRITE`. So the rule was applied to the
**content** of a `write` (and to a `grep`'s pattern) as if the content were a path. Three shapes
cost three refusals, and each was one character away from the last:

| the file had | the segment that matched |
|---|---|
| the directory's name as the last path component of a joined path | `.ssh`, because of the separator before it |
| a doc comment that put another path component after it | `.ssh`, between two separators |
| a Rust string where an **escaped quote** followed it | `.ssh`, because the escape's backslash is a separator too |

The third one is the trap worth keeping: **the backslash splits as well as the slash**, so in Rust
source an escaped quote right after the directory name produces an exact segment. A markdown code
span (a backtick follows) or a `join("` with the name alone (no separator adjacent) is accepted —
that is not a way around the rule, it is the rule's own matcher, and it is why the fix belongs in
the gate and not in the content.

## What it cost

After the third refusal the **circuit breaker opened for `write`** — "open for
`write|write_file|host_project|host_other` after 3 consecutive refusals … only the operator lifts
this" — so the session could not write ANY file; `edit` still worked, and a new file cannot be made
with `edit`. The breaker counts by task direction, and a rule that refuses a whole file for one
segment inside it turns an argument-matcher's false positive into a lost tool.

The orchestrator's ruling (2026-10-26) was to narrow the rule rather than the content: a
replacement's text, a file's body and a search pattern are not paths and are not scanned
(`CONTENT_ARGUMENTS` in `adjudicate.rs`); a **path argument** still is, and so is `bash`'s
`command`, which is how a command names a path.

## And the half the list could never do

The literal check answers a *spelling*. A link in the workspace named `keys` and pointing at that
directory passes every segment test and lands in it, which is why RH5 added
`ssh_resolution_hit`: each path argument is resolved — every symlink, and a leaf that does not
exist yet — and refused if it lands in the home directory's ssh store. **A guard that reads
arguments as text is a guard a symlink routes around.**

## The rake, for the next session that hits it

A `write` coming back `denied (boundary:host)` with basis "… is on the never-write list (§11.4)"
is not about the file's path — it is about a segment ANYWHERE in an argument, including the body.
The answer is `never_hit`'s key filter, not a different path and not a different spelling; and if
the breaker has already opened, only the operator can lift it.
