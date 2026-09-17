# The 91 GLM rows, checked against the pages they came from

Static check, no model: does each row's flag literally appear in that program's
own man page, after undoing roff's `\-` hyphen escape?

    rows 91    verified 88    quarantined 3

All three quarantined rows were then checked by hand and are REAL:

    ssh-add  -D            `man ssh-add` shows it; ssh-add.1 is mdoc, so flags
    ssh-add  -e pkcs11     are macros (`.Fl D`) and the literal never appears
    shred    --remove      real; my tokeniser kept the `[=HOW]` suffix

So 91 of 91 name flags that exist. No drift was found in this run.

## Why this check exists

lubuntu1-lab's 27B run produced rows describing a DIFFERENT program than the one
asked about, at ~10%. Their harness did `o["program"] = prog` after parsing,
overwriting the model's own `program` field with the asked-for one — so the model
was reporting its drift and the code erased the report before writing. Invented
flags (`--dms-delete-stored-image`, in zero man pages on that box) then looked
authoritative.

This check catches that class without a model and without trusting the extractor:
a flag that is not in the page is not a flag, whatever wrote it down.

## Two ways it under-reports, stated so nobody reads 88/91 as a score

1. **mdoc pages.** Flags are macros, not literal text. A literal search rejects
   real flags — two of the three above. A page in mdoc needs `man(1)` to render
   it first.
2. **It checks EXISTENCE, not MEANING.** A flag that is in the page but does not
   destroy anything passes here. That is what the answer key measures, and this
   is not a substitute for it.

It also cannot see the reverse error — a destructive flag the model MISSED is
invisible to a check that only looks at rows that exist. Recall is the answer
key's job.
