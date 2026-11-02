<!-- abstract: `ETXTBSY` is a fact about the INODE, so "write a script, chmod it, exec it" races every other fork in the same test binary - the child holds the write descriptor until its own exec - and the tree's answer is `write_stub`. -->

# `ETXTBSY` is a fact about the INODE, so "write it, chmod it, exec it" races every fork in the binary

`execve` answers `ETXTBSY` for a file ANY process holds open for writing, and it is a property of the
inode, so a temp name renamed into place does not help. Retrying is the only answer.

The holder is usually a fork of the test process itself: a child forked while the fixture's write
descriptor was open inherits it, Rust's own descriptors are `O_CLOEXEC` so the child drops it at
`exec`, and the window is microseconds idle and milliseconds when the run queue is full. That is why
the test passes alone and fails in the gate, and why no sleep fixes it.

`letibot_tools::testing::write_stub` (`crates/tools/src/testing.rs`) writes, chmods, then probes by
really exec'ing it until `io::ErrorKind::ExecutableFileBusy` stops, 100 tries at 20 ms: one successful
exec proves nobody holds a write descriptor and nobody can get one after it. It is behind the
`testing` feature, which a dev-dependency turns on for a test build, and its probe gets the stub's
EMPTY argv - so a stub whose real start must block has to answer `[ $# -eq 0 ]` differently.
