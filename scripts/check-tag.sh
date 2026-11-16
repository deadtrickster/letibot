#!/bin/sh
# Does the release tag agree with the version compiled into the binaries?
#
#     sh scripts/check-tag.sh v0.1.1
#
# A tag ahead of the compiled version makes an update check offer the same release forever.
# The version is the workspace's (`[workspace.package] version`), which every crate inherits.
# The tag is `$1` so the check runs by hand; CI passes `$GITHUB_REF_NAME`.
#
# Exit 0 when they agree, 1 when they do not, 2 when the tag is not `vN.N.N`.
set -eu

# `set -u` plus an explicit test rather than `${1:?}`: the shell's own message for
# that construct is `scripts/check-tag.sh: 25: 1: usage: …`, which reads like a line
# number and a parameter name rather than like a usage error.
tag=${1:-}
if [ -z "$tag" ]; then
  echo "usage: check-tag.sh <tag>   (e.g. v0.1.1)" >&2
  exit 2
fi

# Said in this script's own words rather than in sed's, so a malformed argument
# reads as a usage error and not as "no version in Cargo.toml".
case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "check-tag: '$tag' is not shaped like a release tag (want vX.Y.Z)." >&2
    echo "  A tag decides the asset URL install.sh downloads:" >&2
    echo "  releases/latest/download/letibot-\$TRIPLE.tar.gz, and 'latest'" >&2
    echo "  ignores the shape — so a mis-shaped tag publishes something nobody" >&2
    echo "  can install and looks fine until somebody tries." >&2
    exit 2
    ;;
esac

# The FIRST `version =` in the root manifest, which is `[workspace.package]`'s by
# construction: the section is above every member table in this file, and a crate's
# own version now reads `version.workspace = true` and would not match this pattern.
manifest=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)

if [ -z "$manifest" ]; then
  echo "check-tag: no [workspace.package] version found in Cargo.toml." >&2
  echo "  Every crate inherits it; without it there is no version to agree with." >&2
  exit 1
fi

if [ "$tag" != "v$manifest" ]; then
  echo "check-tag: tag $tag does not match the workspace version $manifest." >&2
  echo "  The binaries would report $manifest, and an update check compares the" >&2
  echo "  release tag against that — so this release would offer an update to" >&2
  echo "  itself, forever. Either:" >&2
  echo "    * bump [workspace.package] version in Cargo.toml to ${tag#v}, or" >&2
  echo "    * tag v$manifest instead." >&2
  exit 1
fi

echo "check-tag: $tag matches the workspace version $manifest"
