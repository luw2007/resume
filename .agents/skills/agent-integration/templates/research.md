# Research: `<agent>`

## Sources

- <doc URL / source file / version, one per line>

## Local session persistence

- Store root: `<path or env var, with default>`
- Override precedence: `<flag > env > config > default, as applicable>`
- Format: `<JSONL / SQLite / JSON / other, with version notes>`
- Session identity field(s): `<field name(s) that uniquely and stably identify one session>`

## Session ↔ workspace relation

- Field(s) recording the working directory: `<field path>`
- Stability: `<does it survive rename/move? relative or absolute?>`

## Native resume

- Documented command: `<exact command/flags, or "none documented">`
- Verified command (after install probe): `<exact argv observed to work, or "not yet probed">`
- Isolation/profile concept, if any: `<description, or "none">`

## Install probe

- Install command run: `<exact command>`
- Maintainer approval: `<date/reference>`
- Install location: `<path(s) written>`
- Login/network required: `<yes/no, detail>`
- Minimal run performed: `<what was run to produce one real session artifact>`
- Probed artifact path/excerpt: `<path, and a short redacted excerpt if useful>`

## Implementation mapping

- `SessionKey`: `<agent, effective_root, profile, native_locator derivation>`
- `WorkspaceEvidence`: `<how derived, read-only>`
- `ResumeSpec`: `<program, argv, cwd, env derivation>`
- Closest existing integration for shape parity: `<pi | claude | codex | omp>`

## Verification (real installation)

- `resume --list -a <agent>` output: `<summary or confirmation>`
- Workspace match confirmed: `<yes/no + detail>`
- Fake-native-executable test argv cross-checked against real native resume: `<yes/no + detail>`

## Result

- Status: `<researched | skill-defined | implemented | verified | unsupported | blocked>`
- If `unsupported`/`blocked`: `<which of the three support conditions failed, or what is blocking, with evidence>`
