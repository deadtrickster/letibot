<!-- abstract: Four sentences a refused mux session prints arrive as exit 255 with nothing on stdout and are a REFUSAL, not a drop, so a retry is safe; and three ways a fake ssh that counts sessions lies. -->

# A refused ssh session, three ways a fixture's ledger lies, and where the 0-byte file came from

`mux_client_request_session: session request failed`,
`mm_send_fd: sendmsg(1): Message too long`,
`channel 0: open failed: administratively prohibited` and
`Control socket connect ...: Connection refused` arrive alike: exit 255, nothing on stdout. They
are not a drop. `mm_send_fd` failing means the session's descriptors never reached the master,
so the remote shell never started, nothing ran, and a retry is legitimate for any operation,
including a `run`. `MuxRefusal` in `crates/tools/src/remote.rs` (`:979`) is that classification.

The 0-byte file came from the old chunked write in `Ssh::write_abs` (`remote.rs:2003`), which
dropped each chunk's exit status and ran an unconditional `mv`; that subject is now
`a-write-through-the-git-placement-can-leave-a-file-empty.md`.

Three ways a fake ssh that counts sessions lies: `exec` skips the `EXIT` trap, so a fixture
ending in `exec bash -lc "$last"` never removes its live-sessions file (run as a child, then
`exit $?`); a `mkdir` lock is not a lock, because a releaser's `rmdir` can land after the next
holder's `mkdir` (40 callers finished at 32); and `'$$'` in single quotes is literal, so every
session writes one file.
