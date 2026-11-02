<!-- abstract: sqlite3 releases a file's descriptor only when the process's last connection to it closes, so the count is the high-water mark of concurrent connections and `Store` is one shared connection per path per process. -->

# A closed connection keeps its descriptor until the process's last one goes

`sqlite3`'s unix VFS keeps the descriptor it opened for a file in the inode's own record and releases it
when the process's LAST connection to that file closes, not when the connection that opened it does. So
the count is the high-water mark of CONCURRENT connections: twenty opened and closed one at a time
leave four descriptors on a WAL store, twenty at once leave forty-three while open and twenty-three
after closing them.

That is why `Store` is one connection per file per process: `Store::open` keeps a process-wide
`Mutex<HashMap<canonical path, Weak<Shared>>>` and hands out the live `Arc<Shared>`, replacing the entry
when the path names a different inode. A pool would buy concurrency, not fewer descriptors.

Two rules come with it. A `Conn` guard must not be alive across another `Store` method call - that is a
self-deadlock the compiler will not name (hence the `_on(conn, ..)` forms). And a write takes the
connection INSIDE the retry closure (`write_tx`), so a busy `BEGIN IMMEDIATE` drops the guard before the
backoff sleep; the queue's own writes did not, so up to `BUSY_RETRY_TOTAL` ran holding the process's
only connection.
