<!-- abstract: `ExecBackend::read_many` turns a round-trip backend's per-file reads into pages of 64, and the two shell facts under it are that BSD `wc -c` pads its count and that a firecode backend's `in`-call count is not a file-read count. -->

# A batched read, and the two shell facts under it

Over a round-trip backend one `read` is one `firecode in` (about 0.5 s), so a `grep` over 200
candidate files was 200 calls and eight minutes with no tool result. `ExecBackend::read_many` defaults
to the per-file loop; firecode overrides it with one guest command printing each file as
`0x1e <length> \n <base64> \n`, and `0x1e` cannot occur in base64. `grep` and `outline`'s tree mode
page at 64 paths (`grep::READ_PAGE_FILES`) and fall back to per-file reads for a page whose batch call
fails.

Two rakes if you touch the guest command: BSD `wc -c` pads its count with spaces where GNU's does not,
so a `case "$n" in ''|*[!0-9]*)` guard reports every file unreadable on a Mac (use
`tr -d '[:space:]'`); and `up` keeps talking to the VM, so counting `in` lines around a page read
gives 6, not 1 - count the command text instead.

`crates/tools/src/remote.rs`'s `read` still pays one ssh round trip per file.
