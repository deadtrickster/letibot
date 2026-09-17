# Two corpora, and they are not the same thing

Both are produced as a side effect of working, and conflating them would make
each useless for the other's purpose.

## 1. `adjudication` — what the OPERATOR decided (in sessions.db)

One row per gated call: the un-normalised input (tool, arguments, mode, options),
layer A's reading (action, baseline, tier, trail), what the ORACLE said, what the
gate did, and — separately — what the operator ruled. Written by
`letibot-tokencore`'s `record_adjudication`, labelled by `record_operator_ruling`.

Fine-tunes: **the guard**. Input is the brief, target is the verdict.
The label is the operator's ruling, and the rows worth most are the ones where
the ruling and the verdict differ.

## 2. `layer-a` — what a PROGRAM does (this directory)

One row per destructive flag, extracted from the program's own man page by a
local model. Input is the man-page excerpt, target is the structured row.

Fine-tunes: **the extractor**, and populates layer A's table directly.
The label is the man page, which is why `provenance` carries the source: a row
that cannot cite the text it came from is a row nobody can check.

## Why they must not be merged

The first is about AUTHORISATION and its ground truth is a person. The second is
about EFFECT and its ground truth is documentation. A model trained on the union
would learn that a person's approval and a man page's wording are the same kind
of evidence, which is the confusion the whole two-layer design exists to prevent.

## Row shape here

    {"program":"tar","flag":"--overwrite","unless":[],"conditional":false,
     "why":"...","source":"man tar(1)","extractor":"glm-5.3-flash",
     "page_sha":"...","run":"..."}

`extractor` and `page_sha` are what make agreement measurable: the same page put
through two models is two rows that can be compared, and the same page put
through one model twice bounds the instrument.
