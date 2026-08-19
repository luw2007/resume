# Agent Integration Checklist

The sole progress ledger for the `agent-integration` Skill. A row exists
only for an agent the maintainer has explicitly named to the Skill — this
table is never pre-populated from the project's Supported Agents list.

Status is exactly one of:

- `untriaged` — row created, no research yet.
- `researched` — local persistence, workspace relation, and native resume
  behavior are documented in `evidence/<agent>/research.md`.
- `skill-defined` — the probed evidence is mapped onto `resume`'s
  `SessionKey`/`Session`/`ResumeSpec`/`WorkspaceEvidence` contract.
- `implemented` — `src/integration/<agent>/` exists, wired into `resume`,
  with fixture/fake-native tests passing **and** a `<agent>_discovery`
  benchmark group in `benches/discovery.rs` (plus its fixture generator in
  `benches/fixtures.rs`); performance characterization recorded in
  `evidence/<agent>/research.md`. This does not claim a real installation
  was probed.
- `verified` — a real local session artifact, its workspace relation, and
  exact-resume behavior are recorded from the maintainer's actual
  installation after an explicit side-effect confirmation, or a documented
  read-only probe of an already-installed artifact; README/docs are updated
  and changes committed. Fixtures, fake executables, documentation, and
  captured CLI help alone cannot establish this status. **Terminal.**
- `unsupported` — reproducible evidence shows the candidate cannot meet the
  three support conditions; reason recorded in `evidence/<agent>/`.
  **Terminal.**
- `blocked` — cannot proceed without something outside this Skill's control
  (e.g. missing access); reason recorded in `evidence/<agent>/`.

| Agent | Status |
|---|---|
| opencode | verified |
