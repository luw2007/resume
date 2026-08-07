# Step 2 — Skim Feasibility Spike Report

**Date:** 2026-08-08
**Step:** v0.1.0 implementation plan, Step 2 (Embedded Skim feasibility gate)
**Decision gate result:** ✅ **PASS — keep Skim**

## Summary

The throwaway-capable spike at `src/picker.rs` (driven by `examples/resume-spike.rs`
and tested by `tests/picker_spike.rs`) proves Skim `0.17.3`'s **public library API**
meets every essential interaction model required by ADR 0001, without forking Skim
and without a second TUI framework. All twelve automated PTY tests and eleven unit
tests pass.

No code path depends on Ratatui, Crossterm, or Tokio directly; those appear only
transitively through Skim, as the ADR mandates.

## Requirements vs. evidence

| Requirement | How it is met | Test |
|---|---|---|
| Candidates streamed from a bounded channel while picker is open | `crossbeam::channel::bounded(32)` feeding `SkimItemReceiver`; producer thread sends items with cooperative backpressure, `drop(tx)` signals completion | `skim_streamed_path_still_selects` |
| `display` / `text` / `output` separation on a custom `SkimItem` | `SpikeItem` implements `display()` (sanitized), `text()` (searchable), `output()` (opaque `key:<N>` only), `preview()` (in-memory) | `spike_item_separates_display_text_output`, `skim_streams_candidates_and_selects_opaque_key` |
| Preview from in-memory key lookup with native scrolling | per-item `preview()` returns `ItemPreview::Text` looked up from a `HashMap<CandidateKey, String>`; Skim's own preview window provides scrolling (preview-up/down/etc. actions) | `preview_hidden_by_default_and_ctrl_o_toggles` |
| Preview hidden by default, `Ctrl+O` toggle | `preview_window = "right:60%:hidden"` + `bind "ctrl-o:toggle-preview"` | `preview_hidden_by_default_and_ctrl_o_toggles` |
| `Ctrl+R` Normalized/Raw switch | **Dual-section fallback** (accepted by ADR): preview always renders both normalized and raw sections. `Ctrl+R` is bound to `ignore` (see finding below) | `ctrl_r_does_not_scan_filesystem` |
| Opaque-identity selection stability while streaming | Selection returns the `Arc<dyn SkimItem>` whose `key` is an opaque `CandidateKey(u64)`; downcast recovers the same identity regardless of Skim's visible-row sort order | `skim_streams_candidates_and_selects_opaque_key` |
| Terminal restoration on Esc / Ctrl+C / empty / zero / resize / panic / normal | tuikit `clear_on_exit`; `Esc`→exit 0, `Ctrl+C`→exit 130; `catch_unwind` around `run_with` for the panic path | `esc_cancels_and_restores_terminal`, `ctrl_c_exits_130`, `zero_candidates_cancels_cleanly`, `resize_does_not_crash` |
| `/dev/tty` operation when stdin is redirected | Skim's `get_tty()` opens `/dev/tty` directly, independent of fd 0; verified by running the binary with `< /dev/null` | `works_with_redirected_stdin` |
| No-control-terminal / terminal < 60×10 fail before start | `preflight()` opens `/dev/tty` and reads `TIOCGWINSZ` before `run_with` | `tiny_terminal_fails_preflight`, `adequate_terminal_passes_preflight` |
| Candidate/preview content protected against ANSI/OSC/control-sequence execution | `sanitize_for_display` strips CSI/OSC/OSC-8/OSC-52/title/C0/C1/bidi; preview uses `ItemPreview::Text`, **never** `ItemPreview::Command` | `control_sequence_attacks_are_neutralized` + 8 sanitizer unit tests |

## Critical finding: `Ctrl+R` reload is unsafe for a channel-fed picker

Skim's `reload` action, when invoked with **no argument**, re-runs the *default
command* (`find` on the current working directory) even when items were supplied
via a channel (`SkimItemReceiver`). If `resume` bound `ctrl-r:reload`, pressing
Ctrl+R would replace the streamed Session list with a listing of real files from
the user's working directory — a correctness and privacy violation for a launcher
fed by integration workers.

Evidence: `skim-0.17.3/src/model/mod.rs:460` —
`act_reload(None)` falls back to `self.query.get_cmd()`, which returns the default
command (`SKIM_DEFAULT_COMMAND` or the built-in `find` equivalent).

**Resolution:** The spike binds `Ctrl+R` to `ignore` and relies on the ADR-accepted
**dual-section preview fallback**, which renders both normalized and raw
(terminal-safe) text in a single preview window. This needs no picker rebuild and
no command execution. The test `ctrl_r_does_not_scan_filesystem` proves no
filesystem entry leaks into the candidate list after Ctrl+R.

> Note: Skim's *default* `Ctrl+R` binding is `EvActRotateMode` (toggles
> fuzzy/exact matching), not reload — so even an unbound Ctrl+R is harmless. The
> explicit `ignore` binding documents the intent and guards against a future
> accidental `reload` binding.

## Documented behavior: selection stability while candidates stream

Skim sorts the **visible** matched results by rank, so the on-screen order of rows
may change as late candidates arrive. However, the selection contract is stable:
`SkimOutput::selected_items` returns the actual `Arc<dyn SkimItem>` objects, not
row indices. `resume` keys every item with an opaque `CandidateKey` and recovers
it via `as_any().downcast_ref::<SpikeItem>()`. No late-arriving candidate can make
a key point at a different Session. The global visible order is therefore
approximate during streaming — which Step 9 accounts for with a final reorder only
if the prototype proved stability (it did, for *identity*, not for visible order).

## What was deliberately NOT built (per the spike scope)

- The Active modal.
- Adapter logic / real integration readers.
- Polished row layout (`STATUS AGENT[PROFILE] UPDATED TITLE …`).
- The 64 MiB LRU preview cache (Step 3 owns it; the spike uses a plain `HashMap`).
- Confirmation, revalidation, and `exec`.

## How to reproduce

```bash
cargo test --test picker_spike        # 12 PTY tests
cargo test --lib picker::             # 11 sanitizer/item unit tests
cargo run --example resume-spike demo # manual smoke (needs a real TTY)
```

PTY tests can be disabled with `SPIKE_PTY_TESTS=0` in environments without a
usable pseudo-terminal.

## Recommendation

Proceed to Step 9 (production picker) using the proven interaction model. Carry
the `ctrl-r:ignore` binding and dual-section preview forward verbatim. Do not
introduce `reload` for a channel-fed picker under any circumstance.
