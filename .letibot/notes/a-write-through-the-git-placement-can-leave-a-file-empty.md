<!-- abstract: A placed session's write used to be able to truncate the target with no error - the chunked write discarded its exit statuses and ran an unconditional `mv` - which R99 fixed in `crates/tools/src/remote.rs`; `wc -c` after a write is still the cheap check. -->

# A placed session's `edit` can truncate the file it was told not to touch

Over the git placement a `write` used to be able to leave the target at 0 bytes and return no error:
`Ssh::write_abs` sent the file as 48 KiB chunks, discarded every chunk's exit status, and then ran
an unconditional `mv -f` over a temp name shared per path per process. The `edit` that read the file
afterwards answered *found no match ... Nothing was written*, truthfully, about a file that no longer
held anything.

R99 fixed it (`crates/tools/src/remote.rs`): one ssh session with the bytes on stdin, the far side
counts and hashes them and runs the `mv` only when both match, the temp name is unique per attempt
with a trap that removes it, and the bound scales with the payload. A failed or partial write now
leaves the target byte-identical and the tool says so.

Still worth doing: `wc -c` (or `wc -l`) after any write to a file you care about, and after a FAILED
one especially, because a zero-byte file is silent - the next `grep` answers "no match", which reads
like a wrong pattern. Recovery is `git checkout -- <path>` and redo the edits.

And the shape the incident pushed the work into is better anyway: new machinery goes into a new file
written once (`builtins/fanout_bg.rs`) rather than 500 lines inserted into an existing one.
