#!/bin/sh
# Package built binaries as the release asset `install.sh` asks for.
#
#   scripts/make-dist.sh <target-triple> [outdir]
#
# The contract is `install.sh`'s, not this script's: it downloads
# `letibot-<triple>.tar.gz` from the release and expects the binaries at the
# archive root. That naming lives in ONE place — here — and both workflows call
# this script, so a release asset cannot be built with a name the installer does
# not ask for. `scripts/check-dist-names.sh` asserts the triples line up.
#
# # What goes in, and why not the other three binaries
#
#   harnessd          the daemon — the session, the gate, the model connection
#   letibot-tui       the head — what a person actually looks at
#   letibot-askpass   what `sudo` runs inside a session to ask for a password
#
# **`letibot-askpass` must sit BESIDE `harnessd`** — `crate::sudo::install()`
# looks for it next to the running binary and installs a `sudo` shim that calls
# it. A package that left it out would give a session with no way to answer a
# password prompt, and `sudo` would fail with no explanation.
#
# **The three left out are not runtime**: `letibot-m1` is M1's exit measurement,
# and `letibot-render` / `letibot-render-qwen` are the seams between the Rust
# renderers and the Python fidelity gate. All three are built by `cargo build
# --release` and none is needed to run a session. Shipping six would be three more
# things to version and a slower install for nothing.
#
# # And the FOUR llama libraries, which are NOT optional
#
# **FOUR, and this is the correction that matters most in this file.** The chain is
# two levels deep, not one:
#
#     harnessd      -> libllama.so.0
#     libllama.so.0 -> libggml.so.0, libggml-base.so.0
#     libggml.so.0  -> libggml-cpu.so.0, libggml-base.so.0      <-- the second level
#     libggml-cpu.so.0 -> libggml-base.so.0
#
# An earlier version of this script shipped THREE (llama, ggml, ggml-base) because
# the check stopped one level down: it read `harnessd`'s `DT_NEEDED`, then
# `libllama.so.0`'s, and never asked what `libggml.so.0` needs. MEASURED, both ways,
# in a bare directory with the llama.cpp tree made unreachable:
#
#     three libraries -> ./harnessd: error while loading shared libraries:
#                        libggml-cpu.so.0: cannot open shared object file      (exit 127)
#     four libraries  -> harnessd 0.1.1                                       (exit 0)
#
# **And the wrong version PASSED on the development box**, which is the part worth
# remembering: that machine's `libggml.so.0` carries an absolute runpath into
# `build-glm/bin`, so the loader found `libggml-cpu.so.0` there and the daemon
# started. It would have failed on every machine without that directory — the
# precise failure shape this script exists to keep off a user's disk, reproduced
# inside the script that guards against it.
#
# The authoritative list is the transitive closure, taken with `ldd` on a library
# built with `$ORIGIN` only (so nothing resolves by accident):
#
#     libllama.so.0  libggml.so.0  libggml-cpu.so.0  libggml-base.so.0
#
# Everything else `ldd` reports is system: `libc`, `libm`, `libstdc++`, `libgcc_s`
# and `libgomp` (the OpenMP runtime, which ships with gcc on every normal distro).
#
# `libggml-cuda.so` is NOT in this set for the release build, because it is 68 MB,
# CUDA is off, and ggml reaches its backends through the CPU one. It IS in this
# box's own `libggml.so.0`'s `DT_NEEDED` — this box's llama.cpp is a CUDA build —
# which is another reason the list above is taken from a `$ORIGIN`-only build
# rather than from whatever happens to be installed here.
#
# They are packaged because `harnessd`'s build script bakes `$ORIGIN` into its
# runpath: the daemon finds these next to itself, and a package missing any one of
# them dies at exec with `cannot open shared object file` on every machine that has
# no llama.cpp checkout.
#
# `LETIBOT_LLAMA_LIB` names the directory they are copied from — the same variable
# the build script uses, so the libraries packaged are the libraries linked.

set -eu

triple="${1:-}"
if [ -z "$triple" ]; then
    echo "usage: make-dist.sh <target-triple> [outdir]" >&2
    exit 2
fi
outdir="${2:-dist}"

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

for want in harnessd letibot-tui letibot-askpass; do
    [ -x "$bin/$want" ] || { echo "make-dist: $bin/$want is missing or not executable" >&2; exit 1; }
done

# The libraries are a hard requirement, not a best effort: a package that names
# them but does not carry them installs cleanly and then fails to start, which is
# the failure shape this whole script exists to keep off a user's machine.
for so in libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0; do
    if [ ! -f "$lib/$so" ]; then
        echo "make-dist: $lib/$so not found." >&2
        echo "  harnessd links it and needs it in the package. Set LETIBOT_LLAMA_LIB" >&2
        echo "  to the directory holding libllama.so.0 (a llama.cpp build tree's bin/)." >&2
        exit 1
    fi
done

mkdir -p "$outdir"
name="letibot-$triple.tar.gz"

# **A staging directory rather than `tar -C` twice.** The binaries come from the
# target directory and the libraries from the llama tree, and an archive built from
# two `-C` invocations is one whose contents depend on argument order. Staging makes
# the archive a listing of one directory, which is also what the check below walks.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT INT TERM

for want in harnessd letibot-tui letibot-askpass; do
    cp "$bin/$want" "$stage/$want"
done
for so in libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0; do
    # `-L`: these are symlinks in a llama.cpp build tree (libllama.so.0 →
    # libllama.so.0.4.0) and an archive holding a dangling symlink extracts to
    # nothing. The real file is what has to travel.
    cp -L "$lib/$so" "$stage/$so"
done
chmod 755 "$stage"/*

# **THE LIBRARIES MUST NOT DEPEND ON AN ABSOLUTE PATH, and this is the check for it.**
#
# MEASURED, 2026-10-01, and it is the defect that would have shipped: `harnessd`'s
# own runpath is `[$ORIGIN, <llama build dir>]`, so it finds `libllama.so.0` beside
# itself — but **`DT_RUNPATH` is NOT inherited by a library's own dependencies**.
# `libllama.so.0` carries `RUNPATH=/home/dead/Projects/llama.cpp/build-glm/bin` and
# looks for `libggml.so.0` THERE and nowhere else, so on a machine with no llama.cpp
# at that path the loader gets as far as libllama and then dies with
# `libggml.so.0: cannot open shared object file`.
#
# Proven with a three-line experiment rather than taken from the manual: a
# `libparent.so` with `RUNPATH=/nonexistent-only`, a `libchild.so` beside it, and a
# `prog` with `RUNPATH=$ORIGIN` — all three in one directory. Running it fails with
# `libchild.so: cannot open shared object file`, which is the whole of the above.
#
# **So the fix is at BUILD time, in whoever builds llama.cpp**: it must be
# configured with `-DCMAKE_INSTALL_RPATH='$ORIGIN'` (and
# `-DCMAKE_BUILD_RPATH='$ORIGIN'`) so every library in the set finds its siblings
# beside itself. `patchelf --set-rpath '$ORIGIN'` on the four libraries does the same
# to an existing build. This script cannot fix it — it only assembles — so it
# REFUSES rather than packaging something that installs cleanly and then dies on a
# user's machine, which is the one failure shape a release must not have.
#
# It is applied to the STAGED COPIES and never to the operator's build tree: a
# release script that rewrote somebody's llama.cpp checkout would be a worse bug
# than the one it fixes, and `patchelf` is only needed when the libraries were built
# without `$ORIGIN` in the first place.
for so in libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0; do
    [ -f "$stage/$so" ] || continue
    # **Both spellings, and the empty case is NOT a pass.** `RPATH` is the older tag
    # and a library may carry either; matching only `RUNPATH` would read an `RPATH`-only
    # library as having no runpath at all. And a library with NO runpath is not fine:
    # it cannot find its sibling any more than an absolute one can, so the empty case
    # falls through to the refusal below rather than being accepted by a bare `''`.
    rp=$(readelf -d "$stage/$so" 2>/dev/null |
        sed -n 's/.*\(RUNPATH\|RPATH\)[^[]*\[\(.*\)\].*/\2/p' | head -1)
    case "$rp" in
        '$ORIGIN') ;;                 # exactly right: finds siblings, nothing else
        *'$ORIGIN'*)
            # It will RUN — `$ORIGIN` is searched — but it also carries a path from
            # wherever it was built, which is inside a published binary. MEASURED:
            # this is what CMake produces without `CMAKE_BUILD_WITH_INSTALL_RPATH=ON`
            # (`[$ORIGIN:/home/runner/.../llama.cpp/build/bin]`). Not fatal, so not a
            # refusal, but it belongs on the record rather than in the archive quietly.
            echo "make-dist: warning: $so's RUNPATH is \"$rp\"." >&2
            echo "  It will run, but it names a directory from the machine that built it," >&2
            echo "  and that path is now inside a published binary. Build llama.cpp with" >&2
            echo "  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON to get a clean [\$ORIGIN]." >&2
            ;;
        *)
            if command -v patchelf >/dev/null 2>&1; then
                patchelf --set-rpath '$ORIGIN' "$stage/$so" ||
                    { echo "make-dist: patchelf failed on $so" >&2; exit 1; }
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

# **THE CLOSURE CHECK — the one whose absence shipped a broken package.**
#
# The check above names four libraries and asserts they exist. That is a list, and
# a list is only as good as the person who wrote it: the version that named three
# passed every test on the development box and would have died on a user's machine
# with `libggml-cpu.so.0: cannot open shared object file`.
#
# This asks the question the list cannot: **does every library the staged files
# need actually come from somewhere the user will have?** For each ELF in the
# stage, every `DT_NEEDED` must be either (a) already in the stage, or (b) a
# library the base system provides, per `ldconfig`. Anything else is a hole.
#
# It is deliberately a NAME check and not an `ldd` run. MEASURED, and this is the
# whole reason: `ldd` on the staged `harnessd` resolves through the binary's own
# runpath, which legitimately carries the absolute llama.cpp build directory after
# `$ORIGIN` — so the loader finds the missing library on the build machine and
# `ldd` reports a clean closure. That is precisely how the three-library package
# passed here. `ldconfig` consults no runpath, so it cannot be fooled the same way.
#
# `ldconfig` is glibc's; on a musl-only system it is absent and this check SKIPS
# with a line saying so, rather than passing silently. The package is glibc-linked
# in any case (it carries `libstdc++`), so the target is a glibc system.
system_libs=""
if command -v ldconfig >/dev/null 2>&1; then
    system_libs=$(ldconfig -p 2>/dev/null | awk '{print $1}' | sort -u | tr '\n' ' ')
else
    echo "make-dist: note: no ldconfig here, so the dependency closure of the" >&2
    echo "  package cannot be checked. That check is what catches a library the" >&2
    echo "  package forgot; without it, a hole is found by a user instead." >&2
fi

if [ -n "$system_libs" ]; then
    for f in "$stage"/harnessd "$stage"/letibot-tui "$stage"/letibot-askpass "$stage"/lib*.so.0; do
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
fi



# COPYFILE_DISABLE stops macOS tar writing AppleDouble `._harnessd` entries, which
# would otherwise land in the archive and be extracted by the installer.
COPYFILE_DISABLE=1 tar -czf "$outdir/$name" -C "$stage" .

# **Prove the archive is what the installer expects before it is published.** Every
# file at the root, the three binaries executable, and no extra entries — a stray
# file riding along into a release is a thing that only shows up on a user's disk.
listing=$(tar -tzf "$outdir/$name" | sed 's|^\./||' | grep -v '^$' | sort)
expected=$(printf '%s\n' harnessd letibot-tui letibot-askpass \
    libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0 | sort)
if [ "$listing" != "$expected" ]; then
    echo "make-dist: $name does not hold what it should." >&2
    echo "--- expected:" >&2
    printf '%s\n' "$expected" >&2
    echo "--- got:" >&2
    printf '%s\n' "$listing" >&2
    exit 1
fi

printf '%s\n' "$outdir/$name"
