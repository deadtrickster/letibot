#!/bin/sh
# shellcheck disable=SC2086 # the name lists are space-separated words, split on purpose
# Every triple the release workflow builds must be one install.sh asks for, and the archive
# must hold the files install.sh copies. A mismatch 404s the download and every install
# silently falls back to a source build.

set -eu

# shellcheck disable=SC1007 # an empty CDPATH for this cd only, so it cannot print a path
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# `--built [DIR]`: also check a build's output (default `$CARGO_TARGET_DIR/release`).
built_dir=""
case "${1:-}" in
  --built) built_dir="${2:-${CARGO_TARGET_DIR:-$repo/target}/release}" ;;
  "") ;;
  *)
    echo "usage: check-dist-names.sh [--built [DIR]]" >&2
    exit 2
    ;;
esac

# What install.sh derives from `uname`, read from its `triple=<name>` assignments: the
# assignment is the contract, a triple's shape is not (`*-apple-darwin` has no `-unknown-`).
known=$(grep -oE 'triple=[A-Za-z0-9_.-]+' "$repo/install.sh" | sed 's/^triple=//' | sort -u)
if [ -z "$known" ]; then
  echo "check-dist-names: found no triples in install.sh — did the layout change?" >&2
  exit 1
fi

# What the workflows build. The matrix lists them literally, as `triple: <name>`,
# so this does not have to understand YAML. An interpolated value cannot be read, so it is
# refused rather than dropped.
values=$(grep -h 'triple:' "$repo"/.github/workflows/*.yml 2>/dev/null |
  sed 's/.*triple:[[:space:]]*//' |
  grep -v '^$' || true)
interpolated=$(printf '%s\n' "$values" | grep '[$]' || true)
if [ -n "$interpolated" ]; then
  echo "check-dist-names: a workflow names a triple this check cannot read:" >&2
  printf '  triple: %s\n' "$interpolated" >&2
  echo "  List each triple literally in the matrix." >&2
  exit 1
fi
built=$(printf '%s\n' "$values" | grep -v '^$' | sort -u || true)
if [ -z "$built" ]; then
  echo "check-dist-names: no triples found in the workflows" >&2
  exit 1
fi

bad=""
for t in $built; do
  case " $(printf '%s ' $known) " in
    *" $t "*) ;;
    *) bad="$bad $t" ;;
  esac
done
if [ -n "$bad" ]; then
  echo "check-dist-names: the workflow builds triples install.sh never asks for:$bad" >&2
  echo "  Every such asset is published and downloaded by nobody." >&2
  exit 1
fi

# A triple install.sh knows and we do not ship: reported, not fatal.
gap=""
for t in $known; do
  case " $(printf '%s ' $built) " in
    *" $t "*) ;;
    *) gap="$gap $t" ;;
  esac
done
if [ -n "$gap" ]; then
  echo "check-dist-names: install.sh knows triples we do not publish:$gap" >&2
  echo "  Those installs fall back to a source build, which is by design." >&2
fi

# The installer is served from `main` and the archive from `releases/latest`, two refs. So
# install.sh's BINARIES + LIBRARIES and make-dist.sh's ARCHIVE are read from their files and
# compared as sets, both ways. A copy of either list here would be a third place to drift.
# `LIBRARIES` is the local archive's set: a cloud archive carries none of them.
install_files=""
for var in BINARIES LIBRARIES; do
  v=$(sed -n "s/^$var=\"\(.*\)\"$/\1/p" "$repo/install.sh" | head -1)
  [ -n "$v" ] || {
    echo "check-dist-names: install.sh has no $var line." >&2
    exit 1
  }
  install_files="$install_files $v"
done
packaged=$(sed -n 's/^ARCHIVE_BINARIES="\(.*\)"$/\1/p;s/^ARCHIVE_LIBRARIES="\(.*\)"$/\1/p' \
  "$repo/scripts/make-dist.sh")
[ -n "$packaged" ] || {
  echo "check-dist-names: make-dist.sh has no ARCHIVE_* declaration." >&2
  exit 1
}

missing_from_archive=""   # install.sh wants it; the packager does not make it
missing_from_installer="" # the packager makes it; install.sh does not copy it
for f in $install_files; do
  case " $(printf '%s ' $packaged) " in
    *" $f "*) ;;
    *) missing_from_archive="$missing_from_archive $f" ;;
  esac
done
for f in $packaged; do
  case " $(printf '%s ' $install_files) " in
    *" $f "*) ;;
    *) missing_from_installer="$missing_from_installer $f" ;;
  esac
done
if [ -n "$missing_from_archive" ]; then
  echo "check-dist-names: install.sh copies files the packager does not build:$missing_from_archive" >&2
  echo "  Those installs fail at copy time, or worse: the one-liner is served from" >&2
  echo "  main while the asset comes from releases/latest, so a user gets the" >&2
  echo "  mismatch with no tag between them. Add them to ARCHIVE_* in make-dist.sh" >&2
  echo "  (and to the release's asset if the archive is already published)." >&2
  exit 1
fi
if [ -n "$missing_from_installer" ]; then
  echo "check-dist-names: the packager builds files install.sh never copies:$missing_from_installer" >&2
  echo "  They ship in the archive and are not installed, which is a size cost and," >&2
  echo "  for a binary, a file nothing runs." >&2
  exit 1
fi

# The macOS list, held to the same rule. install.sh's LIBRARIES_DARWIN is what a Mac
# copies and make-dist.sh's ARCHIVE_LIBRARIES_DARWIN is what an `*-apple-darwin` archive
# holds; the binaries are shared with the Linux lists above.
inst_darwin=$(sed -n 's/^LIBRARIES_DARWIN="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
pack_darwin=$(sed -n 's/^ARCHIVE_LIBRARIES_DARWIN="\(.*\)"$/\1/p' "$repo/scripts/make-dist.sh" | head -1)
if [ -z "$inst_darwin" ] || [ -z "$pack_darwin" ]; then
  echo "check-dist-names: install.sh's LIBRARIES_DARWIN or make-dist.sh's" >&2
  echo "  ARCHIVE_LIBRARIES_DARWIN is missing, so the macOS asset has no list to agree with." >&2
  exit 1
fi
if [ "$(printf '%s\n' $inst_darwin | sort)" != "$(printf '%s\n' $pack_darwin | sort)" ]; then
  echo "check-dist-names: the macOS library lists disagree:" >&2
  echo "  install.sh  LIBRARIES_DARWIN:         $inst_darwin" >&2
  echo "  make-dist   ARCHIVE_LIBRARIES_DARWIN: $pack_darwin" >&2
  exit 1
fi

# **The two asset NAMES, which is the other half of the same contract.** install.sh asks for
# `letibot-<triple>.tar.gz` by default and `letibot-<triple>-local.tar.gz` when asked for a local
# install; make-dist.sh produces exactly those. The suffix is a constant in both files, so it is
# read from both rather than restated here.
inst_suffix=$(sed -n 's/^ASSET_SUFFIX_LOCAL="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
pack_suffix=$(sed -n 's/^ASSET_SUFFIX_LOCAL="\(.*\)"$/\1/p' "$repo/scripts/make-dist.sh" | head -1)
if [ -z "$inst_suffix" ] || [ -z "$pack_suffix" ]; then
  echo "check-dist-names: install.sh or make-dist.sh has no ASSET_SUFFIX_LOCAL line." >&2
  echo "  That constant is the whole of which asset is the default and which carries llama." >&2
  exit 1
fi
if [ "$inst_suffix" != "$pack_suffix" ]; then
  echo "check-dist-names: the two asset names disagree:" >&2
  echo "  install.sh  ASSET_SUFFIX_LOCAL:  $inst_suffix" >&2
  echo "  make-dist   ASSET_SUFFIX_LOCAL:  $pack_suffix" >&2
  exit 1
fi
# And each file must still build both names from that suffix: the default one and the local one.
# shellcheck disable=SC2016 # the `$1` and `$triple` are the files' own text, matched literally
if ! grep -qF 'letibot-$1.tar.gz' "$repo/install.sh"; then
  echo "check-dist-names: install.sh no longer asks for letibot-<triple>.tar.gz by default." >&2
  exit 1
fi
# shellcheck disable=SC2016 # the same
if ! grep -qF 'letibot-$triple$ASSET_SUFFIX_LOCAL.tar.gz' "$repo/scripts/make-dist.sh"; then
  echo "check-dist-names: make-dist.sh no longer produces letibot-<triple>$ASSET_SUFFIX_LOCAL.tar.gz." >&2
  exit 1
fi

# Every workflow that publishes must build BOTH assets: the cloud one under the default name and
# the local one beside it. One without the other is an install that 404s or a local model nobody
# can install prebuilt, and neither shows up until a user tries.
for want in --cloud --local; do
  if ! grep -q -- "make-dist.sh.*$want" "$repo"/.github/workflows/*.yml 2>/dev/null; then
    echo "check-dist-names: no workflow runs make-dist.sh $want, so only one asset is published." >&2
    exit 1
  fi
done

# A role link is made at install time, never shipped, and never a launcher's name.
launchers=$(sed -n 's/^LAUNCHERS="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
roles=$(sed -n 's/^ROLE_LINKS="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
for r in $roles; do
  case " $launchers $packaged " in
    *" $r "*)
      echo "check-dist-names: $r is in ROLE_LINKS and is also a launcher or a packaged file." >&2
      echo "  install.sh makes it from letibot-multi, so nothing may ship under that name." >&2
      exit 1
      ;;
  esac
done

# No compiled binary under a launcher's name in a build's output. An install loop that copies
# `target/release/*` would put it over the command a person types, stale or not.
if [ -n "$built_dir" ]; then
  [ -x "$built_dir/letibot-multi" ] || {
    echo "check-dist-names: no letibot-multi in $built_dir: run cargo build --release first." >&2
    exit 1
  }
  for l in $launchers; do
    f="$built_dir/$l"
    [ -f "$f" ] || continue
    head -c 2 "$f" | grep -q '^#!' && continue
    if [ "$built_dir/letibot-multi" -nt "$f" ]; then
      echo "check-dist-names: $f is a stale build product, older than letibot-multi; delete it." >&2
      exit 1
    fi
    echo "check-dist-names: $f is a compiled binary under the launcher's name." >&2
    echo "  A bin target named \`$l\` is back; the multicall ships as letibot-multi." >&2
    exit 1
  done
  echo "check-dist-names: $built_dir holds no compiled binary under a launcher's name"
fi

echo "check-dist-names: $(printf '%s ' $built | wc -w) triple(s) built, all asked for by install.sh; install.sh and the archive agree on $(printf '%s ' $packaged | wc -w) file(s)"

# Every workflow that builds must pin the same llama.cpp, or one job links a library the
# release does not ship. Read as `LLAMA_REF:` values, so this does not parse YAML.
pins=$(grep -h '^[[:space:]]*LLAMA_REF:' "$repo"/.github/workflows/*.yml 2>/dev/null |
  sed 's/.*LLAMA_REF:[[:space:]]*//' |
  sed 's/[[:space:]]*$//' |
  grep -v '^$' |
  sort -u)
n=$(printf '%s\n' "$pins" | grep -c . || true)
if [ "$n" -eq 0 ]; then
  echo "check-dist-names: no LLAMA_REF pin found in the workflows." >&2
  echo "  The tree does not compile without a llama.cpp checkout, so every job" >&2
  echo "  must name one. Silence here means a job is building against whatever" >&2
  echo "  its clone happened to default to." >&2
  exit 1
fi
if [ "$n" -gt 1 ]; then
  echo "check-dist-names: the workflows pin different llama.cpp revisions:" >&2
  printf '  %s\n' $pins >&2
  echo "  one job would build against a library the release does not ship." >&2
  exit 1
fi
echo "check-dist-names: the workflows pin llama.cpp $pins"
