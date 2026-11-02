<!-- abstract: A fixture that rebinds a unix socket at a path cannot prove identity, because ext4 hands the same inode back - take the name by a rename and assert the identity differs, and remember the record's identity is `(dev, ino)` plus the ctime pair. -->

# `a_record_of_a_socket_that_is_gone_is_not_an_answer` is red on ext4, and it is not your change

The test (`crates/sessionlog/src/placement.rs:128`) failed on lab2x1 on an unmodified tree, and the
cause is the filesystem: `bind(p) → st_ino 69773837; close; unlink; bind(p) → st_ino 69773837`. ext4
hands the same inode back when the socket is closed before it is unlinked, so
`socket_identity(socket) == recorded` and a stale `host:lab2x1` record is read as the live daemon's -
the exact defect the identity check exists to prevent, arriving through the check itself.

The fix is the rule: the successor binds at another name and takes the old one by `rename`, and the
fixture asserts with `assert_ne!` that the identity really differs before it asserts `read` is `None`.
`socket_identity` returns `((dev, ino), (ctime, ctime_nsec))` - this file, and this binding of it - so
`a_rebind_that_takes_the_same_inode_back_is_not_an_answer` stages the reused-inode case with a `chmod`
instead of a bet on the allocator.

Do not "fix" such a test by asserting `Some(...)`: the refusal is the property, the fixture is what
cannot make its premise true on this box.
