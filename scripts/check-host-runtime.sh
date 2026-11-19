#!/bin/sh
# Proves `install.sh`'s host-runtime check can fail.
#
#     sh scripts/check-host-runtime.sh
#
# A fake `ldconfig` first on PATH lies about the host, then the install's verdict is checked:
#   A.  none of the three            -> refuses, and names SQLite alone (the cloud plan)
#   A2. none of the three, LETIBOT_LOCAL=1 -> refuses, and names all three
#   B.  only libgomp.so.1            -> refuses, naming SQLite and not the one that is there
#   C.  all three (no curl)          -> the host check does not fire
# B proves each library is asked about separately. C proves the check is not refuse-all. A and A2
# are the two prebuilt plans: the cloud asset needs one host library, the local one three.
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
install="$repo/install.sh"

[ -f "$install" ] || {
  echo "check-host-runtime: no install.sh at $install" >&2
  exit 1
}

# A PATH with the POSIX essentials and a fake ldconfig, and deliberately WITHOUT
# curl/git/tar — so a run that gets past the host check fails at the next thing it
# reaches rather than downloading anything. `--help` says it cannot help: this is sh.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# The tools install.sh reaches before a refusal. Every one must be here, or a case fails
# for the wrong reason (no `mktemp`, say) and the test lies.
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
  } >"$work/ldconfig"
  chmod 755 "$work/ldconfig"
}

# Run install.sh as a stranger does: a copy outside a checkout, so the host check comes first.
# Prints the output, returns the exit status. `$1`, when given, replaces the standard library
# directories (see `patched_dirs`).
run_install() {
  d="$work/run"
  rm -rf "$d"
  mkdir -p "$d"
  cp "$install" "$d/install.sh"
  if [ -n "${1:-}" ]; then
    patched_dirs "$d/install.sh" "$1" || return 2
  fi
  # `$PLAN_ENV` adds variables to the empty environment — LETIBOT_FROM_SOURCE and the
  # llama pair, which decide which runtime libraries this run is checked for.
  # shellcheck disable=SC2086
  (cd "$d" && env -i PATH="$work" HOME="$work" LETIBOT_INSTALL_DIR="$work/prefix" ${PLAN_ENV:-} \
    sh ./install.sh 2>&1)
}

# Substitute the four standard library directories, which are literals in install.sh, for a
# controlled one. A patched copy, so the substitution must apply or the case fails: a stale
# patch cannot pass silently.
patched_dirs() { # patched_dirs FILE DIR
  _f="$1"
  _old='/lib /lib64 /usr/lib /usr/lib64'
  sed "s|$_old|$2|g" "$_f" >"$_f.patched" && mv "$_f.patched" "$_f" || return 1
  grep -qF -- "$2" "$_f" || return 1
  # And the original literal must be gone, or the substitution half-applied.
  ! grep -qF -- "$_old" "$_f" || return 1
  return 0
}

fail=0
note() { printf '%s\n' "$*"; }
bad() {
  printf 'check-host-runtime: FAIL — %s\n' "$*" >&2
  fail=1
}

# --- A: a host with none of them ------------------------------------------------
# The default prebuilt plan is the CLOUD asset, whose only host library is SQLite. So a bare host
# is refused for SQLite, and must NOT be sent after llama.cpp's two: nothing in that build loads
# them, and naming them would refuse an install that would have worked.
write_fake_ldconfig # prints nothing at all
out=$(run_install) && { bad "install.sh exited 0 on a host with none of $HOST_LIBS_FOR_TEST"; }
case "$out" in
  *"missing libraries the binaries need at run time"*) ;;
  *) bad "case A: the refusal did not appear. Output was: $out" ;;
esac
case "$out" in
  *"libsqlite3.so.0"*) ;;
  *) bad "case A: the refusal does not name libsqlite3.so.0" ;;
esac
case "$out" in
  *libstdc++* | *libgomp*)
    bad "case A: a cloud install was refused for llama.cpp's libraries, which it never loads"
    ;;
esac
note "A: a host with no SQLite is refused for SQLite, and not for llama.cpp's libraries"

# --- A2: the LOCAL prebuilt asset is checked for all three ----------------------
# `LETIBOT_LOCAL=1` asks for the asset that carries llama.cpp, so that plan needs what
# libllama and libggml link as well.
out=$(PLAN_ENV="LETIBOT_LOCAL=1" run_install) && { bad "case A2: exited 0 for the local asset on a host with none of the three"; }
for lib in libstdc++.so.6 libgomp.so.1 libsqlite3.so.0; do
  case "$out" in
    *"$lib"*) ;;
    *) bad "case A2: the local plan's refusal does not name $lib" ;;
  esac
done
note "A2: LETIBOT_LOCAL's prebuilt asset is refused for all three, llama.cpp's included"

# --- B: a host with only libgomp — the per-library check -------------------------
write_fake_ldconfig libgomp.so.1
out=$(run_install) && { bad "install.sh exited 0 on a host with only libgomp"; }
case "$out" in
  *"libsqlite3.so.0"*) ;;
  *) bad "case B: libsqlite3.so.0 is missing and was not named. Output was: $out" ;;
esac
# The library that IS present must not be reported — otherwise the message sends
# somebody to install something they already have.
case "$out" in
  *"libgomp.so.1 "*) bad "case B: libgomp.so.1 is PRESENT and was reported missing" ;;
esac
note "B: a host with only libgomp is refused for SQLite, and libgomp is not reported"

# --- C: a complete host, and the check must not fire -----------------------------
write_fake_ldconfig libstdc++.so.6 libgomp.so.1 libsqlite3.so.0
out=$(run_install) && { bad "install.sh exited 0 with no curl and no checkout"; }
case "$out" in
  *"missing libraries the binaries need at run time"*)
    bad "case C: the host check fired on a host that HAS all three. Output was: $out"
    ;;
esac
# It must have got past the host check and reached the next refusal, which on this
# PATH is the missing downloader.
case "$out" in
  *"no curl found"*) note "C: a complete host passes the check and fails later, on curl" ;;
  *) bad "case C: expected the run to reach the missing-curl refusal. Output was: $out" ;;
esac

# --- D: no glibc at all (musl) ------------------------------------------------------
#
# The prebuilt binaries cannot run on musl, so the refusal must say the asset is glibc and
# name the source build, not send the user after a library. Reached via an empty directory.
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
# The fallback's found branch, with ldconfig absent and the libraries only in the controlled dir.
fallback_dir="$work/fallback-libs"
mkdir -p "$fallback_dir"
for lib in libstdc++.so.6 libgomp.so.1 libsqlite3.so.0; do
  : >"$fallback_dir/$lib"
done
# glibc's loader must still be findable, or case D's refusal fires first: the
# controlled directory needs a loader file too.
: >"$fallback_dir/ld-linux-test.so.1"
rm -f "$work/ldconfig" # no ldconfig at all: this is the fallback
if out=$(run_install "$fallback_dir"); then
  bad "case E: install.sh exited 0 with no curl and no checkout"
else
  case "$?" in
    2) bad "case E: the patch did not apply, so this case proved nothing" ;;
  esac
  case "$out" in
    *"missing libraries the binaries need at run time"*)
      bad "case E: the fallback did not find libraries that are in its search path"
      ;;
  esac
  case "$out" in
    *"no curl found"*) note "E: with no ldconfig, the directory fallback finds them" ;;
    *) bad "case E: expected the run to reach the missing-curl refusal. Output was: $out" ;;
  esac
fi

# --- not covered: the fallback's not-found branch ---------------------------------
# It needs `have_glibc` and `host_has_lib` patched to different directories. D covers the same
# failure from the other side.

# ---------------------------------------------------------------- the build being installed
#
# The check asks for what this run installs: the release asset carries llama (all three); a
# source build without llama.cpp needs SQLite only; with llama.cpp, all three again.

# F. Cloud-only source build, on a host with only SQLite: not refused over libraries.
write_fake_ldconfig libsqlite3.so.0
out=$(PLAN_ENV="LETIBOT_FROM_SOURCE=1" run_install) && bad "case F: install.sh exited 0 with no curl"
case "$out" in
  *"missing libraries the binaries need at run time"*)
    bad "case F: a cloud-only build was refused over llama's libraries. Output was: $out"
    ;;
esac
case "$out" in
  *"no curl found"*) note "F: a cloud-only build needs only SQLite, and gets past the check" ;;
  *) bad "case F: expected the run to reach the missing-curl refusal. Output was: $out" ;;
esac

# G. Cloud-only, and SQLite missing: refused, naming SQLite and ONLY SQLite.
write_fake_ldconfig
out=$(PLAN_ENV="LETIBOT_FROM_SOURCE=1" run_install) && bad "case G: exited 0 with no SQLite"
case "$out" in
  *"libsqlite3.so.0"*) ;;
  *) bad "case G: libsqlite3.so.0 is missing and was not named. Output was: $out" ;;
esac
case "$out" in
  *"libgomp"* | *"libstdc++"*) bad "case G: a cloud-only refusal names llama's libraries: $out" ;;
esac
note "G: a cloud-only build missing SQLite is refused for SQLite alone"

# H. A source build that names a llama.cpp needs llama's libraries again.
write_fake_ldconfig libsqlite3.so.0
out=$(PLAN_ENV="LETIBOT_FROM_SOURCE=1 LETIBOT_LLAMA_DIR=/x LETIBOT_LLAMA_LIB=/x" run_install) &&
  bad "case H: exited 0 with libgomp and libstdc++ missing"
case "$out" in
  *"libgomp.so.1"*) note "H: a local source build is checked for llama's libraries" ;;
  *) bad "case H: a local build missing libgomp was not refused for it. Output was: $out" ;;
esac

# I. No glibc, and a source build: not refused for glibc — the binaries are built here.
write_fake_ldconfig libsqlite3.so.0
if out=$(PLAN_ENV="LETIBOT_FROM_SOURCE=1" run_install "$glibc_absent"); then
  bad "case I: install.sh exited 0 with no curl"
else
  case "$?" in
    2) bad "case I: the patch did not apply, so this case proved nothing" ;;
  esac
  case "$out" in
    *"no glibc dynamic loader"*)
      bad "case I: a source build was refused for glibc, which the musl refusal itself recommends"
      ;;
    *"no curl found"*) note "I: on musl, the source build the refusal names is not refused itself" ;;
    *) bad "case I: expected the run to reach the missing-curl refusal. Output was: $out" ;;
  esac
fi

# A prebuilt refusal over llama's libraries names the way out: the cloud install, which is the
# default and needs none of them.
write_fake_ldconfig libsqlite3.so.0
out=$(PLAN_ENV="LETIBOT_LOCAL=1" run_install) || true
case "$out" in
  *"missing libraries the binaries need at run time"*"llama.cpp's"*"LETIBOT_FROM_SOURCE=1"*)
    note "J: a local prebuilt refusal over llama's libraries offers the cloud install"
    ;;
  *) bad "case J: the library refusal does not offer the cloud install. Output was: $out" ;;
esac

[ "$fail" = 0 ] || exit 1
echo "check-host-runtime: refuses by name, per library, distinguishes musl, does not false-positive"
