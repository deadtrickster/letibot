# The man-page extraction, as it was extracted

Two models were asked, program by program, which flags in a man page destroy
something that existed before the command ran. These are their answers, before
any filtering, so the table in `crates/tools/src/documented_flags.rs` can be
rebuilt from them and disagreed with.

    glm-5.3-flash.rows.jsonl     91 rows,  31 programs
    qwen3-27b.rows.jsonl        838 rows, 617 programs
    qwen3-27b.quarantined.jsonl  42 rows the verifier rejected, kept to be read

## Rebuilding the table

    scripts/verify-man-rows.py docs/evidence/man-scrape/qwen3-27b.rows.jsonl \
        --verified /tmp/qwen.verified.jsonl --quarantine /tmp/qwen.quarantine.jsonl
    scripts/verify-man-rows.py docs/evidence/man-scrape/glm-5.3-flash.rows.jsonl \
        --verified /tmp/glm.verified.jsonl
    scripts/man-rows-to-table.py \
        --rows glm-5.3-flash=/tmp/glm.verified.jsonl \
        --rows qwen3-27b=/tmp/qwen.verified.jsonl \
        --hand-table crates/tools/src/intent.rs \
        --out crates/tools/src/documented_flags.rs

`verify-man-rows.py` reads the man pages **on the box it runs on**, so the
numbers move with what is installed. Measured on lab2x1, 2026-09-18:

    glm    91 rows   82 verified    0 quarantined    9 destroy-by-default
    qwen  838 rows  578 verified   42 quarantined  188 destroy-by-default   30 no page

GLM's 91 are a useful calibration rather than just input: they were checked by
hand when they were extracted and every one names a real flag, so a verifier
that rejects any of them is wrong about that row. The first version rejected 24.

## What the quarantine file is for

It is the extractor reporting its own drift. A 27B run wrote rows describing a
DIFFERENT program than the one asked about, because the harness did
`o["program"] = prog` after parsing and overwrote the model's own answer. The
rejected rows still read as that: `git-diff-files` / `reject` is a sentence
about `git credential`, and `dnsmasq` / `remove` is one about `dmsetup`. Kept
rather than deleted, because a list of the ways an instrument lies is worth
more than a clean file.

## What is NOT in the table, and why

**Rows naming a verb rather than a flag** (458). Both models read "flags or
subcommands that DESTROY" as an invitation to name subcommands that CAN, so
`git reset`, `git branch` and `git tag` arrived as bare words. A rule built from
one fires on every use of the verb: `git reset HEAD~5` came back destructive,
and it leaves every commit reachable. The hand-written answer key caught it as
the only false positive in thirteen negatives.

**Rows whose program is not installed here** (10). Nothing was checked against a
page that could not be read, and "never checked" must not be able to read as
"checked and clean".
