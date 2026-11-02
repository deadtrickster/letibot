<!-- abstract: Only one fact here survives its own correction: a fixture that opens a `Harness` by hand cannot see a store-lifetime bug, because a bare harness gives everything back. The rest of this file is superseded and should be removed. -->

# A store's descriptors outlive every close except the last one

This file is superseded and is a deletion candidate. Its central number was wrong: the count of
descriptors on `sessions.db` is the high-water mark of CONCURRENT connections, not a debt per
connection ever made. The corrected account, with the fix and the test, is
`a-closed-connection-keeps-its-descriptor-until-the-last-one-goes`.

The one fact worth carrying forward on its own: `Harness::open_with_registry` plus drop takes its
descriptors to 0, so a fixture that opens a bare `Harness` cannot see this class of bug at all. Every
leak the original measured needed `Sessions` underneath it.
