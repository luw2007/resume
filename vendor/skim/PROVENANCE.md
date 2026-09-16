# Provenance

This directory vendors `skim` 0.17.3 from the `skim-rs/skim` project:
<https://github.com/skim-rs/skim/tree/v0.17.3>.

`resume`'s picker renders a title, optional first-message preview, metadata,
and an empty separator. Upstream `Selection` (`src/selection.rs`) draws one
screen row per `SkimItem`. This vendored copy patches:

- `src/lib.rs`: adds `SkimItem::display_rows` (defaulting to `display()`) and
  `display_height` so each item reports its physical row count.
- `src/selection.rs`: calculates the visible window and row offsets from item
  heights, including navigation, paging, mouse selection, and drawing. It
  prints up to three content rows per item; one-row items retain upstream's
  highlight-aware path. Multi-row cards print rows independently.

Other upstream files only gain explicit elided lifetimes (`'_`) to silence
`mismatched_lifetime_syntaxes` warnings with current Rust compilers.

The vendored code remains under the upstream MIT license in [LICENSE](LICENSE).
