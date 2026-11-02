<!-- abstract: Any edit to `firecode.layer` re-runs every line, and the `chown -R` over the toolchain was the bulk of it because metacopy=N makes chowning a lower-layer inode copy its data up. -->

# firecode.layer probe 6 — MEASURED 2026-10-10, guest `firecode` (kernel 14:00:41)

The layer's stamp `/var/lib/firecode-layer.sha` is per FILE, not per line, so a comment edit re-runs
every RUN line in the next VM: boot to layer-done was 37.9 s, and ~18 s of it was the unconditional
`chown -R 1000:1000` over 1.9 GB of toolchain.

Ownership in the image buys nothing: `/proc/mounts` shows the root overlay with
`/sys/module/overlay/parameters/metacopy` = **N**, so a metadata change on an inode in a lower layer
copies the whole file's data up. `firecode.layer` now chowns from a `stat`, so the copy happens only on
a guest built from a bare image.

A guard that skips writes nothing, so mtimes are the evidence it held: `/usr/local/bin/cargo` still
carrying the image's time means `[ ! -x /usr/local/bin/cargo ]` skipped the relink. The format is one
directive per line, so a trailing backslash is not a continuation and costs the whole file.
