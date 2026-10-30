<!-- abstract: MEASURED 2026-10-29: `grep` over a round-trip backend was one `firecode in` per candidate file (200 files → 200 calls); `ExecBackend::read_many` makes it 4 (pages of 64), and `outline`'s tree mode the same (140 → 3). Two rakes under the guest command: BSD `wc -c` PADS its count with spaces, and a firecode backend's `up` keeps calling `in` afterwards, so an `in`-call count is not a file-read count. -->

# A batched read, and the two shell facts under it

MEASURED 2026-10-29, branch `agent/grep-does-not-read-a-file-at-a-time`, from the
operator's report that every review on the box takes the better part of an hour.

## The defect, in one number

A gatekeeper child in a firecode VM issued a repo-wide `grep` at 20:07:34 and its
transcript recorded nothing for eight minutes. It was not stuck: `grep`'s scan loop did
`ctx.backend.read(&e.path)` per candidate, and over firecode one `read` is one
`firecode in` — a client fork, the guest's `socat`, a `bash -l`, ~0.5 s apiece. The
guest's own call log was a stream of `base64 -w0 < …/<one file>` lines, one process
each. A few hundred files, several minutes, no tool result until the last one.

`ExecBackend::list_tree` had already been given exactly this fix for the walk
(`find` in one call, 122 directories → 1 call). `read` had not.

## What landed

`ExecBackend::read_many(&[&str]) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError>`
— default is the per-file loop (so every backend keeps today's behaviour), firecode
overrides it with one guest command that prints each file as
`0x1e <length> \n <base64> \n`. `0x1e` is safe because base64's alphabet cannot produce
it, so a file holding the marker, a NUL, a CR or a non-UTF-8 byte is not a boundary
problem. `grep` and `outline`'s tree mode page at 64 paths (`grep::READ_PAGE_FILES`,
which outline imports — the tree already has that precedent: `read.rs` imports
`grep::FILE_CEILING`) and fall back to per-file reads for a page whose batch call fails.

| tool | before | after |
|---|---|---|
| `grep`, 200 candidates | 200 `read` calls, 0 pages | 0 reads, **4** pages |
| `outline` tree, 140 parsable files | 140 `read` calls, 0 pages | 0 reads, **3** pages |

Through the backend (a stub that runs the real guest command over the copy `up` made):
four files, **one** `in` call.

`outline` also takes `grep::READ_PAGE_BYTES` (16 MiB a page), which there is the only
size bound a file has — that tool has no per-file ceiling, so one large file is still
read whole as it always was, and never 64 of them at once.

## Rake 1 — BSD `wc -c` PADS its count, GNU's does not

```text
$ wc -c < /tmp/xt/plain.txt
       8        # macOS: six spaces
```

The guest command reads each file's length first (`n=$(wc -c < "$f")`) so a slot can be
checked against the bytes decoded. A `case "$n" in ''|*[!0-9]*)` guard then called the
padded form a failure and reported **every file unreadable** — a batch read that answers
`NotFound` for everything, which a caller skips as "missing files". Fix: `tr -d '[:space:]'`.
GNU's `wc` does not pad, so a Linux guest would never have shown this; the test on the
Mac did.

## Rake 2 — `up` keeps talking to the VM, so the `in` log is not the read count

A test that counts `in` lines before and after a page read sees **6**, not 1: the
firecode backend's own door probe and run-record keep issuing calls. Count the command
TEXT instead — the tree test filters `find `, the page test filters `for f in` and
`base64 -w0 <` (the old per-file shape, which must be 0).

## Rake 3 — a page that breaks on BYTES must not print its progress line twice

The progress line (`scanned i of N`) sits at the top of the file loop, and a page that
stops growing at `READ_PAGE_BYTES` leaves `i` on a file the NEXT page then starts with —
so the same index fired the line twice. Test the byte bound *before* emitting progress.
Harmless in output, but the claim was "identical", and this was the one place it was not.

## The same shape elsewhere (audited)

- `glob` — walks, reads nothing. No defect.
- `list_tree` — one command on firecode and over ssh; its default is the per-directory
  loop, which is the same defect and was already closed by that seam.
- `outline`'s tree mode — **fixed on this branch**, same seam, same page size. Its page
  is chosen by the LANGUAGE (known before any read), and that loop never exits early, so
  a page boundary cannot change an answer.
- `merge_gate` — ~a dozen fixed names, not a walk.
- `read`/`edit`/`write`/`lsp`/`intent::write_plan`, `runtime.rs`'s second-gate re-read,
  `harnessd::publish_citations` — one named file per call by definition.
- **`crates/tools/src/remote.rs`'s `read` still pays one ssh round trip per file** (it
  inherits the default) — left deliberately on this branch. `batched_read_command` and
  `parse_batched_reads` are `pub(crate)` in `firecode.rs` precisely so that is a small
  follow-up.

## And the host cost, so nobody re-measures it

Paging is not slower where the round trip is a syscall: one `grep` tool call over a copy
of this tree's `crates/` (734 `.rs` files) was 21.7 / 21.9 ms before and 20.9 / 20.9 ms
after, same answer. The win is entirely the round trip.
