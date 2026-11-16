#!/bin/sh
# Package built binaries as the release asset `install.sh` asks for.
#
#   scripts/make-dist.sh <target-triple> [outdir]
#
# install.sh downloads `letibot-<triple>.tar.gz` and expects the binaries at the archive root.
# `scripts/check-dist-names.sh` holds the names to install.sh.
#
# `letibot-askpass` is a link to `letibot-multi` that install.sh makes, and must sit beside
# `harnessd`: `sudo::install()` looks for it there. `letibot-m1` and the `letibot-render*`
# binaries are measurement and fidelity seams, not runtime, so they are left out.
#
# The four llama libraries are the transitive closure (taken with `ldd` on an `$ORIGIN`-only
# build), two levels deep:
#
#     harnessd      -> libllama.so.0
#     libllama.so.0 -> libggml.so.0, libggml-base.so.0
#     libggml.so.0  -> libggml-cpu.so.0, libggml-base.so.0
#     libggml-cpu.so.0 -> libggml-base.so.0
#
# `libggml-cuda.so` is not in the set: CUDA is off in the release build. `harnessd`'s runpath
# is `$ORIGIN`, so a package missing any of them dies at exec on a box without llama.cpp.
# `LETIBOT_LLAMA_LIB` names the directory they are copied from, the one the build linked.

set -eu

# The archive's contents, in one place. install.sh must agree (`check-dist-names.sh`), because
# the installer and the archive come from different refs. The set is the contract, not the order.
# The launcher is in the archive: install.sh tells the user to run `letibot`, and shipping it
# here keeps the script and the binaries on one tag. It is staged from `scripts/`.
ARCHIVE_BINARIES="harnessd letibot-tui letibot-multi letibot leticode letibot-mcp letibot-acp"
ARCHIVE_LIBRARIES="libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0"
# The same four for an `*-apple-darwin` triple, which install.sh's LIBRARIES_DARWIN
# must match (`check-dist-names.sh` holds the two to each other).
ARCHIVE_LIBRARIES_DARWIN="libllama.0.dylib libggml.0.dylib libggml-cpu.0.dylib libggml-base.0.dylib"

triple="${1:-}"
if [ -z "$triple" ]; then
  echo "usage: make-dist.sh <target-triple> [outdir]" >&2
  exit 2
fi
case "$triple" in
  *-apple-darwin)
    darwin=1
    ARCHIVE_LIBRARIES=$ARCHIVE_LIBRARIES_DARWIN
    ;;
  *) darwin=0 ;;
esac
ARCHIVE="$ARCHIVE_BINARIES $ARCHIVE_LIBRARIES"
outdir="${2:-dist}"

# shellcheck disable=SC1007 # an empty CDPATH for this cd only, so it cannot print a path
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
lib="${LETIBOT_LLAMA_LIB:-/home/dead/Projects/llama.cpp/build-glm/bin}"

# Native build first, then a cross build's output.
bin=""
for candidate in "$repo/target/release" "$repo/target/$triple/release"; do
  if [ -x "$candidate/harnessd" ]; then
    bin="$candidate"
    break
  fi
done
if [ -z "$bin" ]; then
  echo "make-dist: no built binaries for $triple" >&2
  echo "  looked in target/release/ and target/$triple/release/" >&2
  exit 1
fi

for want in $ARCHIVE_BINARIES; do
  src="$bin/$want"
  if [ -x "$src" ] || [ -x "$repo/scripts/$want" ]; then
    continue
  fi
  echo "make-dist: no $want in $bin or $repo/scripts/" >&2
  exit 1
done

# The libraries are a hard requirement, not a best effort: a package that names
# them but does not carry them installs cleanly and then fails to start, which is
# the failure shape this whole script exists to keep off a user's machine.
for so in $ARCHIVE_LIBRARIES; do
  if [ ! -f "$lib/$so" ]; then
    echo "make-dist: $lib/$so not found." >&2
    echo "  harnessd links it and needs it in the package. Set LETIBOT_LLAMA_LIB" >&2
    echo "  to the directory holding ${ARCHIVE_LIBRARIES%% *} (a llama.cpp build tree's bin/)." >&2
    exit 1
  fi
done

mkdir -p "$outdir"
name="letibot-$triple.tar.gz"

# A staging directory rather than `tar -C` twice, so the archive is a listing of one
# directory and does not depend on argument order. The checks below walk it.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT INT TERM

for want in $ARCHIVE_BINARIES; do
  # The launcher is a repository script; everything else is a build product. One
  # list, two sources, because two lists is how the two drift.
  if [ -f "$repo/scripts/$want" ]; then
    cp "$repo/scripts/$want" "$stage/$want"
  else
    cp "$bin/$want" "$stage/$want"
  fi
  # A launcher name takes only a script: a stale `target/release/letibot` from before the
  # multicall was renamed would otherwise be staged under the command a person types.
  if [ "$want" = "letibot" ] || [ "$want" = "leticode" ]; then
    if head -c 2 "$stage/$want" 2>/dev/null | grep -q '^#!'; then
      :
    else
      echo "make-dist: $stage/$want is not a script." >&2
      echo "  That name belongs to the launcher; the multicall is letibot-multi." >&2
      exit 1
    fi
  fi
done
for so in $ARCHIVE_LIBRARIES; do
  # `-L`: these are symlinks in a llama.cpp build tree (libllama.so.0 →
  # libllama.so.0.4.0) and an archive holding a dangling symlink extracts to
  # nothing. The real file is what has to travel.
  cp -L "$lib/$so" "$stage/$so"
done
chmod 755 "$stage"/*

# The libraries must not depend on an absolute path. `DT_RUNPATH` is not inherited by a
# library's dependencies, so a `libllama.so.0` with an absolute runpath finds its siblings only
# on the build box. The fix is at llama.cpp build time (`-DCMAKE_INSTALL_RPATH='$ORIGIN'`), or
# `patchelf --set-rpath '$ORIGIN'` on the staged copies. The operator's tree is never rewritten.
# macOS: the same guarantees in Mach-O terms, and the Linux checks below are skipped. Each staged
# dylib must use `@rpath/<name>` and `@loader_path`; `install_name_tool` fixes the copy and
# `codesign -s -` re-signs it, since Apple silicon will not load an unsigned dylib. Then every
# dependency is either `@rpath/` and staged, or the OS's (`/usr/lib`, `/System/Library`).
if [ "$darwin" = 1 ]; then
  for so in $ARCHIVE_LIBRARIES; do
    f="$stage/$so"
    changed=0
    if [ "$(otool -D "$f" | sed -n 2p)" != "@rpath/$so" ]; then
      install_name_tool -id "@rpath/$so" "$f"
      changed=1
    fi
    for rp in $(otool -l "$f" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }'); do
      [ "$rp" = "@loader_path" ] && continue
      install_name_tool -delete_rpath "$rp" "$f"
      changed=1
    done
    if ! otool -l "$f" | grep -q 'path @loader_path '; then
      install_name_tool -add_rpath @loader_path "$f"
      changed=1
    fi
    if [ "$changed" = 1 ]; then
      codesign --force -s - "$f" 2>/dev/null ||
        {
          echo "make-dist: codesign failed on the fixed copy of $so" >&2
          exit 1
        }
      echo "make-dist: $so: install name and rpath set to @rpath/@loader_path, re-signed"
    fi
    chmod 644 "$f"
  done
  staged_libs=""
  for so in $ARCHIVE_LIBRARIES; do staged_libs="$staged_libs $stage/$so"; done
  for f in "$stage"/harnessd "$stage"/letibot-tui $staged_libs; do
    for dep in $(otool -L "$f" | tail -n +2 | awk '{print $1}'); do
      case "$dep" in
        @rpath/*)
          [ -f "$stage/${dep#@rpath/}" ] && continue
          echo "make-dist: ${dep#@rpath/} is needed by $(basename "$f") and is not in" >&2
          echo "  the package. A Metal or BLAS build of llama.cpp also links" >&2
          echo "  libggml-metal and libggml-blas; the release builds CPU-only" >&2
          echo "  (-DGGML_METAL=OFF -DGGML_BLAS=OFF) so that the four listed are all." >&2
          exit 1
          ;;
        /usr/lib/* | /System/Library/*) ;;
        *)
          echo "make-dist: $(basename "$f") links $dep, which is neither in the" >&2
          echo "  package nor part of macOS — an absolute path from the build machine." >&2
          exit 1
          ;;
      esac
    done
  done
  echo "make-dist: dependency closure complete for $stage (otool: package + OS only)"
else

  for so in $ARCHIVE_LIBRARIES; do
    [ -f "$stage/$so" ] || continue
    # Both `RPATH` and `RUNPATH`. A library with no runpath cannot find its sibling either, so
    # the empty case falls through to the refusal.
    rp=$(readelf -d "$stage/$so" 2>/dev/null |
      sed -n 's/.*\(RUNPATH\|RPATH\)[^[]*\[\(.*\)\].*/\2/p' | head -1)
    # shellcheck disable=SC2016 # `$ORIGIN` is the loader's token, matched literally
    case "$rp" in
      '$ORIGIN') ;; # exactly right: finds siblings, nothing else
      *'$ORIGIN'*)
        # It runs (`$ORIGIN` is searched) but carries a build path into a published binary, as CMake
        # does without `CMAKE_BUILD_WITH_INSTALL_RPATH=ON`. Not fatal, so a warning.
        echo "make-dist: warning: $so's RUNPATH is \"$rp\"." >&2
        echo "  It will run, but it names a directory from the machine that built it," >&2
        echo "  and that path is now inside a published binary. Build llama.cpp with" >&2
        echo "  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON to get a clean [\$ORIGIN]." >&2
        ;;
      *)
        if command -v patchelf >/dev/null 2>&1; then
          # shellcheck disable=SC2016 # `$ORIGIN` is written literally for the loader to expand
          patchelf --set-rpath '$ORIGIN' "$stage/$so" ||
            {
              echo "make-dist: patchelf failed on $so" >&2
              exit 1
            }
          continue
        fi
        echo "make-dist: $so's runpath is \"$rp\", which is not \$ORIGIN," >&2
        echo "  and patchelf is not installed to fix the copy." >&2
        if [ -z "$rp" ]; then
          echo "  It has NO runpath at all, so it cannot find its siblings beside" >&2
          echo "  itself any more than an absolute one can." >&2
        fi
        echo "  A library finds its own dependencies through its own runpath, and" >&2
        echo "  DT_RUNPATH is NOT inherited — so this package would run on this box" >&2
        echo "  and fail on every machine without that directory." >&2
        echo "  Rebuild llama.cpp with -DCMAKE_INSTALL_RPATH='\$ORIGIN' (and" >&2
        echo "  -DCMAKE_BUILD_RPATH='\$ORIGIN'), or patchelf --set-rpath '\$ORIGIN' it." >&2
        echo "" >&2
        echo "  MEASURED, and it is the cheapest fix rather than a rebuild: reconfiguring" >&2
        echo "  the EXISTING build directory only RELINKS the shared objects, because the" >&2
        echo "  object files are already compiled. All three flags are needed — without" >&2
        echo "  CMAKE_BUILD_WITH_INSTALL_RPATH=ON, CMake appends its own build directory" >&2
        echo "  and you get [\$ORIGIN:<build dir>] instead of a clean [\$ORIGIN]:" >&2
        echo "" >&2
        echo "    cmake -S <llama.cpp> -B <its build dir> \\" >&2
        echo "      -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON \\" >&2
        echo "      -DCMAKE_BUILD_RPATH='\$ORIGIN' -DCMAKE_INSTALL_RPATH='\$ORIGIN'" >&2
        echo "    cmake --build <its build dir> -j \"\$(nproc)\"" >&2
        exit 1
        ;;
    esac
  done

  chmod 644 "$stage"/lib*.so.0

  # The closure check: every `DT_NEEDED` of every staged ELF is either staged or provided by the
  # base system per `ldconfig`. Not an `ldd` run: `ldd` resolves through the binary's runpath,
  # which carries the build directory, so it would find a missing library on the build box.
  # No `ldconfig` (musl) skips with a line saying so; the package targets glibc anyway.
  system_libs=""
  if command -v ldconfig >/dev/null 2>&1; then
    system_libs=$(ldconfig -p 2>/dev/null | awk '{print $1}' | sort -u | tr '\n' ' ')
  else
    echo "make-dist: note: no ldconfig here, so the dependency closure of the" >&2
    echo "  package cannot be checked. That check is what catches a library the" >&2
    echo "  package forgot; without it, a hole is found by a user instead." >&2
  fi

  if [ -n "$system_libs" ]; then
    for f in "$stage"/harnessd "$stage"/letibot-tui "$stage"/letibot-multi "$stage"/lib*.so.0; do
      [ -f "$f" ] || continue
      # `-f` means a symlink to a real file passes; the staged libraries are real
      # files because the copy above used `cp -L`.
      needs=$(readelf -d "$f" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\].*/\1/p')
      for need in $needs; do
        # In the package already: fine, whatever it is.
        [ -f "$stage/$need" ] && continue
        # Otherwise the system has to provide it. The leading and trailing
        # spaces make this an exact name match rather than a substring, so
        # `libggml.so.0` cannot be satisfied by `libggml-base.so.0`.
        case " $system_libs " in
          *" $need "*) continue ;;
        esac
        echo "make-dist: $need is needed by $(basename "$f") and is in neither" >&2
        echo "  the package nor the base system, so the archive would be missing it." >&2
        echo "  Add it to the library list at the top of this script, and copy it" >&2
        echo "  from LETIBOT_LLAMA_LIB. (This is the check that catches a" >&2
        echo "  dependency one level deeper than the list was written for.)" >&2
        exit 1
      done
    done
    echo "make-dist: dependency closure complete for $stage (ldconfig consulted)"

    # The build host has `libstdc++`, `libgomp` and `libsqlite3`, so the closure check passes them.
    # install.sh refuses by name over `HOST_LIBS`, so every host library the closure found must be in
    # `HOST_LIBS` or on the short list every glibc system has. An unclassified dependency fails.
    always='libc.so.6 libm.so.6 libgcc_s.so.1'
    candidates=''
    for f in "$stage"/harnessd "$stage"/letibot-tui "$stage"/letibot-multi "$stage"/lib*.so.0; do
      [ -f "$f" ] || continue
      for need in $(readelf -d "$f" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\].*/\1/p'); do
        [ -f "$stage/$need" ] && continue
        case " $always " in *" $need "*) continue ;; esac
        # The glibc loader, matched by shape: it is arch-named (`ld-linux-x86-64.so.2`,
        # `ld-linux-aarch64.so.1`), and install.sh's `have_glibc` globs for the same shape.
        case "$need" in ld-linux*.so.*) continue ;; esac
        case " $candidates " in *" $need "*) continue ;; esac
        candidates="$candidates $need"
      done
    done
    # What install.sh promises to check for, read from the file rather than restated.
    install_sh_host=$(sed -n 's/^HOST_LIBS="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
    if [ -z "$install_sh_host" ]; then
      echo "make-dist: install.sh has no HOST_LIBS line to compare against." >&2
      echo "  That list is what refuses an install on a machine missing a runtime" >&2
      echo "  library; without it the failure is an exit-127 on a user's box." >&2
      exit 1
    fi
    unlisted=''
    for need in $candidates; do
      case " $install_sh_host " in
        *" $need "*) ;;
        *) unlisted="$unlisted $need" ;;
      esac
    done
    # And the reverse: a name in `HOST_LIBS` that nothing needs would turn a user away from an
    # install that works.
    # shellcheck disable=SC2086 # the lists are split into words on purpose
    echo "make-dist: host-provided libraries: $(printf '%s ' $candidates)"
    # shellcheck disable=SC2086 # the same
    echo "make-dist: install.sh names:          $(printf '%s ' $install_sh_host)"
    stale=''
    for named in $install_sh_host; do
      case " $candidates " in
        *" $named "*) ;;
        *) stale="$stale $named" ;;
      esac
    done
    if [ -n "$unlisted" ]; then
      echo "make-dist: these are needed at run time and install.sh does not check for them:$unlisted" >&2
      echo "  They come from the host, so a machine without them installs cleanly and" >&2
      echo "  then dies at exec. Either add them to HOST_LIBS in install.sh (with the" >&2
      echo "  package name for the fix), or add them to \`always\` above if every glibc" >&2
      echo "  system has them by definition." >&2
      exit 1
    fi
    if [ -n "$stale" ]; then
      echo "make-dist: install.sh refuses over libraries nothing here needs:$stale" >&2
      echo "  A stale name turns a user away from an install that would have worked," >&2
      echo "  quoting a package they already have. Remove it from HOST_LIBS." >&2
      exit 1
    fi
  fi

# COPYFILE_DISABLE stops macOS tar writing AppleDouble `._harnessd` entries, which
# would otherwise land in the archive and be extracted by the installer.
fi # the Linux half, from the RUNPATH check down; macOS's is the block above it

COPYFILE_DISABLE=1 tar -czf "$outdir/$name" -C "$stage" .

# Prove the archive is what the installer expects before it is published. Every
# file at the root, the three binaries executable, and no extra entries — a stray
# file riding along into a release is a thing that only shows up on a user's disk.
listing=$(tar -tzf "$outdir/$name" | sed 's|^\./||' | grep -v '^$' | sort)
# shellcheck disable=SC2086 # one name per word of the list
expected=$(printf '%s\n' $ARCHIVE | sort)
if [ "$listing" != "$expected" ]; then
  echo "make-dist: $name does not hold what it should." >&2
  echo "--- expected:" >&2
  printf '%s\n' "$expected" >&2
  echo "--- got:" >&2
  printf '%s\n' "$listing" >&2
  exit 1
fi

printf '%s\n' "$outdir/$name"
