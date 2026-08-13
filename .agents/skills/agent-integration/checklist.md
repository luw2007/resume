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
  with fixture/fake-native tests passing.
- `verified` — real local discovery, workspace relation, and exact-resume
  behavior confirmed against the maintainer's actual installation;
  README/docs updated; changes committed. **Terminal.**
- `unsupported` — reproducible evidence shows the candidate cannot meet the
  three support conditions; reason recorded in `evidence/<agent>/`.
  **Terminal.**
- `blocked` — cannot proceed without something outside this Skill's control
  (e.g. missing access); reason recorded in `evidence/<agent>/`.

| Agent | Status |
|---|---|
