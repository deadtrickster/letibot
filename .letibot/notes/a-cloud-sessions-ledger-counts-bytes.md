<!-- abstract: A metered session's ledger counts one id per byte of the rendered text, so it over-counts a cloud provider about 3.4x; images are not the cause, and the remedy is a carried ratio rather than a re-count. -->

# A cloud session's ledger counts BYTES, and that is the 3-5x overcount

`Vocab::bytes` (`crates/tokencore/src/vocab.rs`) gives ids 0-255 to the bytes of the rendered text, and
that is what a session whose turns go to a cloud provider tokenizes with. So the ledger's "tokens" are
the bytes of the dialect rendering, markup and control tokens included: on one real session,
`context_ledger` 2,704,409 against `context_tokens` 792,991, a ratio of 3.41, at about 2.5 chars per
provider token.

Images are NOT the overcount. Fourteen `ToolResult.media` rows carrying 1,995,136 base64 chars cost
228-243 ledger ids each, because the renderer emits `VISION_START`/`IMAGE_PAD`/`VISION_END` where the
part sat and never the `data:` URI.

The ledger cannot be re-counted in place: its ids are the hash chain, and a store trigger pins
`tok_len` to the blob. The remedy is the measured ratio, and what was broken was that a provider
switch CLEARED it - `Config::carried_scale` now keeps the last pair, named after the model that
measured it, until a round on the new model measures again.
