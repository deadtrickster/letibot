<!-- abstract: MEASURED 2026-11-01: "collapsed notifications" are R89's one-event-one-row pairings (`App::absorbed`), and the state was lost three ways — `is_absorbed` refused a RETIRED note (so ctrl-n swapped a one-line card for the row it had replaced), `App::load` and `App::rows_replaced` CLEARED the map (so a /resync, a reattach or a switch back un-collapsed every notice), and `mark_seeded` wrote `retired` to head.toml without merging (so a second head's seed wiped the file that remembers). -->

# A collapsed notice is a pairing, and three things dropped it

MEASURED 2026-11-01, branch `agent/the-notices-stay-collapsed`, from the operator's *"btw after
certain evens all collapsed notifications reappear"*.

## What "collapsed" turned out to be

**R89's one-event-one-row pairing**, `crates/tui/src/app/one_event.rs`: a session row that says the
same event as a hub warning — `[reseat]`, `[restart]`, `EMERGENCY — <target>`, the stale-prompt
sentence — is **absorbed** into the warning's one-line card. That is what "collapsed" means here;
the code has no `collapse`/`fold` word for it, which is why grepping the head found nothing.

The state: `App::absorbed`, and it was `HashMap<usize /*row index*/, String /*note key*/>`.

## The three ways it came back, all measured

| # | what | why |
|---|---|---|
| 1 | **the reader's `ctrl-n`** | `is_absorbed` asked `!dismissed.contains(key)` — *a retired note gives its rows back* — so a key whose whole promise is *hide this* put the row on the screen in the card's place |
| 2 | **any snapshot** (`/resync`, a reattach, a switch back) | `App::load` did `absorbed.clear()`; the pairing is re-made only from a note at a `Placed::Seam` within `WINDOW` rows, and a snapshot's warnings are `Placed::Before` — never paired |
| 3 | **a second head's starter-todo seed** | `mark_seeded` calls `prefs::save` with a whole `HeadPrefs` and `prefs()` carries THIS head's `dismissed`: a head that retired nothing wrote an empty `retired` over another head's, so the *file* that remembers forgot. (Every other write goes through `save_prefs`' `Union`; the seed merges `todo_seed` and not `retired`.) |

Measured on the tree, `cargo test -p letibot-tui --lib onevent`, 4 arms, `(row drawn, card rows,
notice rows)`:

| event | before | after |
|---|---|---|
| a reseat, watched live | (0, 1, 0) | (0, 1, 0) |
| … then `ctrl-n` | **(0, 0, 1)** | (0, 0, 0) |
| … then a resync | (0, 0, 1) | (0, 0, 0) |
| … then a switch away and back | (0, 0, 1) | (0, 0, 0) |
| a switch away and back, nothing retired | (0, 0, 1) | (0, 0, 1) — right |
| a head that only ever had the snapshot | (0, 0, 1) | (0, 0, 1) — right |

## The rule that survived, and the one that had to stay

A row is collapsed while **its note's card is on the screen** (`Placed::Seam`) **or the reader
retired that note**. A note that is held but neither drawn nor retired — a snapshot's warning,
history by R19 — still gives its rows back: the row is then the only thing that says the event
happened. That asymmetry is the whole design and it is why "keep the pairing across a switch"
cannot be done by keeping it unconditionally.

## Rakes

* **`absorbed_texts` sorted row INDICES** to get transcript order. Keying the map by `item_id`
  makes that sort meaningless (it would be alphabetical), so the order has to come from the
  position in `items` — the popup's "newest first" was silently at risk.
* **`pair_one_event`'s "one row of each kind per warning"** looked rows up by index in
  `self.absorbed`; with ids it must find the row by id.
* **A probe that files the live warning with `env(seq, …)` and the snapshot's from `hub.publish`
  compares two different `ts`s**, so `note_key` differs and the pairing "fails" for a reason that
  does not exist in production. Feed the head the envelope the hub published.
* **The file-side bug needs two heads up at once**: a head that starts *after* the dismissal loads
  it from the file and keeps it. The fixture must create head B, load its prefs, *then* have head A
  dismiss.

## The fourth door, NOT closed — a fork (a compaction, a `/reseat`)

MEASURED with a real fork in the fixture: after `TranscriptForked`, `rows_replaced` sets
`kept = items.position(first row of the OLD transcript)` — which is 0 when the head holds nothing
of the new transcript yet — and demotes every seam `> kept` to `Placed::Before`. A `Before` note is
not drawn, and (by the rule above) does not collapse its rows, so **a fork un-collapses every
notice and takes every card off the screen**: `(row, card, notice)` went `(0,1,0)` → `(0,0,2)`.

That is deliberate on both counts — the seam names a place in a transcript that is gone (pg-noop,
2026-10-16: a stale seam drew nothing over the operator's next message), and the wire does not say
where a carried row landed. An `item_id` is `{transcript}.{n}`, so a carried row is a NEW id and the
pairing cannot cross by identity. Closing it needs the fork to name where the rows it carried went;
nothing in the head can guess it, and a content match is not safe (`same_event` answers *true* for
any `[reseat]`/`[restart]` row — there is nothing in the text to compare).
