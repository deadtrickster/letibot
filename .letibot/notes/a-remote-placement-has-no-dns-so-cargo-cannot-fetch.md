<!-- abstract: `~/.cargo` is read-only in a placement: a git rev is checked out from `git/db` when the halves are symlinked, but a REGISTRY DOWNLOAD needs a copy of `registry/index` + `registry/cache`, because cargo writes the `.crate` into the cache and a symlink to a read-only dir stays read-only. -->

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

## A download and a checkout need different shapes (MEASURED 2026-11-05, wt-z-t10 on lubuntu1)

The symlink shape above works for a GIT CHECKOUT, where cargo writes only `git/checkouts/<rev>`. A
REGISTRY DOWNLOAD is different and the symlink fails: cargo writes the `.crate` INTO
`registry/cache`, so the link is as read-only as its target.

`cargo test -p letibot-harnessd --lib` on this worktree died before compiling anything, on a crate
that is macOS-only and merely in `Cargo.lock`:

```
warning: failed to write cache, path: .../registry/index/index.crates.io-.../.cache/ac/ce/accelerate-src, error: Read-only file system (os error 30)
error: failed to open `.../registry/cache/index.crates.io-.../accelerate-src-0.3.2.crate`
```

The index `.cache` write comes first, so even a crate that IS in the cache can be re-fetched. Light
crates build fine and only the first heavy `-p` shows it, which reads like that crate being broken.

The shape that works is a copy (186 MB, a few seconds):

```sh
mkdir -p /tmp/cargo-home/registry
cp -a /home/dead/.cargo/registry/index /tmp/cargo-home/registry/index
cp -a /home/dead/.cargo/registry/cache /tmp/cargo-home/registry/cache
cp -a /home/dead/.cargo/git       /tmp/cargo-home/git
export CARGO_HOME=/tmp/cargo-home
```

It needs `/tmp` that survives between calls, which the default confined view does NOT give (see
`scratch-space-in-this-worktree-is-not-scratch`); in a session whose `/tmp` persists it does.
