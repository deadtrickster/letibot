<!-- abstract: `never_hit` splits every non-content string argument on both separators and refuses a SEGMENT that equals a never-write entry, so prose quoting the ssh directory was refused as if it were writing there - and three refusals open the write breaker. -->

# The never-write list refused prose, and cost a session its write tool

`never_hit` (`crates/tools/src/adjudicate.rs:3397`) splits every string argument on `/` and on
the backslash, and refuses when a SEGMENT equals an entry of `NEVER_WRITE` (`:1257`). Content is
exempt (`CONTENT_ARGUMENTS`, `:3321`): a `write` body, a replacement's text, a search pattern.
`bash`'s `command` is not, so a heredoc's prose is scanned as a path - write prose with
`write`/`edit`. This is live: a `git grep` whose pattern named the directory was refused while
these notes were cut.

The cost is the breaker: three refusals open it for `write`, `write_file`, `host_project` and
`host_other`, and only the operator lifts it (`:2586`). `edit` still works; a new file cannot be
made with it.

A literal check answers a spelling, so `ssh_resolution_hit` also resolves every path argument -
symlinks, and a leaf that does not exist yet - and refuses if it lands in the home's ssh store.

Rule: a refusal saying "is on the never-write list" is about a segment anywhere in an argument,
not about the file's path. The answer is the key filter, not another path or another spelling.
