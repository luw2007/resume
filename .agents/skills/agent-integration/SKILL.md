---
name: agent-integration
description: Take one coding-agent name supplied by the maintainer through installation, local investigation, and a native `resume` integration for the `resume` Session Launcher, closing with a verified or evidence-backed unsupported result. Use only when the maintainer names a specific candidate agent to integrate; never batch-run this Skill across a list.
---

# Agent Integration

Adds one coding agent to `resume`'s discover/preview/resume integrations
(`src/integration/<agent>/`), or closes it out as `unsupported` with
reproducible evidence. Driven by the maintainer on their own machine — this
Skill never installs, runs, or judges a candidate the maintainer did not name.

## Non-negotiables

- **Single input.** The maintainer supplies exactly one agent name per run.
  Never read the project's `Supported Agents` table as a work queue or infer
  "do the next one."
- **Explicit confirmation before any side effect.** Before running a package
  manager, downloading a binary, or executing the agent's CLI for the first
  time, show the maintainer exactly what will run (command, install
  location, whether it requires login/network) and wait for their go-ahead.
  Never chain confirmations across agents.
- **No runtime plugin.** The deliverable is ordinary Rust source under
  `src/integration/<agent>/`, wired the same way `pi`/`claude`/`codex`/`omp`
  are. This Skill and its evidence files are never loaded by the `resume`
  binary.
- **CI never installs or runs a real agent.** Only fixture data and
  fake-native-executable regression tests may run unattended.
- **Closed loop, always.** The run ends in exactly one terminal
  `checklist.md` status: `verified` or `unsupported`. Never leave a run at
  `researched` or `implemented` without continuing to a terminal state or
  explicitly stopping and saying why.

## Procedure

Work through these steps in order. Each has a concrete exit condition —
don't advance until it's met.

### 1. Untriaged → researched

1. Add a `| <agent> | untriaged |` row to `checklist.md` if the agent isn't
   already listed.
2. Read the agent's own documentation/source for: where it persists
   sessions/conversations on disk, the on-disk format, how a session record
   ties back to a working directory, and any documented native "resume this
   session" command or flag.
3. Record findings in `evidence/<agent>/research.md` using
   [`templates/research.md`](templates/research.md). Cite sources (doc URLs,
   source files, versions).
4. If research already shows the agent has no locally persisted,
   individually resumable session (e.g. it is web/hosted-only, or sessions
   live only in a vendor cloud with no local file), stop here and go to
   [§5 Unsupported](#5-any-step--unsupported).
5. Set `checklist.md` status to `researched`.

### 2. Researched → installed & probed (explicit confirmation gate)

1. Determine the install command for the agent (its documented install
   method — package manager, script, binary download).
2. Present the maintainer the exact command, where it writes files, and
   whether first run requires login or network access. **Wait for explicit
   approval; do not run anything before it.**
3. On approval, install and perform one minimal run (e.g. one trivial
   prompt) sufficient to produce a real, on-disk session artifact.
4. Locate and inspect that artifact: exact path, format, the field(s) that
   identify the session, and the field(s) that record the working
   directory.
5. Record the install command, approval, and probe evidence (paths, file
   excerpts, format notes) in `evidence/<agent>/research.md`. Set
   `checklist.md`'s Install confirmation to the approval date/reference.
6. If the probe contradicts step 1's documentation, correct
   `research.md` — the probe is stronger evidence than docs.

### 3. Installed & probed → skill-defined (implementation protocol)

Map the probed evidence onto `resume`'s existing integration contract
(`src/session.rs`: `SessionKey`, `Session`, `ResumeSpec`,
`WorkspaceEvidence`). Write this mapping into `evidence/<agent>/research.md`
before writing code:

- **Session store root** → the discovery root, with its override precedence
  (env var, CLI flag, config key), mirroring `src/integration/pi.rs`'s
  documented precedence comment.
- **One session artifact** → one `Session` with a stable `SessionKey`
  (`agent`, `effective_root`, `profile`, `native_locator`).
- **Session ↔ workspace field** → `WorkspaceEvidence`, read-only, no
  normalization beyond what the other integrations already do.
- **Native resume command** → `ResumeSpec` (`program`, `argv`, `cwd`, `env`),
  built as discrete argv — never a shell string.
- **Isolation/profile concept**, if the agent has one (compare `omp`'s
  profile handling) → part of `SessionKey` identity.

Set `checklist.md` status to `skill-defined` once this mapping is written
down and reviewed against an existing integration (`pi`, `claude`, `codex`,
or `omp`) for shape parity.

### 4. Skill-defined → implemented → verified

1. Implement `src/integration/<agent>/` (`discover`, `format`/parse,
   `resume`, `roots` as needed — follow the module shape of the closest
   existing integration).
2. Wire the new agent into `src/app.rs` (`effective_options`'s agent
   allow-list and `discover_agent`'s match) and `src/cli.rs`/config docs so
   `-a <agent>` and `agents = [...]` accept it.
3. Add integration tests exercising discovery against fixture data and a
   fake-native-executable resume regression, following the existing
   `tests/` patterns for `pi`/`claude`/`codex`/`omp`. These are the tests CI
   runs; they must not touch the real agent binary.
4. Add a `<agent>_discovery` benchmark group to `benches/discovery.rs`
   and its synthetic fixture generator to `benches/fixtures.rs`, matching
   the shape of the existing `codex_discovery`/`pi_discovery`/
   `omp_discovery`/`claude_discovery` groups (comparable file/line/large-
   file parameters so results are cross-agent comparable). The group's
   doc comment must state what performance risk it tracks: full-file
   parse cost for file-based stores, or query-scale cost for indexed
   stores (SQLite, etc.) — every agent gets a group regardless, so a
   future regression is always visible. Record the performance
   characterization in `evidence/<agent>/research.md`.
5. Set `checklist.md` status to `implemented`.
6. Run the real, maintainer-installed agent end to end: `resume --list -a
   <agent>` must show the real session probed in step 2; confirm the
   `Session`'s workspace matches; confirm the fake-native-executable test's
   asserted argv matches what the real native resume actually needs (cross-
   check against the agent's own resume docs/`--help`, not just the fixture).
7. Update `README.md`'s Support list table and any config docs to include
   the new agent.
8. Commit the source, tests, docs, and checklist/evidence changes for this
   one agent (see repo git rules — explicit paths, no `git add -A`).
9. Set `checklist.md` status to `verified`.

### 5. Any step → unsupported

If at any step the evidence shows the candidate cannot meet all three of:
(a) a locally enumerable persisted session store, (b) a stable session-to-
workspace relation, (c) a native command that resumes a specific selected
session — stop implementation. Record which condition failed and the
evidence in `evidence/<agent>/research.md`, and set `checklist.md` status to
`unsupported`. This is a valid, closed terminal state — do not leave the row
at `researched` instead.

## Files

- [`checklist.md`](checklist.md) — the only progress ledger: `| Agent |
  Status |`. See its header for the exact status vocabulary.
- [`templates/research.md`](templates/research.md) — required structure for
  every `evidence/<agent>/research.md`.
- `evidence/<agent>/` — durable, reviewable research and probe artifacts for
  one candidate; created on first use.
