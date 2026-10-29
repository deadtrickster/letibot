#!/bin/sh
# Install what the gate BUILT into the directory the LAUNCHER starts from.
#
#     sh scripts/install-gate-build.sh
#
# Run as the LAST step of letibot's merge gate, after `cargo build --release --bins`. It takes
# no arguments and reads no flags: the two directories it moves between are facts about the
# checkout, and a step that could be pointed elsewhere by the gate's own line is a step that
# can be pointed at the wrong tree by a typo.
#
# # THE TWO PATHS, because this script exists for the gap between them
#
#   THE GATE BUILDS in a PRIVATE target directory — `<worktree>/target`, which the merge
#   queue sets for every step through `CARGO_TARGET_DIR` (`crates/harnessd/src/mergequeue.rs`,
#   `GATE_TARGET_DIR`). That is right, and it is what stops the gate judging a branch with
#   artifacts built from another tree's sources. Its consequence was written down nowhere:
#   `cargo build --release --bins` in the gate leaves the MAIN tree's `target/release`
#   exactly as it was.
#
#   THE LAUNCHER STARTS FROM THE MAIN TREE'S `target/release`. `~/bin/letibot` is a symlink
#   to `scripts/letibot`, which resolves its own location through symlinks, takes the first
#   candidate that holds `harnessd` (beside the launcher, then `<launcher dir>/../target/
#   release`) and execs `$BIN/harnessd` and `$BIN/letibot-tui` from there. Nothing on the box
#   looks at `$CARGO_TARGET_DIR`.
#
# So the gate could be green on every step of every landing and the daemon the operator
# restarts would still be running a binary from hours ago. MEASURED 2026-10-10: a daemon came
# up at 20:55 on code from 17:44 — three hours and seventeen landings behind — with no
# per-workspace queue and no queue views in it, and restarting could not fix it, because the
# file it execs had never been rebuilt. This script is the step that closes it.
#
# # Why this COPIES the gate's artifact instead of building the main tree
#
# The other shape — a step that runs `cargo build --release --bins` in the main tree, with the
# private dir kept for the verification steps — installs a tree the gate did NOT judge: the
# gate runs BEFORE the fast-forward, so at that moment `main` does not yet carry this branch,
# and a build of main's sources would be one landing behind for ever. What is copied here is
# the artifact that passed, built from the branch rebased onto the tip of main, which is what
# `main` becomes one step later.
#
# The hazard is named rather than hidden: if the fast-forward or the push fails after a green
# gate, this directory holds a branch's build that never became main. That is a wrong binary
# rather than a corrupt one, and the next landing's install replaces it.
#
# # The names come from `install.sh`
#
# `BINARIES=` there is the list an install copies, and `scripts/check-dist-names.sh` already
# holds it to `make-dist.sh`'s `ARCHIVE_BINARIES`. Read out of the tree that was BUILT rather
# than restated here, because a second copy of a list drifts (see `agents.md`) and a binary
# added to the installer is one the launcher's directory has to hold too.
#
# # Why the copy is a copy-beside-and-rename
#
# A daemon is normally RUNNING from `<main>/target/release/harnessd` while the gate runs.
# MEASURED on this box: `cp` over a running executable fails with `Text file busy`, so a plain
# `cp` here would make this step red on every box that has a daemon up — which is the box it
# exists for. A rename replaces the directory entry atomically instead: a process that is
# already running keeps the inode it started from, and a launcher that execs the path in
# between gets either the old binary or the new one, never a half-written file. The temporary
# name carries `$$` so two gates over one repository (two daemons, two worktrees) cannot
# interleave into one file.
set -eu

here=$(pwd -P)

# The tree that was built — the gate's own worktree, from the step's working directory.
tree=$(git -C "$here" rev-parse --show-toplevel) || {
    echo "install-gate-build: $here is not inside a git checkout, so neither the tree that was" >&2
    echo "  built nor the tree the launcher starts from can be named." >&2
    exit 1
}

# **The main tree, which is not this one.** The gate runs in a LINKED worktree, whose common
# git directory is the main checkout's `.git` — the same reader `repo_of_worktree` uses. A
# common dir that is not `<tree>/.git` is a bare repository, and a bare repository has no
# `target/release` for a launcher to start from, so that is a refusal by name rather than an
# install into a directory nobody looks at.
common=$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)
case "$common" in
    */.git) main=${common%/.git} ;;
    *)
        echo "install-gate-build: the common git directory is $common, which is not <tree>/.git." >&2
        echo "  A bare repository has no working tree and no target/release, so there is nowhere" >&2
        echo "  for the launcher to start this build from." >&2
        exit 1
        ;;
esac
dest=$main/target/release

# What the gate's build step wrote. `CARGO_TARGET_DIR` is the queue's own variable and is set
# for every step; unset means a person ran this by hand, and then the tree's own `target` is
# where the last build went — which is `dest` itself when they ran it in the main tree, and a
# copy of a file onto its own name is a no-op rather than an error.
target=${CARGO_TARGET_DIR:-$tree/target}
case "$target" in
    /*) ;;
    *) target=$here/$target ;;
esac
built=$target/release
if [ ! -d "$built" ]; then
    echo "install-gate-build: nothing was built at $built." >&2
    echo "  The step before this one is \`cargo build --release --bins\`, and it writes into" >&2
    echo "  \$CARGO_TARGET_DIR/release. A gate whose install step runs without it has nothing" >&2
    echo "  to install, and installing nothing quietly is the defect this script exists for." >&2
    exit 1
fi

bins=$(sed -n 's/^BINARIES="\(.*\)"$/\1/p' "$tree/install.sh" | head -1)
if [ -z "$bins" ]; then
    echo "install-gate-build: $tree/install.sh has no \`BINARIES=\` line to read the names from." >&2
    echo "  That line is what an install copies; refusing rather than installing nothing." >&2
    exit 1
fi

# **The names in that list that are REPOSITORY SCRIPTS, not build products.** `install.sh`
# declares them separately (`LAUNCHERS="letibot leticode"`) and copies them out of `scripts/`,
# because they are sh scripts beside the binaries — `leticode` is `scripts/leticode`, which execs
# `letibot`. This step installs what the gate BUILT, so a launcher is not its business.
#
# MEASURED 2026-10-29, the first landing whose push succeeded (so the first time this step ran at
# all): every earlier landing failed its push and the queue skipped the install, and this step
# then died on the last name — `leticode is not an executable file — the build step did not
# produce leticode in target/release` — after copying the four real binaries. The landing itself
# was fine and pushed; only the install was lost, which is the defect this script exists against.
launchers=$(sed -n 's/^LAUNCHERS="\(.*\)"$/\1/p' "$tree/install.sh" | head -1)

# **The two the launcher execs are not optional**, whatever the list says: a `BINARIES=` that
# had lost one would install a directory the launcher cannot start, silently.
for name in harnessd letibot-tui; do
    case " $bins " in
        *" $name "*) ;;
        *)
            echo "install-gate-build: $name is not in install.sh's BINARIES, and the launcher" >&2
            echo "  execs it — this install cannot be complete." >&2
            exit 1
            ;;
    esac
done

mkdir -p "$dest"
installed=""
skipped=""
for name in $bins; do
    src=$built/$name
    if [ ! -x "$src" ]; then
        # **A launcher is not a build product**: `install.sh` copies it out of `scripts/`, and the
        # launcher's own resolution (`scripts/letibot`) never looks for it beside `harnessd`. Said
        # once per name rather than passed over, so a `BINARIES=` that gained a script nobody
        # listed in `LAUNCHERS` still fails below instead of vanishing.
        case " $launchers " in
            *" $name "*)
                echo "install-gate-build: $name is a repository script (install.sh's LAUNCHERS)," \
                     "not a build product — left to \`scripts/$name\`" >&2
                skipped="$skipped $name"
                continue
                ;;
        esac
        echo "install-gate-build: $src is not an executable file — the build step did not" >&2
        echo "  produce \`$name\` in $built." >&2
        exit 1
    fi
    # `-p` keeps the mode and the mtime. The mtime is the honest one for the launcher's own
    # staleness line (`daemon_stale_line` compares it with the daemon's start): when the code
    # was BUILT, not when it was copied into place.
    cp -p "$src" "$dest/$name.new.$$"
    mv -f "$dest/$name.new.$$" "$dest/$name"
    installed="$installed $name"
done

echo "install-gate-build: proved    $built"
if [ -n "$skipped" ]; then
    echo "install-gate-build: launchers $skipped are scripts in the tree, not build products"
fi
echo "install-gate-build: built from $tree at $(git -C "$tree" rev-parse --short HEAD)"

# **Run the two the launcher execs, from where they now are.** `--version` needs no socket, no
# store and no model — it is the same probe `install.sh` and CI's smoke step use — and it is
# the difference between *a file was copied* and *a binary is in place*. The other two names
# are NOT probed: `letibot-askpass`'s first argument is sudo's own prompt, so running it is
# asking somebody for a password, and the multicall's `--version` would be about the
# dispatcher rather than about what the launcher starts.
"$dest/harnessd" --version
"$dest/letibot-tui" --version

echo "install-gate-build: installed$installed into $dest"
echo "install-gate-build: the launcher (~/bin/letibot -> scripts/letibot) execs $dest/harnessd"
echo "install-gate-build: and $dest/letibot-tui — a daemon ALREADY RUNNING keeps the old inode until"
echo "install-gate-build: it is restarted ('letibot --status' says so; 'letibot --stop && letibot' picks this up)"
