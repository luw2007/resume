# Provenance

This directory vendors `skim-tuikit` 0.6.6 from the `skim-rs/skim` project:
<https://github.com/skim-rs/skim/tree/v0.6.6>.

`resume` patches `src/input.rs` only. Upstream 0.6.6 parses xterm
`CSI 1;3{A,B,H,F}` modifier sequences but omits `CSI 1;3C` and `CSI 1;3D`.
The two added parser arms map those sequences to `AltRight` and `AltLeft` so
Resume's documented picker tab-navigation bindings work in macOS terminals.

The vendored code remains under the upstream MIT license in [LICENSE](LICENSE).
