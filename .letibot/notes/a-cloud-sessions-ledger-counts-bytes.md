<!-- abstract: A metered session's ledger counts one id per byte (Vocab::bytes), so it over-counts a provider ~3.4x — measured on the session that was switched to glm-coding and refused at round 0; images are NOT the overcount, and the vendored catalogue has no modalities. -->

# A cloud session's ledger counts BYTES, and that is the 3-5x overcount — MEASURED 2026-10-25

`Vocab::bytes` (crates/tokencore/src/vocab.rs:39-45) gives ids 0–255 to the bytes of the
rendered text, and it is what a session whose turns go to a cloud provider tokenizes with.
So the ledger's "tokens" are the BYTES of the dialect rendering — chatml markup, control
tokens and all — not a tokenizer's count.

Measured on the operator's `s-1791394076177451000`, `stable_prefix.vocab_source =
bytes:f1de219e8ed9`:

```text
session row      context_tokens 792,991   context_ledger 2,704,409   ratio 3.41
transcript #t13  1,743 rows, sum(tok_len) 2,595,965
                 of which reasoning 1,050,349 · tool_result 801,286 · assistant 705,899
the text the request carries  ~2,012,190 chars (reasoning 937,954 + payload 786,744
                               + assistant 250,927 + user 27,269 + system 9,296)
```

~1 ledger id per byte of text plus the markup, against the provider's ~2.5 chars/token. The
`edit` before/after (253,645 chars in `item_json`) is not rendered, and is not counted.

**Images are NOT the overcount, which is the question worth writing down.** Fourteen
`ToolResult.media` rows carry 1,995,136 base64 chars between them and each ledger row is
**228–243** tokens: `render_user_parts`/`ToolResult` emit `VISION_START`/`IMAGE_PAD`/
`VISION_END` where the part sat and never the `data:…` URI (dialect-qwen/src/render.rs:461,
385-389). Had the base64 been tokenized those rows would have been ~350k each.

**The fix is not in the ledger.** Its ids are the hash chain and `transcript_item.tokens`,
whose `tok_len` a store trigger pins to the blob (`tok_len <> LENGTH(tokens)/4` aborts), so
there is no in-place re-count. The designed remedy is the measured ratio, and what was
broken was that a switch CLEARED it: `Config::carried_scale` now keeps the last pair, named
after the model that measured it, until a round on the new model measures again.

# The vendored catalogue carries no `modalities`, so every model is "unknown"

`scripts/refresh-models-snapshot.sh` filtered `limit` and `cost` only until 2026-10-25, so
`crates/provider/data/models-snapshot.json` has no `modalities` — and a box with no
`~/.cache/opencode/models.json` and no `~/.cache/letibot/models.json` (this one) reads that
snapshot and nothing else. `Catalogue::takes_images` therefore answers `None` for every
preset model here, and `messages::Eyes::Unknown` is what runs: images become the
`[image N · mime · size — not sent: …]` placeholder. The parser and the script both carry
`modalities` now; the DATA does not until somebody with network runs the script.

# The store does not record WHICH model measured the pair

`session.context_tokens`/`context_ledger` survive a restart and are recovered as a scale,
but nothing says which model produced them. A daemon restarted after a switch to another
provider recovers the OLD model's pair and labels it as the new one's measurement — the
same numbers, said as measured rather than as an estimate. `CarriedScale` says it
correctly only for switches made inside one daemon life (`set_provider`).
