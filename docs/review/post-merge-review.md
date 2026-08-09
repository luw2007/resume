# Post-Merge Review — `0be9051..HEAD` (e08fafb)

**Scope:** the full merge of five parallel branches back into `main`:
`cli-help-layers` (c13535c), `packaging-release-ux` (f0d31d6), `pi-claude-module-split` (7f8df67),
`active-detection-codex` (820d66b), `active-detection-omp` (901bb94), plus the six merge commits
430445b / d28cb55 / 4ddcbf3 / 86c84cc / b777334 / e08fafb.

**Review method:** six independent read-only lanes (app.rs merge mechanics; integration facade
export parity; README-vs-code truth check; mechanical merge residue and lost tests; errors.rs
vs diagnostics coherence; design-doc vs merged-code divergence). Findings below are
cross-checked and de-duplicated across lanes. Several were reproduced against a built binary.

**This document changes no `.rs`, `.toml`, or `.yml` file.**

---

## 0. Verdict

The merge is **mechanically excellent and semantically leaky**.

Mechanically it is close to flawless: `cargo check`, `cargo clippy --all-targets --all-features
-- -D warnings`, `cargo fmt --check` and the full test suite are all green on default,
`--all-features` and `--no-default-features`. **Zero warnings. Zero lost tests** — all 373 test
names present at the fork point survive at HEAD, plus 31 new ones. No conflict markers, no
orphaned files, no stale `crate::jsonl`-style import survived the `preview/` move in compiled code.

The problems are all at the **seams between branches** — precisely where no single branch's own
test suite could see them. Five branches each self-verified in isolation; nothing verified the
union. Every MUST-FIX below is a place where branch A's guarantee is silently voided by branch B's
feature.

| Severity | Count | Gate on merge? |
|---|---|---|
| BLOCKER | 0 | — |
| MUST-FIX | 6 | yes — fix before release |
| SHOULD-FIX | 14 | no — schedule as follow-ups |
| NIT / stale docs | 20+ | opportunistic |

---

## 1. MUST-FIX

### M1 — `RESUME_DISABLE_PROC_PROBE` does not disable the Codex probe

**Where:** `src/integration/codex/activity.rs:107` (missing guard); switch implemented only at
`src/proc.rs:250`; called unconditionally from `src/app.rs:82-87`.

**Problem.** The kill switch was added by the OMP branch and lives inside `crate::proc::snapshot()`,
which only feeds OMP/Pi. The Codex branch added a wholly separate `lsof` probe with no env check.
Neither branch could see the other.

**Reproduced.** With a stub `lsof` on `PATH`:
`RESUME_DISABLE_PROC_PROBE=1 resume --json -a codex` still emits
`"activity":"Active { observed_at: ... }"` and `--list` still prints `ACTIVE`.
The same var with `-a omp` correctly suppresses `ps`.

**Why it is a problem.**
1. `README.md:155` states the variable disables process probing so activity falls back to
   `Unknown`. That sentence is false for Codex.
2. `tests/step9_app.rs:48-52` sets the var with a comment claiming it prevents inheriting a host
   `lsof`. Hermeticity there survives only accidentally, via `.env_clear()` plus a redirected `PATH`.
   `tests/picker_spike.rs:42` does **not** `env_clear` and inherits the real `PATH` — on a developer
   machine with Codex running, a fixture row can flip `READY` → `ACTIVE`.
3. That flip is not cosmetic: `src/session.rs:99-104` sorts Active first, and `src/launch.rs:145`
   makes Active a mandatory-confirm risk. Ordering and prompting both change. The flake will present
   as a picker bug.

**Fix.** Add the same guard at the top of `codex::activity::probe()`:

```rust
if crate::proc::proc_probe_disabled(std::env::var_os(crate::proc::DISABLE_PROC_PROBE_ENV).as_deref()) {
    return (ActivitySnapshot::empty(), Vec::new());
}
```

This requires widening `proc_probe_disabled` (currently private, `src/proc.rs:215`) to
`pub(crate)`. Alternatively gate once inside `DiscoveryContext::probe` so the whole context honours
a single check. Then correct the now-true comment at `tests/step9_app.rs:48-49`, and add a
regression test asserting a fake `lsof` early on `PATH` is never invoked when the var is set.

---

### M2 — non-verbose `--list` silently swallows every discovery diagnostic

**Where:** `src/app.rs:127-131` (the `else` branch).

**Problem.** Pre-merge, diagnostics printed unconditionally before both output forms
(`git show 0be9051:src/app.rs:84`). The packaging branch moved the `--list` call inside
`if options.verbose`. Non-verbose `--list` now drops `codex_root_unavailable`,
`omp_discovery_failed`, `pi_skipped`, `git_scope_discovery_failed`, `codex_sqlite_id_mismatch` —
all of them.

**Reproduced.** `resume --list -a codex` with no `$HOME`:
stdout `No Sessions found in Scope.`, **stderr empty**, **exit 1**. A nonzero exit with zero
explanation.

**Why it is a problem.** It contradicts `src/man.rs:442-449`, which says failures are "collapsed by
category and printed as `resume: CATEGORY: COUNT`" with no scoping to `--json` or `--verbose`, and
`src/man.rs:502`/`:516`, which say the codes "appear only as a diagnostic" when the run continues.
It also inverts the packaging branch's own commit intent — verbosity was meant to control *detail*,
not *existence*. It is a behavioral regression against `0be9051`, not a refactor. No test covers it:
`tests/step9_app.rs` only asserts stderr on `--json` runs (`:226`, `:262`, `:348`).

**Fix.** Drop the `if options.verbose` guard; always call `print_diagnostics(&state, options.verbose)`
*after* `print_list(&records)`. Keeping it after preserves the branch's real goal (the friendly empty
message leads stdout) while restoring the contract. Add a test asserting `--list -a codex` against a
corrupt-rollout fixture emits `codex_invalid_session` on stderr without `--verbose`.

---

### M3 — OMP `Active` has no staleness gate; the top-ranked risk in its own design doc is unmitigated

**Where:** `src/integration/omp/activity.rs:165-188` (`correlate_live_with`).

**Problem.** `ActivityEvidence { live_process: true, breadcrumb_alive: true, .. }` is emitted
whenever (a) any `omp` process occupies TTY *T*, (b) a readable breadcrumb exists at `<dir>/T`, and
(c) the named path `is_file()`. `docs/research/session-formats.md:127` records that OMP **leaves
breadcrumbs in place on exit** and **stores no PID**. TTY device names are recycled. So: yesterday's
`omp` on `ttys003` leaves a breadcrumb pointing at yesterday's transcript; today a different `omp`
takes `ttys003`; all three implemented conditions hold and the **wrong session reports Active**.

Note `src/integration/omp/activity.rs:175`'s `!breadcrumbs.is_alive(&tty)` is a tautology — the
default impl at `:57-59` is `session_for_tty(tty).is_some()`, already proven `Some` at `:172`. The
gate is not weakened; it is absent.

**Why it is a problem.** `src/launch.rs` `risk_reasons` pushes "Session is Active", and
`should_confirm` returns `risky || (confirm_always && !no_confirm)` — `risky` short-circuits
`no_confirm`. A scripted `resume --no-confirm` then blocks on `io::stdin()`. The reconciled design
doc calls exactly this "the only outcome that makes this feature worse than not shipping it."

The designated mitigation — `breadcrumb.recorded_at >= process.started_at`, plus a 12h
`BREADCRUMB_FRESHNESS` fallback — is entirely absent. `ProcEntry::elapsed` is parsed at
`src/proc.rs:160,190` and read by **nothing**: the discriminator is computed and thrown away. The
plan's other layered mitigation (`NullBreadcrumbs`, making Active structurally impossible in the
first PR) is also gone, so nothing stands in front of this.

**Fix.** These land as one change; splitting them leaves a gate that depends on data nobody records:
- add `started_at` to `ProcEntry` (`observed_at - elapsed`, already computable);
- add `ProcessTable::live_on_tty(command, tty) -> Option<&ProcEntry>` so the matching process's
  start time is recoverable;
- extend `BreadcrumbSource` to return `recorded_at` (breadcrumb file mtime suffices — the format is
  bare text per `session-formats.md:127`);
- gate on `recorded_at >= started_at`.

**Caveat.** This is a high-confidence reading of `correlate_live_with` against
`session-formats.md:127`, not a live reproduction. Reproduce with a recycled TTY before designing
the fix.

---

### M4 — store present-but-unreadable is a true silent failure that `E3001` claims to cover

**Where:** `src/integration/codex/discover.rs:221` (`Err(_) => return`) and
`src/integration/claude/discover.rs:82` (`Err(_) => return candidates`).

**Problem.** `E3001`'s own Trigger text (`src/errors.rs:143`) says the store "cannot be read with the
current permissions". Neither call site distinguishes `PermissionDenied` from `NotFound`; both
swallow the `read_dir` error entirely.

**Reproduced.** `chmod 000` on both stores → `{"sessions":[],"errors":[git_scope only]}`, **exit 0**,
silent.

**Why it is a problem.** This presents a permissions problem — potential data loss — as an ordinary
empty result, invisible to both humans and scripts. It is the worst silent failure in the audit. Pi
handles this correctly (`iter_session_files` propagates), which is why pi and codex/claude diverge.

**Fix.** Match on `io::ErrorKind` at both sites; emit `{codex,claude}_root_unavailable` (or a new
`*_root_unreadable`) for anything that is not `NotFound`.

---

### M5 — `--json` emits un-aggregated `errors[]`, contradicting its own published contract

**Where:** `src/app.rs:825-832` (`print_json`) vs `src/app.rs:859-861` (`print_diagnostics`).

**Problem.** `print_diagnostics` runs `aggregate_diagnostics` first; `print_json` iterates
`state.errors` raw.

**Reproduced**, with three malformed Claude transcripts:

```
stderr:  resume: claude_no_session_id: 3
stdout:  "errors":[…,{"category":"claude_no_session_id","count":1}×3]
```

**Why it is a problem.** It breaks `src/man.rs:447` ("with the **same** category and count appearing
in `--json`") and `src/man.rs:324` ("how many individual failures **rolled up** into this category").
A consumer running `.errors[] | select(.category==X) | .count` gets `1` instead of `3`, and gets
duplicate keys building a map. This is the *only* interface the man page declares stable.

**Fix.** Have `print_json` call `aggregate_diagnostics(&errors_guard, false)` before mapping to
`JsonError`. One line, no schema change. Add a test asserting stderr counts equal JSON counts. No
in-tree test asserts the buggy shape (`tests/step9_app.rs` only uses `.any(category == …)`), so this
is safe in-tree.

---

### M6 — checked-in shell completions still contain the exact defect their design doc was written to fix

**Where:** `completions/_resume:152-153`, plus all three checked-in completion scripts.

**Problem.** `docs/design/cli-help-layers-plan.md` §1.1 opens by citing `completions/_resume:152-153`
— `'config:' \` / `'completions:' \` with empty descriptions — as the verified defect motivating the
whole branch. At HEAD those lines are unchanged. `--man` appears in none of the three scripts. §10
Step 3 lists "regenerate the checked-in completion files" as a gate; it was skipped.

**Why it is a problem.** `docs/completions.md:51` documents these files as "generated from the same
Clap command definition" — now false. Regenerating produces a materially different file for bash,
zsh and fish. §4.1's stated rationale for not regenerating ("keeping `about` byte-identical means the
checked-in file does not churn") is falsified by the branch's own L1 work, which churns it anyway.
There is direct precedent for treating this as a defect: d1f6611 "fix: regenerate stale completion
scripts to include --since flag". This is a shipped-artifact mismatch, not doc drift.

**Fix.** Re-run the three commands at `docs/completions.md:54-56` and commit. Independent of
everything else; land immediately.

---

## 2. SHOULD-FIX

### Correctness and robustness

**S1 — Codex `lsof` probe has no wall-clock timeout and sits on the critical path.**
`src/app.rs:84` → `src/integration/codex/activity.rs:132` uses `Command::output()` with no deadline.
`-S 2` (`activity.rs:24,172`) caps each kernel `stat`/`readlink`, not total runtime. The sibling
`crate::proc::snapshot()` *does* enforce `PROC_PROBE_BUDGET = 300 ms` (`src/proc.rs:19,274-284`).
`DiscoveryContext::probe` runs at `src/app.rs:119`, before any worker thread exists, so it is covered
by neither the `CancelToken` (`:388`) nor `JOIN_BUDGET` (`:290`). A wedged probe delays picker
startup with no cap and no interrupt. This is risk R4 in the OMP plan, mitigated for `ps` and never
adopted for `lsof`. *Fix:* reuse the spawn + deadline pattern at `src/proc.rs:264-297`; on expiry
return `ActivitySnapshot::empty()` plus a `codex_activity_probe_timeout` diagnostic — the
`Vec<Diagnostic>` return channel already exists and is already plumbed to both output paths.

**S2 — `omp_discovery_failed` does not affect the exit code; `pi_discovery_failed` does.**
`discover_pi` returns `AgentDiscovery::failed(...)` (`src/app.rs:472`); `discover_omp` pushes the
diagnostic and still returns `AgentDiscovery::ok(...)` (`:598`, `:605`). `discovery_exit` (`:940`)
keys on `successful_integrations`. Verified: `-a pi` with an unreadable root → exit 1; `-a omp` with
an unreadable root → exit 0. Same condition, opposite process semantics. *Fix:* track `any_root_ok`
across the multi-root loop and set `integration_ok` accordingly — for a multi-root agent
`integration_ok` should mean "at least one root scanned successfully", not a blanket `failed()`.

**S3 — the process probe degrades completely silently.**
Every path in `src/proc.rs:249-301` — spawn error (`:267`), non-zero exit (`:286`), budget expiry
(`:286`), non-UTF-8 (`:293`) — returns `Ok(ProcessTable::empty())` with no `Diagnostic`.
`src/app.rs:78`'s `.unwrap_or_else(|_| empty())` then makes the `Err` arm both unreachable and
uninstrumented, so `snapshot()`'s `io::Result` return type is a lie. The reconciled OMP plan mandates
`proc_probe_failed` / `proc_probe_timeout` — agent-neutral precisely so Pi can reuse the probe.
Neither token exists. OMP reports every session `Unknown` with no signal even under `--verbose`,
indistinguishable from "no live sessions". *Fix:* emit both tokens into `DiscoveryState.errors`;
while there, switch the timeout from poll-and-kill to `recv_timeout` + detach, which is what
`src/runtime.rs:7-9` states as the crate's philosophy.

**S4 — Codex `by_identity` is unreachable without a basename match.**
`src/integration/codex/activity.rs:62-72` makes a `by_name` hit mandatory (`self.by_name.get(...)?`)
before `by_identity` is consulted, reversing design §3's ordering. The doc explicitly calls out
"hard links to the same rollout also resolve correctly (same inode) — desirable"; that case now
returns `Unknown` whenever basenames differ. Evidence-losing only, never fabricating, so not a
MUST-FIX — but it silently drops a documented capability, and the merged test
`lookup_matches_each_duplicate_identity_candidate` (`:379-408`) only covers same-basename hard links.
*Fix:* restore the independent identity step, or amend §3 to declare basename a mandatory prefilter.
Verify with a differing-basename hard-link fixture first.

**S5 — `breadcrumb_directory` is a fourth, undesigned roots-resolution order.**
`src/integration/omp/activity.rs:84-102` resolves the breadcrumb store via `XDG_STATE_HOME` plus an
`is_dir()` existence probe, gated on a *global* `agent_dir_overridden` bool applied identically to
`Default` and `Named` profiles. `src/integration/omp/roots.rs:200-224` and invariant (a) state that
`PI_CODING_AGENT_DIR` "overrides only the unprofiled agent root", with named-profile isolation
enforced by omission — and `roots.rs:218` even carries a `// DO NOT unify these branches` marker.
The activity path reintroduces exactly the coupling that marker forbids. It has tests
(`omp/tests/activity.rs:66-100`) pinning the behavior but no doc adjudicating it. *Fix:* record an
explicit decision, then either align with `agent_root` or document a deliberate exception.

### Error-model coherence

**S6 — `claude_missing_workspace` is documented but unreachable.**
`src/app.rs:493` does `claude::resume_spec(&session, &root).ok()`, discarding the
`IntegrationError::InvalidSession { diagnostic }` built at `src/integration/claude/resume.rs:16`. The
token is documented as an `E3003` category at `src/man.rs:530` and can never appear in any shipped
run. *Fix:* thread the diagnostic into `AgentDiscovery::errors` — it is exactly the "why can't I
resume this?" signal a user wants. (Preferred over deletion.)

**S7 — `io_error` is declared and documented but never constructed.**
`src/errors.rs:82`, `src/man.rs:540`. Its only appearance in `src/` is a unit-test fixture
(`src/app.rs:1093`). *Fix:* delete from both. Shipping documented-but-impossible tokens erodes trust
in the whole ERRORS section.

**S8 — `claude_discovery_failed` arm is unreachable.**
`src/app.rs:499`. `claude::discover` (`src/integration/claude/discover.rs:42-69`) has no `?` and no
`return Err`; it is structurally infallible. *Fix:* either drop the `Result` from its signature and
the token, or make it genuinely fallible on the M4 permission case — which kills two birds.

**S9 — two active-detection tokens are undocumented.**
`codex_activity_probe_failed` and `codex_activity_probe_partial`
(`src/integration/codex/activity.rs:24-25,138,161`) are live and reachable but appear in neither
`errors::category` nor the man page's exhaustive-sounding list at `src/man.rs:536-541`. The list
reads as complete; it is not. *Fix:* add both.

**S10 — the catalog↔man-page bridge is hand-copied prose with no enforcement.**
`ErrorSpec::categories` / `for_category()` / `errors::category::*` have **zero** runtime call sites
outside `src/errors.rs`. `src/man.rs:433-546` restates the mapping by hand. The "single source of
truth" claim (`src/errors.rs:3-6`) is half true: the four-line stderr block genuinely derives from
the catalog, and all 28 title/trigger/fix/example strings are currently present verbatim — but
because someone typed them twice, not because they are generated. `src/errors.rs:60-62` further
claims the consts are shared with "the integration call sites"; no integration uses them.
`man.rs`'s only guard (`errors_section_matches_the_catalog`, `:627`) checks codes and slugs, not
trigger/fix/example/categories — exactly the fields most likely to be reworded.
*Fix (cheap ratchet, high value):* extend that test to assert `page()` contains each spec's
`trigger`, `fix`, `example`, and every token in `categories`. **All of these assertions pass at
HEAD**, so this is a pure ratchet with no fixups required. This is the test the cli-help design doc
§8.2 singled out as "the one that keeps §6 and §7 from drifting apart."

### Export surface (omp only — pi, claude, codex are clean)

The omp split changed four public items in what its own design doc declared a parity-gated pure
move. `docs/design/omp-active-detection-plan.md` states the split PR "must reproduce the pre-split
public surface **exactly**, including `was_live_growing` and `is_user_attributed_pub`", with all four
dispositions assigned to a later cleanup PR under the standing rule "do none of this in the split
PR." Nothing is broken at runtime; this is a process and consistency problem.

**S11 — `omp::ENV_SESSION_DIR` renamed to `FLAG_SESSION_DIR`, and to the wrong name.**
`src/integration/omp/roots.rs:13` (was `omp/mod.rs:88`). A `pub const` on a lib target vanished with
no deprecation. The plan's D3 specified **`SESSION_DIR_FLAG`** ("noun-then-role… not inventing a
`FLAG_*` namespace with one member") *in the cleanup PR*. HEAD shipped the wrong name in the wrong
PR. Worse, the doc comment at `roots.rs:12` still reads "Environment variable overriding the session
root" for what is a CLI-flag token — the exact misnaming D3 existed to kill — while the genuine
`pi::ENV_SESSION_DIR` (`src/integration/pi.rs:41`) still coexists. *Fix:* rename to
`SESSION_DIR_FLAG` and correct the doc comment, or revert and defer.

**S12 — `was_live_growing` narrowed in omp but left `pub` in pi, manufacturing the asymmetry the plan
forbade.** `src/integration/omp/discover.rs:161-163` is now `pub(super)` + `#[allow(dead_code)]`
while the identical predicate at `src/integration/pi/resume.rs:100` remains `pub` and glob-exported
via `pi.rs:35`. D4 says "deleting one and keeping the other would manufacture an omp/pi asymmetry
that the next reader has to explain." The half-move was made. Independently confirmed dead: removing
the `#[allow]` produces `warning: function 'was_live_growing' is never used`. *Fix:* pick one
disposition and apply it to both.

**S13 — `omp::ResolutionInputs::with_profile_flag` deleted; `is_user_attributed_pub` deleted.**
`with_profile_flag` (was `omp/mod.rs:195-198`) is absent from `roots.rs:98-123`; runtime is
unaffected because `src/app.rs:570` sets `profile_flag` by direct field access. But the sibling
builder `with_session_dir_flag` survived at `roots.rs:118` with an `#[allow(dead_code)]` band-aid, so
the impl block is half-cleaned: one dead builder deleted, one annotated — while pi keeps **both**
un-annotated. `is_user_attributed_pub` (was `omp/mod.rs:918`) is gone and the underlying fn is now
fully private at `format.rs:251`; the plan's stated fallback (`pub(super)`) was not applied. *Fix:*
restore both for the split PR and do all four builder deletions together later, or ratify in place
and record in `CHANGELOG.md`.

**S14 — the split introduced 6 new rustdoc broken-intra-doc-link warnings** (lib-doc warnings 4 →
9): `src/integration/claude/mod.rs:32`, `omp/mod.rs:7`, `omp/roots.rs:42`, `pi.rs:26`, `pi.rs:27`,
`pi/discover.rs:41`, `pi/discover.rs:43`. Module docs moved into submodules where `Session`,
`SessionKey`, `ActivityStatus`, `ResumeSpec` are no longer imported. Not caught because
`.github/workflows/ci.yml:65` gates clippy but there is no `cargo doc` step and no
`RUSTDOCFLAGS=-D warnings`. *Fix:* import or fully-qualify, and add a `cargo doc --no-deps -D
warnings` CI step so this class stops regressing invisibly.

---

## 3. README and man-page accuracy

The Support List is largely accurate — Pi/Claude/Codex/OMP discovery, preview, exact-resume, the
native-resume boundary table, config precedence, the JSON envelope, picker keybindings, and the
positive-evidence-only invariant were all verified against source and hold. The following do not.

**R1 (MUST-FIX, ties to M1) — `README.md:155`.** "Set `RESUME_DISABLE_PROC_PROBE` to any value to
disable process probing…" — false for Codex. Best fixed in **code** (M1), which makes the sentence
true and shrinks the doc diff. If fixed in text instead: "…disables the OMP/Pi process-table probe
(`ps`). It does **not** disable the Codex `lsof` probe."

**R2 (MUST-FIX) — `README.md:146,152`.** "Codex Sessions report `Active` only when one process-wide
`lsof` probe finds a live Codex process holding the exact rollout file open." Simultaneously
over-claims and under-describes. `src/integration/codex/activity.rs:109` gates on
`command_available("lsof")`; on macOS without `lsof` the probe returns empty **with no diagnostic**
(`:121-125`) — reproduced as permanent `Unknown`. `lsof` is an undeclared runtime dependency absent
from the Install section (`README.md:11-15`). Meanwhile on Linux `:112-119` falls back to a `/proc`
fd walk, so "only … `lsof`" is wrong there. *Fix:* state both mechanisms, both platforms, and the
missing-`lsof` caveat; add `lsof` to Install as an optional runtime dependency. Consider emitting a
`codex_activity_probe_unavailable` diagnostic so an operator can distinguish "no active session" from
"probe unavailable".

**R3 (MUST-FIX) — `src/man.rs:100-101`.** "When `--since` is active, Sessions whose activity time is
unknown are excluded." The implementation states the exact opposite: `src/app.rs:429-436`
`session_at_or_after` returns `Err(_) => true`, and the doc comment at `:420-428` says
"conservatively kept rather than silently dropped."

**R4 (SHOULD-FIX) — `README.md:128`.** "…compares against each Session's best-available activity
signal." `src/app.rs:429-436` uses **only** `fs::metadata(path).modified()`. `session.activity` — now
genuinely `Active { observed_at }` for Codex/OMP — is never consulted. Vacuously harmless pre-merge;
the merge made it stale.

**R5 (SHOULD-FIX) — Privacy section, `README.md:5,157-166`.** "It never performs a machine-wide
scan" / "no machine-wide scan". Still true of *filesystem* scanning, now materially incomplete as a
privacy claim: `src/proc.rs:262-264` spawns `ps -A` (every process on the host); `:290` scans `/dev`
and `/dev/pts`; `src/integration/codex/activity.rs:176` runs `lsof -n -P -w -S 2 -F0pcfnDi -c codex`;
`:189-235` walks all of `/proc` on Linux reading every PID's `comm`. *Fix:* qualify both statements
as "machine-wide **filesystem** scan" and add a bullet describing the bounded read-only OS probes.
**This one deserves an explicit product decision** — whether "no machine-wide scan" was a load-bearing
promise the probes now violate in spirit, or whether the filesystem-only reading was always the
intended contract.

**R6 (SHOULD-FIX) — `README.md:150` contradicts `README.md:146`.** The table says Pi is
"Conditional: validated ID + Session path evidence; Unknown by default"; the prose says Pi is always
Unknown. The prose is right: `src/app.rs:464` hardcodes `pi::activity_status(&parsed, None)` and
nothing outside `pi/test_support.rs:36` ever constructs a `SessionControlEvidence`. "Conditional"
implies a user-reachable condition that does not exist. Aggravating: `src/app.rs:73-79` includes
`"pi"` in the probe `needed` set, so `-a pi` pays a full `ps -A` spawn whose result is discarded at
`:440` (`let _ = ctx;`) — verified with a fake `ps`.

**R7 (SHOULD-FIX) — the "Supported means tests prove it" definition is not satisfied.**
`README.md:146` defines "Supported" as "the corresponding integration tests prove that capability",
but no test in `tests/` proves either new Active cell: `tests/step9_app.rs:52` and
`tests/picker_spike.rs:42,579` all set `RESUME_DISABLE_PROC_PROBE=1`, so the end-to-end suite can
never observe `ACTIVE`. Coverage exists only as in-crate unit tests with injected fixtures. Either
soften the definition or add an end-to-end fake-`lsof` test.

**R8 (SHOULD-FIX) — the man page violates its own normativity rule.**
`docs/design/cli-help-layers-plan.md` §2 makes Layer 3 normative: "If a user-visible fact is not in
the man page, it is not in the product." `RESUME_DISABLE_PROC_PROBE` and the `lsof` runtime
dependency are user-visible and appear nowhere in `src/man.rs`; there is no ENVIRONMENT section at
all. Additionally `src/man.rs:232` defines READY as "no running process was observed", which
positive-evidence-only semantics make false (READY also means "could not tell"), and `:253` publishes
`Inactive { observed_at }` as an observable `--json` value that **no producer in the tree can emit**.

**Minor (NIT).** `README.md:146,152` state the same Codex fact twice, each version incomplete —
residue of both branches' text being kept. "a live Codex process" overstates precision: `lsof -c
codex` is a prefix match and the crate's own test accepts `codex-helper`
(`src/integration/codex/activity.rs:347`, `:207`). The kill-switch sentence at `:155` is stranded in
the *Preview parsing* paragraph. `README.md:126` omits that `--confirm-always`/`--no-confirm` with
`--list`/`--json` is a usage error exiting 2 (`src/cli.rs::validate`, documented at
`src/man.rs:110-113,164-170`). `README.md:88` omits the empty case (`No Sessions found in Scope.`).
`--man` is never mentioned in README although `src/man.rs:610-620` points back at README.
`README.md:17` says "not published to crates.io for v0.1.0" but `Cargo.toml` has no `publish = false`.

---

## 4. Design-doc divergence, and the conflict the merge papered over

### X1 — the OMP branch implemented a superseded design doc

This is the most consequential structural finding and it needs a **decision, not a patch**.

`c37f4f2` ("reconcile OMP plan with adjudicated design decisions") is the direct parent of the
implementation commit `901bb94`. The reconciliation flipped ten decisions. The merged code matches
the **pre**-reconciliation `c13c4e4` on essentially all of them:

| Decision | Reconciled doc (in tree at HEAD) | Merged code |
|---|---|---|
| `BreadcrumbSource` shape | `breadcrumbs() -> Vec<Breadcrumb>` with `recorded_at`; explicitly rejects a TTY lookup | ships the rejected `session_for_tty` (`omp/activity.rs:53-60`) |
| staleness gate | 4-part conjunction, `recorded_at >= started_at` "not optional" | absent (`omp/activity.rs:165-188`) |
| `BREADCRUMB_FRESHNESS` 12h ladder | required | identifier does not exist |
| `ProcEntry` field | `started_at`, "the discriminator" | `elapsed` (`proc.rs:35`), parsed and never read |
| probe diagnostics | `proc_probe_failed` / `proc_probe_timeout`, agent-neutral | zero occurrences |
| budget constant home | `src/runtime.rs` beside `JOIN_BUDGET` | `src/proc.rs:20` |
| timeout mechanism | `recv_timeout` + detach, per `runtime.rs:7-9` | poll `try_wait` @5ms then `kill` (`proc.rs:275-285`) |
| `live_on_tty` accessor | required, to recover `started_at` | absent; `ttys_for_command` discards the entry |
| D2 `with_session_dir_flag` | DELETE | kept + `#[allow(dead_code)]` (c13c4e4's wording) |
| D3 rename | `SESSION_DIR_FLAG` | `FLAG_SESSION_DIR` (c13c4e4's direction) |
| D4 `was_live_growing` | keep `pub`, delete later with pi's twin | demoted to `pub(super)` |
| D5 `is_user_attributed_pub` | move untouched | deleted |
| re-export surface | reproduce pre-split "exactly" | both above omitted |

A rebase-shaped accident is the most economical explanation, and the 10-for-10 correspondence is
conclusive as to *what* happened — but not as to *why*. The effect either way: `docs/design/` at HEAD
is a decision record the code contradicts on nearly every adjudicated point, **including the
mitigation for the risk both versions rank #1** (that is M3).

**Required action:** adjudicate before any fixer work on OMP. Either re-apply c37f4f2's decisions, or
re-adjudicate and rewrite the plan with the reason recorded. Do not leave both in the tree. M3, S3,
S11–S13 all depend on which way this goes.

*Honored, for balance:* the per-OS cfg-gated `PS_ARGS` (`proc.rs:24-27`, byte-for-byte), the
`RESUME_DISABLE_PROC_PROBE` switch, named-profile isolation plus its `// DO NOT unify` marker
(`omp/roots.rs:218`), the "no new `DiscoverConfig` field" constraint, and the 5-module + `tests/`
mirror layout.

### X2 — two incompatible notions of "the probe", fused into a struct neither doc designed

Codex designed `probe() -> (ActivitySnapshot, Vec<Diagnostic>)`: spawn-based, diagnostic-emitting,
agent-scoped categories. OMP designed `DiscoveryContext { procs, diagnostics }`: one struct owning
both halves, with **agent-neutral** categories chosen specifically so Pi could reuse the probe.

The merge produced `DiscoveryContext { procs, codex_activity }` (`src/app.rs:61-64`) — OMP's name,
Codex's contents, and OMP's `diagnostics` field **evicted into an out-of-band parallel parameter**
(`src/app.rs:200-201, 242-243`). Two values that must stay in sync, one of which the type's name
implies it already carries.

It compiles and Codex's diagnostics do reach `DiscoveryState`, so it *looks* reconciled. But OMP's
half shipped zero diagnostics (S3), so the merged struct carries one agent-scoped vocabulary and no
neutral one — the Pi-reuse rationale that justified neutral naming has quietly evaporated, and
`src/proc.rs` is an agent-neutral module whose failures are unobservable. **The shape was never
designed; it is the residue of two `discover_agent` signatures being unioned.**

*Recommendation:* fold `ctx_diagnostics` back into `DiscoveryContext` as the design intended, so the
struct's name matches its contents and the two values cannot desynchronize.

### X3 — two different notions of "positive evidence", never unified

Codex's evidence is kernel-level, single-signal, self-verifying: an open fd on the exact inode. It
**cannot go stale by construction**, which is why its design needs no time discriminator. OMP's is a
three-signal correlation across two data sources, one of which is *documented as outliving its
writer*. It **cannot be correct without** a discriminator.

The merge placed both behind the same `ActivityStatus::Active { observed_at }` and the same
`launch::risk_reasons` gate, so the user sees one label backed by two radically different confidence
levels — and the weaker one shipped without its discriminator (M3). `src/man.rs:232`'s single
definition of READY is the visible seam.

### X4 — the roots-resolution order forked

`omp::roots::resolve` / `agent_root` (`omp/roots.rs:200-238`) is the designed, invariant-guarded,
test-fenced order. `omp::activity::breadcrumb_directory` (`omp/activity.rs:84-102`) is a second,
undesigned order over different env vars with a different profile-coupling rule. Both are live in
production; only the first has an adjudication record. (See S5.)

### X5 — the man page's normativity contract was set by one branch and broken by the two that landed after

`docs/design/cli-help-layers-plan.md` §2 makes Layer 3 the single source of truth for user-visible
facts. Codex shipped an `lsof` runtime dependency; OMP shipped `RESUME_DISABLE_PROC_PROBE` and
documented it in README only. Neither touched `src/man.rs`. The merge made all three compile without
noticing the contract exists — and because the drift-guard test (S10) was also skipped, no mechanism
would ever have caught it. (See R8.)

### Other design-doc verdicts

**Honored.** Codex: exactly one `lsof` spawn per run with fixed argv, pinned by a test asserting the
fake probe ran exactly once; agent-gating; the "missing `lsof` emits no diagnostic" mirror of
`SqliteOutcome::Absent`; per-record-set skip-forward parsing with an unreset accumulator; post-mutate
`session.activity` with zero public signature changes; the §9.8 QA row updates; the load-bearing
`PATH`-scrub comment. cli-help: `help =` on all twelve fields with a test enforcing it; `--man` as
`exclusive`, handled after `validate()`; the 7-spec catalog with no `code` in `--json`;
`E1005`/`E3004` reserved not implemented; the three fatal-site conversions; both product-design
corrections. Packaging: metadata, release profile, four targets with musl preferred, `src/preview/`
grouping.

**Deviations worth folding back into the docs rather than fixing in code.** Codex's `by_identity` is
`Vec<usize>` rather than the designed `usize`, deliberately handling duplicate identities — better
than designed. Codex §5's re-export list omitted the already-`pub` `extract_user_messages`; the code
is right and the doc incomplete.

**Budget miss.** `-h` is **39 lines** against a documented hard ceiling of 34; the design table
omitted the 4-line `Commands:` block, and even the sanctioned trim levers only reach ~36. Note the
requirement was already incoherent — §2's "fits 80x24 without scrolling" is unachievable at 34 lines
either. Re-adjudicate the ceiling and strike the no-scrolling claim.

**CI duplication.** `.github/workflows/ci.yml:87-119` (`target-builds`) and
`.github/workflows/release-builds.yml:16-52` are the same 4-target cross matrix on the **identical**
cron `"17 5 * * 1"`. The weekly schedule builds all four targets twice. f0d31d6 added the new
workflow without noticing the existing job. Separately, `docs/product-design.md:635` promises SHA-256
checksums, artifact attestation, and `v*`-tag-triggered Releases; `release-builds.yml` triggers only
on `schedule` + `workflow_dispatch` and does none of the three. (Pre-existing v0.1.0 scope, not
merge-induced — flagged because it is now visible.)

---

## 5. Mechanical residue

This is the strongest area of the merge. Reported for completeness.

**Clean, verified by execution not inspection.** Zero warnings from `cargo check --all-targets`,
`--all-features`, `--no-default-features`, `cargo clippy --all-targets -- -W clippy::all`,
`--all-features`, and `cargo fmt --check`. Forced full recompile (`cargo clean -p resume`) to rule
out cached-success masking. `0be9051` is also clippy-clean, so this is parity, not a lucky baseline.

**No stale imports.** The `86c84cc` `crate::* -> crate::preview::*` rewrite was neither missed nor
over-applied: `rg 'crate::preview::[a-z_]+'` yields only the six legitimate targets, and
`rg 'crate::preview::(time|scope|session|cli|picker|app|config|launch|runtime|diagnostics)'` is empty.

**No lost tests.** Three independent checks, all clean:
- per-split set diff: `omp/tests.rs` 60 → 64 (lost: none, +4 new); `pi/tests.rs` 46 → 46 (exact set
  equality); `claude/tests.rs` 39 → 39 (exact set equality);
- repo-wide name diff: fork 373 unique names, HEAD 404, fork-only set **empty**;
- runtime `--list` diff with multiplicity, to catch a test that silently stopped compiling: no name
  disappeared, no count dropped.

Totals: 359 → 390 default, 387 → 418 `--all-features`, `0 failed; 0 ignored` everywhere. The +31 are
genuinely new (errors ×13, proc ×5, omp activity ×4, codex ×5, cli/app/man ×4).

**All test modules are wired.** The `pi`/`claude` `tests/` dirs have no `mod.rs` — this looks like
the classic lost-test trap but is intentional `#[path]` wiring from each production sibling
(e.g. `pi/discover.rs:305-306`). All 8 verified; `omp/tests/mod.rs:296-300` declares all 5. Confirmed
by execution, not reading.

**Not leftovers, explicitly cleared.** `src/integration/pi.rs` beside `pi/` and
`codex/sqlite.rs` beside `sqlite/` are correct Rust 2018 module roots (no `pi/mod.rs` exists — that
would be a hard error, so the absence is proven). `sqlite_stub.rs` is reachable via
`#[cfg(not(feature))] #[path]` and verified by a clean `--no-default-features` build. Every
`src/**/*.rs` is reachable from the module tree. No `#[ignore]`, `todo!()`, `unimplemented!()`,
commented-out code, or conflict markers were introduced.

**Residue actually found (all NIT-grade).**
- `src/integration/omp/discover.rs:162` — `#[allow(dead_code)]` masking a **confirmed-dead**
  `was_live_growing`; removing the attribute produces a real warning. See S12.
- `src/integration/omp/roots.rs:118-119` — `#[allow(dead_code)]` that is **not needed**; removing it
  produces no warning, because the method is `pub` on a re-exported type. Pure cargo-cult noise that
  will now swallow a future real signal. Delete lines 118-119, keep the comment.
- `docs/qa/feature-inventory.csv:147-152` — 7 stale `src/*.rs` paths needing a `preview/` prefix. This
  file *was* touched by the merge, so the column was simply not swept; its "Pass" verdicts are
  currently unverifiable by path lookup. A further ~33 rows (101-107, 119-126, 133-145) cite
  `pi.rs:NNN` / `codex/mod.rs:NNN` / `omp/mod.rs:NNN` line anchors invalidated by the two split
  commits — row 144 points at `omp/mod.rs:940-953` for code now at `omp/activity.rs:38-50`.
- `src/integration/claude/tests/format.rs:447,462` — `crate::jsonl` in comments; code is correct.
- `src/diagnostics.rs:10-121` — `RedactedDiagnostic` and `DiagnosticCollector` have **zero production
  call sites**; only `redact_path` / `redact_text` are live. ~110 lines plus 5 unit tests maintaining
  a parallel model with field-identical shape and near-identical render logic to `session::Diagnostic`
  (`"{cat}: {n}"` vs `"resume: {cat}: {n}"`). This is the *actual* duplicate-modelling instance in the
  codebase — pre-existing, but worth deleting while the error model is being tidied.

**Other NITs.** `src/app.rs:204-207` and `:246-249` lock `state.errors` twice in succession (correct,
just noisy — hoist one guard). `src/app.rs:79` gates OMP/Pi on string literals `"omp" || "pi"` while
`:83` gates Codex on `codex::AGENT` — same function, two conventions.
`src/integration/codex/activity.rs:101` has an unreachable `expect("matched snapshot exists")` panic
seam; bind the snapshot in the match instead. `src/proc.rs:200-207` `normalize_tty` is
`#[cfg(any(target_os = "linux", test))]`, so on macOS it exists only to satisfy tests although the
doc says every TTY string on both sides passes through it. `resume -a pi` still spawns `ps` for a
consumer that discards it. `-h` omits E-codes while `--help` includes them — **intentional** per the
three-layer design; noted so nobody "fixes" it. `docs/design/*.md` status headers all still read
"proposed" / "design only" / "ready to implement" for work that shipped, and every `src/app.rs:NNN`
citation in both activity plans is off by 40-70 lines. `CHANGELOG.md`'s `## Unreleased` is empty
despite five feature merges, although `docs/product-design.md:599` requires distinct entries for
Support List changes.

---

## 6. Design note — how the two error systems should relate

Paste-ready; resolves the "are E-codes and diagnostics duplicate models?" question raised in the
review brief. **Answer: no, and they should not be merged.**

> `resume` has two error channels and this is deliberate. They are distinguished by *cardinality and
> finality*, not by severity.
>
> - **E-codes (`src/errors.rs`)** — fatal, **at most one per run**, terminate the process, and **own
>   the exit code**. Rendered as the four-line `ERROR [E1004] INVALID_CONFIG: …` block on stderr via
>   `Report::emit()`. Never appear in `--json`, because a run that emits one produces no JSON
>   document at all.
> - **Diagnostics (`session::Diagnostic`)** — non-fatal, **N per run**, produced concurrently by four
>   discovery threads, aggregated by category, rendered as `resume: {category}: {count}` on stderr and
>   as `errors[]` in `--json`. They never determine the exit code directly; only the derived predicate
>   "did *every* integration fail?" does (`discovery_exit`).
>
> These cannot be merged: `E1001.report(...).emit()` returns an exit code and there is exactly one of
> them; `claude_no_session_id: 347` is a count.
>
> **Rule 1 — a failure belongs to exactly one channel.** If the run can continue, it is a diagnostic.
> If it cannot, it is an E-code. Never both. Today's apparent overlap (`E3001` ↔ `*_root_unavailable`)
> is not duplication: `E3001` is the *documentation* of what those tokens mean, published only in the
> man page and never injected at runtime. Preserve that asymmetry — `errors[]` declares
> `additionalProperties: false`, so adding a `code` field is a one-way schemaVersion bump for a field
> that would be null on 12 of 19 tokens. The cli-help plan already considered and rejected it.
>
> **Rule 2 — `ErrorSpec::categories` is the only bridge, and it must be enforced by test, not prose.**
> See S10.
>
> **Rule 3 — every category token must be constructible, documented, and aggregated identically on
> both sinks.** Three invariants, one test each:
> (a) no token in `errors::category` without a production construction site — violated today by
> `io_error`, `claude_missing_workspace`, `claude_discovery_failed`, `unknown_agent`;
> (b) no production token missing from `errors::category` and the man page — violated by
> `codex_activity_probe_{failed,partial}`;
> (c) `--json errors[]` and stderr must agree on counts — violated (M5).
>
> **Rule 4 — exit-code contribution is a property of the *integration*, not of the diagnostic.**
> `AgentDiscovery::integration_ok` is the sole input to `discovery_exit`. A per-agent failure must set
> it consistently; today pi does and omp does not, for the same condition (S2). For multi-root agents,
> `integration_ok` means "at least one root was scanned successfully".

**Note on `unknown_agent`:** it is currently modelled *three* times — a reserved `E1005`, a diagnostic
token at `src/app.rs:396`, and the actual shipped bare `eprintln!` at `src/app.rs:152`. Validation at
`:152` fires first, so the diagnostic arm is dead. Verified: exit 2, no E-code, no diagnostic.
Collapse to one.

---

## 7. Recommended sequencing

**Before release (MUST-FIX).**
1. **M6** — regenerate the three completion scripts. Independent of everything; land immediately.
2. **M1** — guard the Codex probe with `RESUME_DISABLE_PROC_PROBE`; fix the `step9_app.rs` comment.
   Fixing this in code makes `README.md:155` true (R1) without a doc change.
3. **M2** — always print diagnostics after `print_list`.
4. **M5** — aggregate before serializing `errors[]`.
5. **M4** — distinguish `PermissionDenied` from `NotFound` in codex/claude `read_dir`.
6. **X1 adjudication** — decide whether c37f4f2's decisions get re-applied or re-adjudicated. **This
   is the one-way door and it gates M3, S3, and S11–S13.** Everything else is a two-way door.
7. **M3** — the staleness gate, once X1 is decided. Land together with S3: `started_at`,
   `live_on_tty`, `recorded_at`, the `>=` gate, and the probe diagnostics are one change. Reproduce
   the recycled-TTY false positive first.

**Cheap ratchets worth batching into the same PR.**
S10's man-page drift test (all assertions already pass — pure ratchet, no fixups), S14's `cargo doc
-D warnings` CI step, and deleting the duplicated cross-build matrix.

**Documentation sweep (one pass).**
R2–R8, the `docs/design/*.md` status headers, the `docs/qa/feature-inventory.csv` path columns, and
the empty `CHANGELOG.md`. R5 (the Privacy claim) needs a product decision, not just an edit.

**Deferred.** S1 (probe timeout), S4 (Codex identity lookup — verify with a differing-basename
hard-link fixture first), S5, S11–S13 (omp export parity, pending X1), and the NIT batch.

---

## 8. Coverage and verification gaps

What this review could **not** establish, so it is not mistaken for a clean bill:

- **M3 was not reproduced.** It is a high-confidence reading of `correlate_live_with` against
  `session-formats.md:127`. It needs a live `omp` on a recycled TTY.
- **X1's root cause is not established.** The 10-for-10 correspondence with c13c4e4 is conclusive as
  to *what*, not *why*. If the divergence was deliberate, the doc needs rewriting, not the code.
- **The `codex-sqlite` feature was not exercised** (default off, `Cargo.toml:37`).
  `codex_sqlite_{id,workspace}_mismatch` and the six degradation sub-tokens are read-verified only.
- **The interactive picker path was not exercised** (no TTY), so `E3003`'s two `.emit()` sites
  (`src/app.rs:319,327`) are read-verified only.
- **No test asserts the public export surface for pi, claude, or omp** — which is why four omp items
  could vanish with a green build. codex has exactly this guard
  (`codex/tests.rs:28 public_extract_user_messages_api_is_preserved`, pinning a fn pointer type).
  Port it to the other three.
- **No end-to-end test can observe `ACTIVE` at all** — every binary-level test disables the probe. A
  fake-`lsof` test through `tests/step9_app.rs` would pin both the `ACTIVE` path and the M1
  kill-switch. A test named after the README sentence would have caught M1 at merge time.
- **No test covers "lsof absent on macOS → Unknown, no diagnostic"** (R2).
- **No CI rustdoc gate**, so S14 regressed invisibly and the next doc-link break will too.
- **Multi-user `lsof`:** `lsof -c codex` returns fds for all users' codex processes where permitted.
  `ActivitySnapshot::lookup` matches on device+inode so a same-dev/inode collision across users is not
  possible locally, but a shared or NFS-hosted rollout may need a caveat. Not investigated.
- **Pre-existing, out of merge scope, noted only for awareness:** no dedup across repeated `-a` flags
  (`resume -a omp -a omp` yields duplicated sessions — present at `0be9051`); `Cargo.toml` lacks
  `publish = false`; the release workflow has no checksum/attestation/tag trigger.
