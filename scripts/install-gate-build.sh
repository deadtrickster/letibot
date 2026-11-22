#!/bin/sh
# Install what the gate built into the directory the launcher starts from.
#
#     sh scripts/install-gate-build.sh
#
# The last step of the merge gate, after `cargo build --release --bins`. No arguments: both
# directories are facts about the checkout.
#
# The gate builds in `<worktree>/target` (`GATE_TARGET_DIR`), while `scripts/letibot` execs from
# the main tree's `target/release`. Without this step a green landing leaves the daemon on an
# old binary.
#
# It copies the artifact the gate judged rather than building main: the gate runs before the
# fast-forward. If the push fails after a green gate, this directory holds an unlanded build
# until the next landing replaces it.
#
# The names are read from the built tree's `install.sh` (`BINARIES=`), never restated.
# Copy beside, then rename: `cp` over a running executable fails with `Text file busy`, and a
# rename keeps running processes on their inode. `$$` in the temp name keeps two gates apart.
set -eu

# Does this binary link llama.cpp? `otool -L` on a Mac, `readelf` on Linux (which is what the
# gate runs on), and `ldd` for a box with no binutils.
links_llama() {
  if command -v otool >/dev/null 2>&1; then
    otool -L "$1" 2>/dev/null | grep -q libllama
  elif command -v readelf >/dev/null 2>&1; then
    readelf -d "$1" 2>/dev/null | grep -q 'NEEDED.*libllama'
  else
    ldd "$1" 2>/dev/null | grep -q libllama
  fi
}

here=$(pwd -P)

# The tree that was built — the gate's own worktree, from the step's working directory.
tree=$(git -C "$here" rev-parse --show-toplevel) || {
  echo "install-gate-build: $here is not inside a git checkout, so neither the tree that was" >&2
  echo "  built nor the tree the launcher starts from can be named." >&2
  exit 1
}

# The main tree is the linked worktree's common git dir's parent. A common dir that is not
# `<tree>/.git` is a bare repository with no `target/release`, so that is a refusal.
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

# `CARGO_TARGET_DIR` is set for every gate step. Unset means a by-hand run, so the tree's own
# `target`; run from the main tree that IS the directory installed into, and the two checks below
# refuse rather than copy every binary onto itself and call it an install.
target=${CARGO_TARGET_DIR:-$tree/target}
case "$target" in
  /*) ;;
  *) target=$here/$target ;;
esac
built=$target/release
if [ ! -d "$built" ]; then
  echo "install-gate-build: nothing was built at $built." >&2
  echo "  The step before this one is \`cargo build --release --bins --features local\`, and it" >&2
  echo "  writes into \$CARGO_TARGET_DIR/release. A gate whose install step runs without it has" >&2
  echo "  nothing to install, and installing nothing quietly is the defect this script exists for." >&2
  exit 1
fi

# One directory under two spellings is one directory, and `-nt` cannot tell a copy from a rename.
same_dir() {
  [ "$(cd "$1" 2>/dev/null && pwd -P)" = "$(cd "$2" 2>/dev/null && pwd -P)" ]
}

# **A gate whose install step was not told where the build went installs nothing.** Run by hand in
# the main tree, `built` is `dest`, so every `cp` is a binary onto itself and the last line still
# says installed. Refused, with the variable that names the real build.
if same_dir "$built" "$dest"; then
  echo "install-gate-build: $built and $dest are one directory, so this copies every binary onto" >&2
  echo "  itself and installs nothing: pass CARGO_TARGET_DIR=<the directory the build went to>." >&2
  exit 1
fi

# The newest `harnessd` this step can see, over the build directories it knows: the tree it was
# built in, that tree's own `target`, and the one the launcher starts from. Installing an older
# build than the one already there is how a green landing leaves the box on the old binary.
newest=""
for dir in "$built" "$tree/target/release" "$dest"; do
  [ -f "$dir/harnessd" ] || continue
  if [ -z "$newest" ] || [ "$dir/harnessd" -nt "$newest/harnessd" ]; then newest=$dir; fi
done
if [ -n "$newest" ] && ! same_dir "$newest" "$built"; then
  echo "install-gate-build: the newest harnessd here is $newest/harnessd, and the build this step" >&2
  echo "  would install ($built/harnessd) is older, so the installed daemon would go backwards." >&2
  echo "  Pass CARGO_TARGET_DIR=<the directory the build went to> if that is the build you mean." >&2
  exit 1
fi

bins=$(sed -n 's/^BINARIES="\(.*\)"$/\1/p' "$tree/install.sh" | head -1)
if [ -z "$bins" ]; then
  echo "install-gate-build: $tree/install.sh has no \`BINARIES=\` line to read the names from." >&2
  echo "  That line is what an install copies; refusing rather than installing nothing." >&2
  exit 1
fi

# Names that are repository scripts, not build products (`LAUNCHERS=` in install.sh). This
# step installs what the gate built, so a launcher is not its business.
launchers=$(sed -n 's/^LAUNCHERS="\(.*\)"$/\1/p' "$tree/install.sh" | head -1)

# The two the launcher execs are not optional, whatever the list says: a `BINARIES=` that
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
# **A cloud-only daemon must not silently replace a local one.** A plain `cargo build` is
# cloud-only now, so a gate step that lost its `--features local` would install a daemon with no
# llama.cpp over a box that serves local models, and nothing would say so until a session asked
# for one. Refused here, where the two binaries are both in hand.
if [ -x "$dest/harnessd" ] && [ -x "$built/harnessd" ] &&
  [ -z "${LETIBOT_ALLOW_CLOUD_DAEMON:-}" ] &&
  links_llama "$dest/harnessd" && ! links_llama "$built/harnessd"; then
  echo "install-gate-build: $built/harnessd links no llama.cpp, and the installed" >&2
  echo "  $dest/harnessd does. This is the box's local-model daemon, so installing this" >&2
  echo "  build would take local models away from every session here." >&2
  echo "  The gate's build step is \`cargo build --release --bins --features local\` for that" >&2
  echo "  reason (AGENTS.md, Merge gate). To install a cloud-only daemon on purpose:" >&2
  echo "    LETIBOT_ALLOW_CLOUD_DAEMON=1 sh scripts/install-gate-build.sh" >&2
  exit 1
fi
installed=""
skipped=""
for name in $bins; do
  src=$built/$name
  if [ ! -x "$src" ]; then
    # A launcher is not a build product. Said per name, so a `BINARIES=` entry missing from
    # `LAUNCHERS` still fails below.
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

# The role names of `letibot-multi` (install.sh's ROLE_LINKS) are made here, as an install makes
# them: `harnessd` looks for `letibot-askpass` beside itself.
roles=$(sed -n 's/^ROLE_LINKS="\(.*\)"$/\1/p' "$tree/install.sh" | head -1)
for name in $roles; do
  [ -x "$dest/letibot-multi" ] || break
  ln -f "$dest/letibot-multi" "$dest/$name.new.$$" 2>/dev/null || cp -p "$dest/letibot-multi" "$dest/$name.new.$$"
  mv -f "$dest/$name.new.$$" "$dest/$name"
  installed="$installed $name"
done

echo "install-gate-build: proved    $built"
if [ -n "$skipped" ]; then
  echo "install-gate-build: launchers $skipped are scripts in the tree, not build products"
fi
echo "install-gate-build: built from $tree at $(git -C "$tree" rev-parse --short HEAD)"

# Run the two the launcher execs. `--version` needs no socket, store or model, and shows a
# binary is in place, not just copied. `letibot-askpass` would prompt for a password, and the
# multicall's version is the dispatcher's, so neither is probed.
"$dest/harnessd" --version
"$dest/letibot-tui" --version

echo "install-gate-build: installed$installed into $dest"
echo "install-gate-build: the launcher (~/bin/letibot -> scripts/letibot) execs $dest/harnessd"
echo "install-gate-build: and $dest/letibot-tui — a daemon ALREADY RUNNING keeps the old inode until"
echo "install-gate-build: it is restarted ('letibot --status' says so; 'letibot --stop && letibot' picks this up)"
