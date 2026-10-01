#!/bin/sh
# **Can `install.sh`'s host-runtime check actually FAIL?**
#
#     sh scripts/check-host-runtime.sh
#
# The check it exercises refuses an install when the machine lacks a library the
# binaries need at run time (`libstdc++.so.6`, `libgomp.so.1`, `libsqlite3.so.0` —
# see `HOST_LIBS` in `install.sh`). A guard of that shape has one obvious way to be
# worthless: if it can only pass. On the machine this was written on, all three
# libraries are present, so the check's happy path is the ONLY path that machine
# can exercise — which is exactly the trap that let a broken three-library package
# pass every test here and die on a user's box.
#
# So this does not test the host. **It lies to `install.sh` about the host** with a
# fake `ldconfig` that is first on PATH, and then asserts what the install does:
#
#   A. a host with none of the three        -> refuses, and NAMES all three
#   B. a host with only libgomp.so.1        -> refuses, and names the OTHER TWO
#   C. a host with all three (no curl)      -> the host check does NOT fire
#
# Case B is the one worth having: it proves the check asks about each library
# separately rather than reporting the first miss and stopping, which is a real
# difference in a message a person is meant to paste.
#
# Case C proves the check is not a refusal-everything: with a complete host the
# install proceeds and fails LATER, for its own reason. The assertion is that the
# later reason is what appears and the host refusal is not.
#
# # What this does NOT cover, said rather than left to be discovered
#
# The directory fallback in `host_has_lib` — the branch taken when there is no
# `ldconfig` at all, which is unusual on glibc and normal on musl. Covering it needs
# the standard library directories to be empty of these libraries, which cannot be
# arranged on a box that has them without a namespace the test does not use. The
# fallback's FOUND branch is exercised by every real run on a glibc box; its
# not-found branch is four lines that return 1 after searching. `have_glibc` is the
# check that answers musl, and it is exercised in the same run.
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
install="$repo/install.sh"

[ -f "$install" ] || { echo "check-host-runtime: no install.sh at $install" >&2; exit 1; }

# A PATH with the POSIX essentials and a fake ldconfig, and deliberately WITHOUT
# curl/git/tar — so a run that gets past the host check fails at the next thing it
# reaches rather than downloading anything. `--help` says it cannot help: this is sh.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# The essentials install.sh reaches for before a refusal. Whatever exists here is
# linked; a missing one is simply absent from the test PATH, which is fine — the
# host check runs first and the cases that get past it are meant to fail early.
#
# **Every tool install.sh reaches before the refusal under test must be here**, or
# the case fails for the wrong reason and the test lies. `mktemp` was missing in the
# first version of this script and case C died at `mktemp: not found` instead of
# reaching the curl refusal — a test that fails for its own reason teaches nothing
# about the code, and this list is the fix for that class rather than that instance.
for t in sh dash sed grep awk cat printf dirname pwd rm mkdir chmod cp sort tr wc ls id \
         mktemp uname head cut ln touch test expr env; do
    p=$(command -v "$t" 2>/dev/null) || continue
    ln -sf "$p" "$work/$t"
done

# `ldconfig -p` prints the loader's cache: one `libname.so => /path` per line.
# The fake prints exactly what a host with the named libraries would print.
fake_cache() { # fake_cache [LIB...]
    for lib in "$@"; do
        printf '\t%s (%s) => /usr/lib/x86_64-linux-gnu/%s\n' \
            "$lib" "$(uname -m)" "$lib"
    done
}

write_fake_ldconfig() { # write_fake_ldconfig [LIB...]
    {
        printf '#!/bin/sh\n'
        printf 'cat <<"EOF"\n'
        fake_cache "$@"
        printf 'EOF\n'
    } > "$work/ldconfig"
    chmod 755 "$work/ldconfig"
}

# Run install.sh the way a stranger does: a copy in a directory that is not a
# checkout, so the local-checkout branch cannot fire and the host check is the first
# thing that happens. Prints the output; returns the exit status.
#
# `$1`, when given, is a directory to use IN PLACE OF the standard library
# directories — see `patched_dirs` below, and read that note before trusting a case
# that passes one.
run_install() {
    d="$work/run"
    rm -rf "$d"
    mkdir -p "$d"
    cp "$install" "$d/install.sh"
    if [ -n "${1:-}" ]; then
        patched_dirs "$d/install.sh" "$1" || return 2
    fi
    ( cd "$d" && env -i PATH="$work" HOME="$work" LETIBOT_INSTALL_DIR="$work/prefix" \
        sh ./install.sh 2>&1 )
}

# **Substitute the four standard library directories for a controlled one.**
#
# `have_glibc` and `host_has_lib`'s fallback both look in `/lib /lib64 /usr/lib
# /usr/lib64`. Those are literals, and on a box that has them there is no way to
# make them absent — so the two branches that depend on them cannot be reached by
# running the shipped file, and without this they would be an acknowledged gap.
#
# This is a PATCHED COPY, which is a weaker thing than running the real file, so it
# is fenced: the substitution must have applied or this returns non-zero, which
# fails the case rather than letting a stale patch pass silently when the literal
# in install.sh changes. The logic under test is untouched; only the paths are.
patched_dirs() { # patched_dirs FILE DIR
    _f="$1"
    _old='/lib /lib64 /usr/lib /usr/lib64'
    sed "s|$_old|$2|g" "$_f" > "$_f.patched" && mv "$_f.patched" "$_f" || return 1
    grep -qF -- "$2" "$_f" || return 1
    # And the original literal must be gone, or the substitution half-applied.
    ! grep -qF -- "$_old" "$_f" || return 1
    return 0
}

fail=0
note() { printf '%s\n' "$*"; }
bad() { printf 'check-host-runtime: FAIL — %s\n' "$*" >&2; fail=1; }

# --- A: a host with none of them ------------------------------------------------
write_fake_ldconfig   # prints nothing at all
out=$(run_install) && { bad "install.sh exited 0 on a host with none of $HOST_LIBS_FOR_TEST"; }
case "$out" in
    *"missing libraries the binaries need at run time"*) ;;
    *) bad "case A: the refusal did not appear. Output was: $out" ;;
esac
for lib in libstdc++.so.6 libgomp.so.1 libsqlite3.so.0; do
    case "$out" in
        *"$lib"*) ;;
        *) bad "case A: the refusal does not name $lib" ;;
    esac
done
note "A: a host with none of the three is refused, and all three are named"

# --- B: a host with only libgomp — the per-library check -------------------------
write_fake_ldconfig libgomp.so.1
out=$(run_install) && { bad "install.sh exited 0 on a host with only libgomp"; }
case "$out" in
    *"libstdc++.so.6"*) ;;
    *) bad "case B: libstdc++.so.6 is missing and was not named. Output was: $out" ;;
esac
case "$out" in
    *"libsqlite3.so.0"*) ;;
    *) bad "case B: libsqlite3.so.0 is missing and was not named" ;;
esac
# The library that IS present must not be reported — otherwise the message sends
# somebody to install something they already have.
case "$out" in
    *"libgomp.so.1 "*) bad "case B: libgomp.so.1 is PRESENT and was reported missing" ;;
esac
#
# There is deliberately NO assertion that the message mentions libgomp: it lists
# only what is MISSING, so naming a present library would send somebody to install
# something they already have. My first version asserted both — that libgomp is
# absent from the list and that the message mentions it — which cannot both hold,
# and a self-contradictory test fails whichever way the code behaves.
note "B: a host with only libgomp is refused, the other two are named, and libgomp is not"

# --- C: a complete host, and the check must not fire -----------------------------
write_fake_ldconfig libstdc++.so.6 libgomp.so.1 libsqlite3.so.0
out=$(run_install) && { bad "install.sh exited 0 with no curl and no checkout"; }
case "$out" in
    *"missing libraries the binaries need at run time"*)
        bad "case C: the host check fired on a host that HAS all three. Output was: $out" ;;
esac
# It must have got past the host check and reached the next refusal, which on this
# PATH is the missing downloader.
case "$out" in
    *"no curl found"*) note "C: a complete host passes the check and fails later, on curl" ;;
    *) bad "case C: expected the run to reach the missing-curl refusal. Output was: $out" ;;
esac

# --- D: no glibc at all — the musl case, and the one that gives WRONG ADVICE ------
#
# On musl the prebuilt binaries cannot run whatever is installed, so this must be
# its own sentence. Without it an Alpine user is told to `apk add libgomp`, which
# will not help them: the refusal has to say the asset is glibc and name the source
# build, not send somebody after a library that was never the problem.
#
# Reached by pointing the directory list at an empty directory, so no `ld-linux*.so.*`
# is found — see `patched_dirs` for why this is a patched copy.
glibc_absent="$work/no-glibc"
mkdir -p "$glibc_absent"
write_fake_ldconfig libc.so.6
if out=$(run_install "$glibc_absent"); then
    bad "case D: install.sh exited 0 with no glibc loader present"
else
    case "$?" in
        2) bad "case D: the patch did not apply, so this case proved nothing" ;;
    esac
    case "$out" in
        *"no glibc dynamic loader"*) ;;
        *) bad "case D: no musl refusal. Output was: $out" ;;
    esac
    # The advice must be the source build, NOT a package name.
    case "$out" in
        *"FROM_SOURCE"*) ;;
        *) bad "case D: the musl refusal does not name the source build" ;;
    esac
    case "$out" in
        *"libgomp1"*) bad "case D: the musl refusal offers a package, which cannot help on musl" ;;
    esac
    note "D: no glibc loader is its own refusal — source build named, no package offered"
fi

# --- E: no ldconfig, and the directory fallback FINDS the libraries ---------------
#
# The fallback is what runs on a glibc system without `ldconfig`. Its found branch is
# exercised by every real run, but not with ldconfig ABSENT, which is the branch
# itself — so here the libraries exist only in the controlled directory.
fallback_dir="$work/fallback-libs"
mkdir -p "$fallback_dir"
for lib in libstdc++.so.6 libgomp.so.1 libsqlite3.so.0; do
    : > "$fallback_dir/$lib"
done
# glibc's loader must still be findable, or case D's refusal fires first: the
# controlled directory needs a loader file too.
: > "$fallback_dir/ld-linux-test.so.1"
rm -f "$work/ldconfig"   # no ldconfig at all: this is the fallback
if out=$(run_install "$fallback_dir"); then
    bad "case E: install.sh exited 0 with no curl and no checkout"
else
    case "$?" in
        2) bad "case E: the patch did not apply, so this case proved nothing" ;;
    esac
    case "$out" in
        *"missing libraries the binaries need at run time"*)
            bad "case E: the fallback did not find libraries that are in its search path" ;;
    esac
    case "$out" in
        *"no curl found"*) note "E: with no ldconfig, the directory fallback finds them" ;;
        *) bad "case E: expected the run to reach the missing-curl refusal. Output was: $out" ;;
    esac
fi

# --- what is still not covered, said here rather than assumed ---------------------
#
# The fallback's NOT-FOUND branch: no ldconfig, directories searched, nothing there.
# Reaching it needs `have_glibc` to succeed and `host_has_lib` to fail from the same
# directory list, so the two would have to be patched to DIFFERENT directories — and
# at that point the case is testing a file that differs from the shipped one in two
# places, which is more patch than test. Case D covers the same failure from the
# other side (nothing found ⇒ refuse), and the shipped file is covered end to end by
# A, B and C.

[ "$fail" = 0 ] || exit 1
echo "check-host-runtime: refuses by name, per library, distinguishes musl, does not false-positive"
