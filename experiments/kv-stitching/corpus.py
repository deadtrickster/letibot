"""Deterministic record corpus.

Every block is a list of "records" carrying a unique id and three fields. A
question about a record has exactly one right answer, and the answer is only
derivable from the block that holds it - so a continuation that answers from
the wrong block is visibly wrong rather than plausibly wrong.
"""
import random

TAGS = ["amber", "basalt", "cinnabar", "dolomite", "ember", "flint", "garnet",
        "hematite", "indigo", "jasper", "kyanite", "lapis", "malachite",
        "nacre", "obsidian", "pyrite", "quartz", "realgar", "serpentine",
        "topaz", "ultramarine", "verdite", "wolframite", "xenotime", "zircon"]

OWNERS = ["Nadia Petrov", "Ilya Sokolov", "Marta Reyes", "Kenji Watanabe",
          "Aoife Byrne", "Tomas Novak", "Lena Fischer", "Omar Haddad",
          "Priya Nair", "Sofia Rossi", "Jonas Berg", "Wei Zhang",
          "Hana Kowalski", "Diego Alvarez", "Ruth Mbeki", "Emil Larsen"]

VERBS = ["was archived", "was reconciled", "was superseded", "was audited",
         "was re-keyed", "was migrated", "was quarantined", "was released"]


def records(rng, n, prefix):
    out = []
    ids = rng.sample(range(1000, 9999), n)
    for i in range(n):
        rid = "%s-%04d" % (prefix, ids[i])
        out.append({
            "id": rid,
            "tag": rng.choice(TAGS),
            "value": rng.randint(10000, 99999),
            "owner": rng.choice(OWNERS),
            "verb": rng.choice(VERBS),
        })
    return out


def render(recs, title):
    lines = [title, ""]
    for r in recs:
        lines.append("Record %s: tag=%s, value=%d, owner=%s. The entry %s during the last cycle."
                     % (r["id"], r["tag"], r["value"], r["owner"], r["verb"]))
    lines.append("")
    return "\n".join(lines)


def question(rec, listname):
    return ("\n\nQuestion: In %s, record %s is listed. State its value and its owner, "
            "exactly as written.\nAnswer: Record %s has value "
            % (listname, rec["id"], rec["id"]))
