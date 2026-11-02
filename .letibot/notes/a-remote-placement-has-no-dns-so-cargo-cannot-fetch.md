<!-- abstract: A re-pin cannot build in a remote placement because `~/.cargo` is mounted read-only, so a rev in the object store still cannot be checked out; a session-local `CARGO_HOME` with the read-only halves symlinked is the fix. -->

# A remote placement's shell has no DNS, so a new rev cannot be fetched

The boundary's `/etc` has no resolver configuration - no `resolv.conf`, no `hosts` - so `cargo fetch` of a new git rev fails with `failed to resolve address for github.com` while `ip addr` shows the link up.

That is fatal for a re-pin, and the first wall is not the last. A `git` + `rev` dependency resolves from `$CARGO_HOME/git/db`, so `git -C <cargo home>/git/db/<name>-* cat-file -t <rev>` is the one command that says whether a build against that rev is possible. Then, with the rev present, the checkout still fails, because `~/.cargo` is mounted read-only in the placement and cargo wants to make `git/checkouts/<rev>`.

The shape that works is a session-local `CARGO_HOME` with the read-only halves symlinked:

```sh
mkdir -p $HOME/cargo-home/git/checkouts
ln -sfn /home/dead/.cargo/registry $HOME/cargo-home/registry
ln -sfn /home/dead/.cargo/git/db   $HOME/cargo-home/git/db
export CARGO_HOME=$HOME/cargo-home
```

`cargo metadata --offline` then checks the rev out from the local db with no network. Every command in the session needs that export beside `CARGO_TARGET_DIR`, or the next one re-resolves against the read-only home and fails again.
