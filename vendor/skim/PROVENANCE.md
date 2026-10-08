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
- `src/options.rs`, `src/header.rs`, `src/model/mod.rs`: optional fixed footer
  below the selection in reverse layout, for the picker shortcut hints.
- `src/options.rs`, `src/model/mod.rs`: optional double-space modal Preview
  overlay (opens only with a focused item; the trigger spaces are removed from
  the query). `q`, `Esc` or `Enter` dismiss it (Enter never accepts), and
  dismissal forces a side-preview refresh; arrows/`hjkl`, `PgUp`/`PgDn` and
  `Ctrl-U`/`Ctrl-D` scroll it; `Ctrl-C` falls through to the normal abort path.
  Only enabled by `resume`'s tabbed picker.
- `src/previewer.rs`: `Previewer::scroll_half_page` for the modal's
  `Ctrl-U`/`Ctrl-D`.
- `src/output.rs`, `src/tmux.rs`, `src/model/mod.rs`: `SkimOutput::preview_visible`
  reports whether the side Preview pane was shown at exit, so the picker can
  keep it across tab switches.
- `src/selection.rs`: mouse row selection moves the cursor in the correct
  direction for non-reverse layouts too.
- `src/lib.rs`, `src/model/mod.rs`: `SkimItem::selectable` (default `true`);
  Enter or double-click on an item that returns `false` is ignored in place.
  Keys bound as `accept` purely for navigation (Tab, Shift-Tab, Alt-Left,
  Alt-Right, Ctrl-L) still end the run.

Other upstream files only gain explicit elided lifetimes (`'_`) to silence
`mismatched_lifetime_syntaxes` warnings with current Rust compilers.

The vendored code remains under the upstream MIT license in [LICENSE](LICENSE).
