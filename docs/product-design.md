# Resume product design

This document consolidates the decisions confirmed during the `grill-with-docs` interview. It records the intended behavior of `resume`; implementation sequencing belongs in [`plans/v0.1.0-implementation.md`](../plans/v0.1.0-implementation.md), canonical vocabulary belongs in [`CONTEXT.md`](../CONTEXT.md), verified agent formats belong in [`docs/research/session-formats.md`](./research/session-formats.md), and the Rust/Skim choice belongs in [ADR 0001](./adr/0001-rust-and-skim-for-terminal-session-picker.md).

## 1. Product

`resume` helps a user find the correct coding-agent Session when they remember the work or project but not which agent they used. It discovers persisted Sessions, shows enough context to make a decision, and invokes the selected agent's exact native Resume operation.

Positioning:

> Find and resume the right coding-agent session from your current project or worktree.

The product is a Resume launcher, not a general transcript manager.

### Core behavior

- The command name is `resume`.
- It runs in the current terminal.
- It never chooses or resumes a Session automatically, including when there is only one result.
- After selection, it enters the Session's recorded Workspace and lets the native agent replace the launcher process with Unix `exec`.
- Discovery and Preview are read-only. They never repair, migrate, import, rename, delete, or rewrite Session data.
- The native agent may write after the user explicitly chooses Resume; the read-only guarantee ends at that handoff.
- Cross-agent imported Sessions remain distinct Resume targets. They may be shown as related but are never merged or automatically deduplicated.
- There is no machine-wide Session listing.

### Platforms

- First-class platforms: macOS and Linux.
- Windows is not supported in v0.1.0 because it lacks equivalent process-replacement semantics and has not been validated.
- The implementation language is Rust with MSRV 1.91.

## 2. Support model

The `npx skills` agent catalog is a source of candidates, not evidence that an agent supports Session discovery or exact Resume.

Each Agent Integration is an independent in-tree module. Integrations share only mechanical helpers such as safe JSONL reading, path normalization, text normalization, and process launch. They do not share discovery rules, schema assumptions, Workspace inference, or Resume commands. There is no plugin system or separately installed adapter package.

### Support states

- **Supported**: discovery, Preview, and exact native Resume by stable identity are verified.
- **Discover Only**: discovery and Preview work, but exact Resume cannot be guaranteed; selection cannot launch it.
- **Unsupported**: the persisted format cannot be parsed reliably.
- **Unavailable**: the integration exists and Session data may be discoverable, but the native CLI is not installed or not on `PATH`.

Only Supported Sessions can Resume. A “continue latest” command cannot substitute for exact Resume.

### Loading behavior

By default, load every implemented integration, including integrations whose native CLI is unavailable. A missing data root returns quickly. Planned but unimplemented integrations are not scanned. `--agent` narrows this set.

### Support order

First batch, all required for v0.1.0:

1. Codex
2. Claude Code
3. Pi
4. OMP / Oh My Pi

Second batch:

- Cursor Agent
- OpenCode
- Grok
- Gemini after stable-ID Resume research

Installed agents are validated first; subsequent additions prioritize broad adoption. README owns the user-facing Support List, with separate columns for Discovery, Preview, Exact Resume, Profiles, and Active Detection. An integration is marked Supported only after fixture tests, launch-contract tests, and local read-only validation.

### Agent profiles and roots

An agent profile or effective data root is part of Session identity and launch provenance. The same native ID in different roots or profiles is not the same Session.

- OMP automatically discovers the default and named profiles, displays names such as `OMP[work]`, and preserves the original profile on Resume.
- If OMP profile provenance cannot be determined, the Session is Discover Only rather than guessed as default.
- Agent data roots are resolved from official environment/config mechanisms first, official read-only status interfaces when safe second, and standard defaults last.
- The launcher does not recursively search `$HOME` for unknown stores.
- The first release has no custom `agent=path` Session-root CLI setting.

The verified first-batch native contracts are:

| Agent | Stable identity source | Authoritative Workspace | Exact Resume |
|---|---|---|---|
| Codex | `session_meta.payload.id`, isolated by `CODEX_HOME` | `session_meta.payload.cwd` | `codex -C <workspace> resume <uuid>`, preserving nondefault `CODEX_HOME` |
| Claude Code | top-level UUID filename agreeing with embedded `sessionId`, isolated by `CLAUDE_CONFIG_DIR` | transcript event `cwd` | run `claude --resume <uuid>` from Workspace, preserving nondefault root |
| Pi | JSONL header `id`, isolated by effective Session root and transcript path | header `cwd` | `pi --session <absolute-jsonl-path>`, preserving custom Session root; never use `--session-id` |
| OMP | JSONL header `id`, isolated by base/profile/Session root | header `cwd` | `omp [--profile <name>] --resume <id>`, preserving discovered root and custom Session directory |

Exact formats, precedence, imports, legacy variants, and fixture implications are in the research document.

## 3. Session identity and metadata

A Session's stable launcher identity is integration-owned and includes:

- agent;
- effective data root or isolation boundary;
- profile where applicable;
- native Session locator.

The stable Resume ID/path is separate from rendered text. A Skim row, title, row number, or shortened ID is never used as identity.

### Duplicate records

Within one integration, duplicate `(agent, isolation provenance, native ID)` records collapse to one candidate:

1. Prefer the agent's official primary index/source when authoritative.
2. Otherwise prefer the record with later activity.
3. Show other source paths in Preview as duplicate sources.
4. If records disagree on Workspace, mark `Conflicting metadata` and require confirmation.

Cross-agent records never deduplicate, even when one was imported from another agent. A known import relationship may be shown as a badge and used to place related entries near one another.

### Decision metadata

The main list provides:

- status;
- agent and profile;
- last activity time;
- native title or deterministic summary;
- Workspace;
- branch and worktree state;
- shortened Session ID.

Preview additionally provides:

- full Session ID;
- full Workspace and canonical real path;
- worktree root and branch;
- original Session storage path;
- support/activity/risk details;
- expected native Resume command;
- user input history.

Default ordering is last activity descending, unknown time last. During asynchronous streaming, strict global order is not guaranteed. Each integration sends newest first. A final global reorder is allowed only if Skim can preserve the stable selection. Once a query is present, fuzzy match quality takes precedence over time.

### Titles and summaries

- Prefer the agent's native title.
- Without one, derive a deterministic one-line summary from the first valid human user input; never invoke an LLM.
- Skip pure agent control commands, known harness automation, and normalized-empty injections.
- Do not skip genuine short natural-language inputs such as “continue” or “confirm.”
- Agent-specific control-command recognition stays in its integration.
- For an untitled Session, perform at most a 1 MiB early read before first display. If no human input is found, show `(no early user input)` and continue Preview parsing in the background.
- Collapse whitespace, sanitize controls, and truncate by Unicode display width. Title allocation is at most 60 columns on a wide terminal and at least 16 columns in the compact layout, ending with `…` when truncated.
- Search and JSON retain the full title/summary.

### Time

Activity uses the agent's native activity timestamp when available and file modification time only as a documented fallback. The Preview identifies the time source.

Main-list format:

- under one hour: minutes;
- under one day: hours;
- under seven days: days;
- older in the current year: local month/day;
- another year: ISO date;
- unavailable: `unknown`.

Preview shows a full local timestamp and source. JSON uses RFC 3339.

## 4. Workspace and Git Scope

The Workspace is the directory recorded by the Session. Resume always uses it, even when it is a subdirectory of a worktree. The current launch directory and worktree root are only Scope and context inputs; neither replaces the Session Workspace.

### Default Scope

Without an explicit direction:

- In a Git repository, include Sessions whose Workspaces belong to the current repository's current worktree, at any depth within it. `--all-worktrees` widens this to every linked worktree of the repository instead of only the current one.
- Determine repository identity using Git common-directory/worktree information, not repository-name or path-prefix guesses.
- Outside Git, match only a Workspace exactly equal to the current real directory.
- If Git is unavailable or repository resolution fails, warn and degrade to the non-Git exact-directory rule.

The launcher filters Workspaces already recorded in agent stores. It does not recursively scan the filesystem.

Resolving every linked worktree costs an additional `git worktree list` subprocess call beyond the single `git rev-parse` call that resolving only the current worktree needs (see "Git metadata performance" below), and most invocations only care about the current worktree's own Sessions. Current-worktree-only is therefore the default; `--all-worktrees` is opt-in.

### Explicit direction

The optional direction replaces the Git default Scope:

```text
-U N, --up N
-D N, --down N
-U all, --up all
-D all, --down all
```

`--up` and `--down` are mutually exclusive. `--all-worktrees` only widens the Git default Scope, so it conflicts with `--up`/`--down`: exit 2. Distance is the number of real path-component edges from the base directory:

- distance `0`: only the base directory;
- `--up 1`: base plus parent;
- `--up 2`: base, parent, and grandparent;
- `--down 1`: base plus direct-child Workspaces;
- `--down 2`: base plus descendants at most two real path edges away.

Upward matching includes only the exact ancestor chain; it never includes sibling or descendant subtrees of an ancestor. Downward matching uses real path distance and ignores Git/worktree/mount boundaries. `all` has no hidden depth limit. `--up all` reaches `/`; `$HOME` and `/` are marked Broad Workspace and require confirmation.

Examples:

```bash
cd /work/team/app/src
resume --up 2
# exact Workspace candidates: src, app, team

cd /work/team
resume --down 2
# Workspace candidates: team and recorded descendants up to two path edges
```

### Directory argument

```text
resume [DIRECTORY]
```

- Defaults to the current directory.
- Changes the Scope base without changing the launcher's working directory during discovery.
- Must exist and be a directory; a file is an error rather than implicitly using its parent.
- Final Resume still enters the selected Session Workspace.

### Path identity

- Existing paths are canonicalized/`realpath`-resolved for membership and distance.
- Compare path components, never string prefixes, and do not force lowercase.
- UI retains the recorded path and shows the resolved path in Preview.
- Home-relative display uses `~`; JSON uses absolute normalized paths.
- Missing paths cannot be resolved and are lexically normalized only for diagnosis.
- No filesystem case-sensitivity probing is performed.

### Missing, changed, and unknown Workspace

- A missing Workspace is an Unavailable Session: visible for diagnosis when it is otherwise attributable to Scope, but cannot Resume. The launcher never substitutes another worktree or checkout.
- If the path still exists but persisted historical Git evidence proves it now has a different repository/worktree identity, mark `Workspace changed` and require confirmation.
- Compare historical evidence only when the Session actually persisted it: common directory, repository root, remote identity, or worktree path. Branch changes alone are normal and do not trigger the warning.
- Without historical Git evidence, show `Original Git identity unknown` and do not warn solely from absence.
- Do not maintain a private path-history database.
- A Workspace may be inferred only through an official deterministic agent encoding. Mark the evidence as recorded or inferred. If Workspace remains unknown, omit it from Picker/list/JSON because Scope membership cannot be proven; include only a skipped count, with redacted details under `--verbose`.

### Git metadata performance

- Query each normalized Workspace at most once.
- Query each Git common directory's worktree list once, and only when `--all-worktrees` is set; the default (current worktree only) resolves the common directory and current worktree together in a single `git rev-parse` call and never needs `git worktree list`.
- A recorded Workspace that does not even share a path prefix with the current repository's worktree(s) is never a Scope match; skip its per-Workspace Git common-directory query entirely rather than spawning `git rev-parse` only to discard the result. Measured against a real multi-project Session history, this ordering alone removed the subprocess spawn for roughly 99% of distinct Workspaces.
- Cache only for the current process.
- Git metadata failure does not block discovery or Resume.
- Do not delay candidate display for Git decoration; update safely if possible, otherwise enrich only Preview.

## 5. Interactive Session Picker

The embedded Skim library owns the full-screen picker. `resume` does not wrap Skim in another Ratatui UI and does not directly depend on Ratatui, Crossterm, or Tokio. Discovery uses standard threads and bounded channels.

### List layout

```text
UPDATED  AGENT[PROFILE]  TITLE  BRANCH
```

TITLE receives the remaining terminal budget after the fixed `UPDATED`/`AGENT[PROFILE]` columns, clamped between 16 and 60 columns; BRANCH always starts at a fixed column after TITLE's padded width. There is no horizontal scrolling and no other adaptive compacting.

Each custom Skim item separates:

- `display()`: terminal-safe visible row;
- `text()`: normalized searchable metadata and, when safely available, user input;
- `output()`: an opaque key used to retrieve the structured Session from memory.

Hidden searchable fields are not encoded through invisible ANSI text and are never written to a temporary file.

### Search

Skim provides fuzzy filtering. Searchable metadata includes agent, profile, title/summary, Workspace, branch, Session ID, and normalized user input when available.

Discovery and content parsing are two-stage:

1. Emit usable metadata quickly.
2. Parse Preview/user input in the background, newest first.

If Skim can safely update an emitted item without moving the stable selection, user content becomes searchable after parsing. Otherwise do not rebuild the list; that Session's content remains browsable through Preview but may not join main-list matching during that run. First-screen speed and selection safety take priority over complete transcript indexing.

There is no implicit age limit, result count cap, continuous watch, or refresh; rerun `resume` to rescan. The Picker opens after discovery settles with an `All` tab plus one tab per discovered agent. Each tab retains every Session, sorts oldest-first with the most recently updated last, splits results into pages of 50, and opens on its newest page. `Alt+P`/`Alt+N` move to the older/newer page in the current tab. `Alt+Left`/`Alt+Right` cycle through tabs and reset the selected tab to its newest page.

Within an open Session Preview, search applies only to that Session's user inputs and supports previous/next match navigation, subject to Skim's proven public interaction surface.

### Preview

- Preview is hidden by default and toggled with `Ctrl+O`.
- Configuration may start it visible.
- Auto layout uses a right pane around 60% on wide terminals and bottom on narrow terminals. Users may prefer `right` or `bottom`, but the UI may safely degrade when space is insufficient.
- Preview scrolling uses Skim's native keys, shown in the interface rather than duplicated as a fixed README key list.
- `Ctrl+R` toggles Normalized and Raw for the current run and clearly labels the mode. The mode preference is not persisted.
- If Skim cannot dynamically redraw this switch, show Normalized and Raw as labeled sections in one Preview. Do not fork Skim or build another TUI for it.
- `Ctrl+O` and `Ctrl+R` are application-owned bindings. Do not inherit `SKIM_DEFAULT_OPTIONS`, including visual settings, because global bindings must not override product behavior.

### Preview content

- Show user inputs in original conversation order with each available timestamp.
- Exclude assistant replies, tool calls, and tool results.
- Preserve ordinary text line breaks and render as plain terminal-safe text, not Markdown execution.
- Source-first filtering distinguishes genuine user input from agent/harness injection. Structural fallback collapses only complete known wrappers such as `<skill>...</skill>`. It never strips arbitrary XML that may be the user's subject matter.
- Raw means no semantic filtering; it still escapes dangerous terminal bytes.
- Attachments use placeholders such as `[Image: screenshot.png]` or `[File: logs.zip]`. Never render image base64, binary bodies, or signed URL query strings. Normalized local paths show only safe filenames; Raw shows the original representation after terminal safety encoding.
- An attachment-only user input is valid and may become the summary.
- Preview never renders or opens the attachment.
- Preview parsing is bounded to 16 MiB per Session. If more content exists, append an explicit truncation notice and show the source path; never silently truncate or launch an external pager/editor.

### Preview cache and workers

- Process-only LRU cache: 64 MiB soft total, 16 MiB per Session.
- Raw and Normalized share a parsed message representation.
- The currently displayed item may temporarily exceed the soft total.
- Use no disk cache.
- One discovery worker per integration; each integration scans its stores/profiles sequentially. **Codex is a scoped exception**: see "Codex parallel scan and discovery cache" below.
- At most four Preview workers; never one thread per Session.
- Prefer selected-item parsing via Skim selection/Preview callbacks. If selection changes cannot be observed reliably, parse on demand in `preview()` with cache, then fall back to newest-first background parsing.
- All workers use cooperative cancellation. Ordinary exit waits at most 250 ms; a successful `exec` does not wait on slow discovery.

### Codex parallel scan and discovery cache

Codex's rollout store has no Workspace-encoded directory names (unlike Pi/OMP/Claude), so it cannot prune whole directories by Scope before reading; every invocation walks the full store. Measured on a real corpus (3546 rollouts, ~2.9 GB) this made Codex the one integration whose single-threaded scan time scales with total corpus size rather than Scope size — up to 18-19 seconds, dwarfing every other integration's sub-second scan on the same machine. Two changes address this, both scoped to Codex only:

- **Bounded parallel scan.** Codex's own file list is processed by up to 8 scoped worker threads (not one thread per file) instead of sequentially, an explicit, documented exception to "one discovery worker per integration... scans sequentially" above. Chosen from real-corpus, in-process measurement (no per-file process spawn, which would only measure process-creation overhead, not disk throughput): 8 workers cut a 1200-file read from 1.2s to 0.086s (14x); 16 workers were only marginally faster (0.074s) for twice the threads. Output order and content are unaffected — results are reassembled in the original sorted order before any downstream code sees them.
- **Discovery cache.** A small file at `$XDG_CACHE_HOME/resume/codex-discovery-v1.json` (falling back to `~/.cache/resume/`) maps each rollout's absolute path to its parsed content, keyed by (size, mtime). A cache hit skips the file read and JSON parse entirely; a miss does a full parse and records the result for next time. Purely a discovery-speed optimization with the same non-authoritative posture as `state_5.sqlite` enrichment above: the rollout JSONL remains authoritative, a missing/corrupt/version-mismatched cache file silently degrades to a full fresh scan (never blocks discovery, never changes the result), and deleting the cache file is always safe. Measured on the same corpus: a warm-cache rerun of a 1016-session Scope dropped from 3.95s (cold, populating the cache) to 0.17-0.19s (20-23x), with byte-identical output confirmed against the uncached path. The cache intentionally ignores Scope when deciding what to cache -- it stores every rollout's true parsed content regardless of which Scope discovered it, so a cache warmed from one project directory also speeds up a later invocation from a completely different one against the same underlying store.

### Asynchronous stability

- Selection binds to the opaque Session key, never row index.
- New candidates may arrive and reorder matching, but must not change which Session is selected.
- If a selected candidate disappears because of filtering, clear selection rather than choose a replacement.
- Each integration reports independent loading/error state; one failure cannot block others.
- Candidate channels and worker pools are bounded.

### Unavailable candidates

Workspace-missing, Discover Only, and Agent-unavailable Sessions may remain visible for diagnosis with clear status labels.

When Enter is pressed:

1. Prefer to keep the Picker open and show why the candidate cannot Resume.
2. If Skim's public API cannot intercept acceptance reliably, exit and report the reason with status 2.
3. Do not fork Skim to improve this behavior.

### Terminal behavior

- Interactive mode uses the controlling terminal (`/dev/tty`) and does not consume redirected stdin.
- If no controlling terminal can be opened, exit 2 and suggest `--list` or `--json`.
- Minimum initial size is 60 columns × 10 rows; smaller terminals exit 2. Runtime resize is delegated to Skim.
- Mouse behavior is whatever Skim safely provides; documentation promises keyboard behavior only.
- Esc is normal cancellation, exit 0.
- Ctrl+C exits 130.
- Zero Sessions exits 0 with a short message.
- All integrations failing exits 1. Partial failure still opens the Picker.
- Cancellation never changes the caller's directory or launches an agent.

## 6. Resume safety and process handoff

Each integration returns structured launch state:

```text
program + argv + Workspace + narrow environment overrides
```

Actual execution uses a direct process API, never `sh -c`, `bash -c`, or a concatenated shell string. A shell-quoted command may be displayed for humans but is not executable state.

Before handoff:

1. Exit Skim and restore terminal state.
2. Retrieve the Session by opaque key.
3. Revalidate native Session identity, transcript/source existence, Workspace, agent executable, support status, and known risk evidence.
4. Enter the recorded Workspace.
5. Apply only integration-required environment overrides.
6. Call Unix `exec` so the agent owns the terminal, signals, and eventual exit status.

If final revalidation fails, do not reopen the Picker or choose a replacement; print the exact reason and exit 1.

### Agent CLI unavailable

- Still list discovered Sessions.
- Mark `Agent unavailable` and disable Resume.
- Preview shows the expected native command.
- Never install the agent or guess alternate executable locations.

### Active Sessions

Active Detection is optional per integration and does not affect Supported status.

- Report Active only from reliable positive association, such as an authoritative socket/lock or a validated process + Session path/TTY mapping.
- Do not infer Active from recent mtime alone.
- Do not infer Inactive when evidence is missing; show `Activity unknown`.
- When Active is known, Preview shows PID, process start, TTY, cwd, command, liveness, and detection confidence when available.
- Active does not forbid Resume. After Skim exits, show the evidence and ask the user.
- Never kill, terminate, steal, attach to, or switch to the existing process.

### Confirmation

- Ordinary ready Session: Enter resumes directly unless `confirm_always` is enabled.
- Active, Workspace changed, Conflicting metadata, and Broad Workspace always require confirmation.
- Risk confirmation happens after Skim exits in the normal terminal, not in a custom modal.
- Refusal exits 0 and does not reopen the Picker.
- Ctrl+C during confirmation exits 130.
- `--no-confirm` disables only ordinary always-confirm behavior; it never skips a risk confirmation.

### Exit status

- `0`: Esc/no results/declined risk confirmation.
- `1`: all integrations failed, final validation failed, or process launch failed.
- `2`: invalid CLI/config or an accepted unavailable candidate when Skim cannot keep it in Picker.
- `130`: Ctrl+C.
- After successful `exec`, the agent determines the eventual exit status.

## 7. CLI and configuration

### Query options

```text
resume [DIRECTORY]
  -U, --up <N|all>
  -D, --down <N|all>
      --all-worktrees      # widens the Git default Scope; conflicts with -U/-D
  -a, --agent <AGENT>      # repeatable
      --since <VALUE>
      --list
      --json               # implies --list; no need to pass both
      --verbose
      --config <PATH>
      --confirm-always
      --no-confirm
```

Agent names are case-insensitive. An unknown name is exit 2. A supported integration still scans its standard store when its CLI is missing. If any `--agent` occurs, the CLI list completely replaces configured agents rather than appending.

`--since` accepts:

- relative `30m`, `12h`, `7d`;
- absolute date `YYYY-MM-DD`, beginning at UTC midnight;
- `all`, which clears configured time filtering without changing Scope.

Use native last activity time, then documented fallback. When `--since` is active, exclude unknown-time Sessions. Invalid values exit 2.

Mutually exclusive or meaningless combinations are errors rather than silently ignored. In particular:

- `--up` and `--down` cannot coexist;
- `--json` implies `--list`; it is a self-sufficient flag and does not require `--list` to also be passed;
- list mode rejects confirmation options;
- `config example` and `completions` reject Session-query options.

### List mode

```bash
resume --list
resume --json
```

`--json` implies list-mode discovery; `resume --list --json` is accepted but redundant.

`--list` is an adaptive human table with:

```text
UPDATED AGENT[PROFILE] TITLE BRANCH
```

`UPDATED` is the latest native Session timestamp, falling back to the transcript file's modification time. `BRANCH` identifies the Workspace worktree; detached and non-Git workspaces render `detached` and `no-branch`.

It is not a stable machine format and does not switch automatically when stdout is redirected.

JSON is the only stable machine interface:

```json
{
  "schemaVersion": 1,
  "sessions": [],
  "errors": []
}
```

- stdout contains JSON only; diagnostics go to stderr.
- Output is one complete object, not JSON Lines.
- Within schema v1, fields may be added but not renamed or semantically changed.
- Sessions sort by final activity descending, unknown last.
- Error objects carry integration/category/count/safe summary.
- Neither table nor JSON includes user message bodies.
- Partial integration failure still returns results and exits 0; all failing exits 1.

### Configuration

Resolution, first match only:

1. `--config <path>`;
2. `$XDG_CONFIG_HOME/resume/config.toml`;
3. `~/.config/resume/config.toml`;
4. built-in defaults.

CLI overrides config. No project-level config and no merge of multiple files.

Supported TOML fields:

```toml
agents = ["codex", "claude", "pi", "omp"]
since = "all"
confirm_always = false
preview = "hidden"              # hidden | visible
preview_position = "auto"       # auto | right | bottom
verbose = false
```

- Built-in `since` is unrestricted and Preview is hidden.
- Scope direction, native commands, Session roots, filter regexes, and keybindings are not configurable.
- Unknown fields and invalid values are errors with file/line/column when possible, exit 2.
- A missing default file is normal and is not created.
- A missing explicit file is an error.
- `resume config example` prints an example to stdout and does not write a file.
- There is no `config path`, `config validate`, or `doctor` command.
- `--verbose` may enable configured false; there is no `--quiet` override.
- No custom `RESUME_*` environment variables. Only standard HOME/XDG/PATH and official agent environment variables are read.

### Diagnostics

- Default diagnostics summarize skipped/error counts per integration.
- `--verbose` adds safe source paths and error chains to stderr, never message bodies.
- JSON stdout does not change under `--verbose`.
- No diagnostic log file is written.
- `--json` (with or without `--list`) and `--verbose` together are the supported read-only diagnostic surface; there is no `resume doctor`.

### Completions

```bash
resume completions bash
resume completions zsh
resume completions fish
```

Scripts go to stdout. Homebrew installs them. Completion generation does not load config or scan Session stores and does not dynamically complete Session IDs or Workspaces.

## 8. Parsing, privacy, and security

### Live and damaged stores

- Parse files while they may be growing.
- Retain complete records up to the last complete record.
- Treat a partial final JSONL record as temporarily incomplete rather than corrupting the whole Session.
- Skip malformed middle records with a warning and continue when the agent format permits.
- Isolate failures per Session; a common unknown format may produce an integration-level `Unsupported session format` summary.
- Accept known older versions and compatible known fields; do not guess missing critical semantics.
- Every newly supported format gets its own fixture and regression test.
- Never lock, copy, repair, delete, checkpoint, migrate, or rewrite the native store.

SQLite stores are opened read-only (`mode=ro`) with a short busy timeout. Do not run migrations, write PRAGMAs, or WAL checkpoints, and do not use a derived database as the sole authority. A busy/corrupt/unknown database degrades the affected optional enrichment or integration without blocking others.

### Terminal safety

All Session-derived text is untrusted:

- strip or visibly encode ANSI, CSI, OSC, C0/C1, terminal title/clipboard operations, and bidirectional controls;
- fold list newlines and tabs to spaces;
- keep ordinary Unicode;
- never send Raw control bytes to the terminal;
- keep execution paths/argv as OS-native values rather than reconstructing them from sanitized display strings.

### Privacy

- Preview is hidden until explicitly opened unless configured visible.
- Normalized Preview shows real human user input; it does not attempt unreliable generic secret redaction.
- Raw requires explicit `Ctrl+R` and remains terminal-safe.
- List and JSON never contain message bodies.
- No telemetry.
- No persistent Preview/search cache or generated Preview file.
- Error logs never contain full message bodies.
- Documentation states that Preview reads and displays local Session content.

## 9. Testing and quality

Development may inspect real local Sessions read-only, but repository tests and CI use minimal synthetic, sanitized fixtures. Never commit real messages, identifiers, paths, credentials, private URLs, repository remotes, or attachment bodies.

### Integration contract coverage

Every agent integration covers:

- normal discovery and exact Resume construction;
- authoritative Workspace and allowed inference;
- title/timestamp/user-message/attachment extraction;
- alternate roots/profiles;
- malformed, incomplete, and live-growing records;
- duplicate identity and conflicting metadata;
- missing Workspace and missing CLI;
- read-only bytes/mtime/directory-entry snapshots.

Use fake agent executables to record cwd, argv, and environment. Parser tests never call native agents. The production `exec` path is tested only in a separate helper child so the main test process is not replaced. Do not expose a public test-only `--dry-run-resume` option.

### CI

On PRs and main:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check licenses bans advisories sources
```

- Ubuntu: full checks/tests.
- macOS: build/tests and fake-agent/TTY behavior.
- Rust 1.91: MSRV build.
- Stable Rust: full format/Clippy/tests.
- Controlled PTY tests cover Skim behavior without manual input.
- Release/periodic workflows build musl artifacts rather than slowing every PR.
- Skim dependency upgrades must rerun PTY interaction tests.
- Cargo uses normal compatible constraints, commits `Cargo.lock`, and CI/release runs `--locked`.
- Dependabot may submit weekly updates.

## 10. Distribution and release

The project uses MIT License and maintains a Keep a Changelog `CHANGELOG.md` with `Unreleased` and standard sections. Integration and Support List changes receive distinct entries. Release automation does not rewrite the Changelog; it is finalized manually before release.

### Artifacts

Initial targets:

- `aarch64-apple-darwin`;
- `x86_64-apple-darwin`;
- `aarch64-unknown-linux-musl`;
- `x86_64-unknown-linux-musl`.

Prefer static musl Linux binaries. Fall back to GNU only if audited dependency evidence requires it, then verify the minimum glibc floor across appropriate containers.

### Versioning

- Semantic Versioning, first release `v0.1.0`.
- `resume --version` prints `resume 0.1.0`.
- No self-update or network update check.
- Main builds do not update Homebrew; only formal Releases do.
- Breaking JSON changes increment `schemaVersion`.

### Homebrew

Primary installation:

```bash
brew install luw2007/tap/resume
```

- Main repository: `luw2007/resume`.
- Tap repository: `luw2007/homebrew-tap`.
- Formula distributes prebuilt GitHub Release binaries, not source compilation.
- Formula selects OS/architecture and pins SHA-256.
- Preserve developer installation with `cargo install --git https://github.com/luw2007/resume`.
- Do not publish crates.io in v0.1.0.

Formal artifacts are built only in GitHub Actions, with SHA-256 and GitHub artifact attestation. Tags matching `v*` trigger Release from protected main. No local formal release and no GPG/minisign key in v0.1.0.

A fine-grained personal access token, scoped only to the `luw2007/homebrew-tap` repository with `Contents: Read and write` and a 1-year expiration, updates `Formula/resume.rb` (`HOMEBREW_TAP_PAT` secret on `luw2007/resume`). Tap failure is independently retryable and never causes rebuilding or retagging the release. Rotate the token before expiry; a GitHub App was considered for shorter-lived credentials but the fine-grained PAT was chosen for lower setup overhead.

Formula verifies `resume --version` and `resume --help`; a controlled TTY environment separately smokes Picker startup.

## 11. v0.1.0 acceptance criteria

Release only when all are true:

- Codex, Claude Code, Pi, and OMP are each Supported, not Discover Only.
- Skim proves streaming candidates, fuzzy filtering, Preview, `Ctrl+O`, stable opaque selection, `/dev/tty`, cancellation, and terminal restoration without a fork.
- Exact cwd/argv/environment launch contracts pass with fake executables.
- Local read-only validation passes for all four installed integrations.
- macOS/Linux CI and PTY tests pass.
- Homebrew installation smoke test passes.
- Fixture privacy and dependency/license reviews pass.
- Support List and Changelog match actual capability.

Second-batch agents do not block v0.1.0.

## 12. Explicit non-goals

- Windows support.
- Machine-wide Session scanning/listing.
- Project configuration.
- Dynamic integration plugins or agent installation.
- Session modification, repair, migration, deletion, import, merge, or cross-agent deduplication.
- Automatic replacement of a missing worktree.
- Continuous watching or refresh.
- Persistent transcript/full-text search index (a user-facing "search across all Session content" feature): rejected. Distinct from the narrow, non-authoritative Codex discovery cache (see "Codex parallel scan and discovery cache" in section 5), which adds no search capability and exists purely to skip re-parsing an unchanged rollout file.
- External pager/editor.
- Custom Skim fork or direct Ratatui UI.
- Automatic agent install, process termination, terminal takeover, or Resume fallback to “latest.”
- Cursor/OpenCode/Grok/Gemini in v0.1.0.

## 13. Superseded interview proposals

The following appeared during exploration but are not part of the final design:

- `-L/--level`: replaced by mutually exclusive `-U/--up` and `-D/--down`, where numeric values are path-edge distances and `all` means unbounded in that direction.
- `--max`, `--up-max`, and `--down-max`: replaced by `--up all` and `--down all`.
- machine-wide `--all`: rejected; there is no machine-wide Scope.
- combining up and down: rejected.
- treating `1` as the current directory: replaced by distance `0` for current-only.
- custom Raw Ratatui/Crossterm/Tokio UI: replaced by embedded Skim as the complete picker.
- `tui-realm`, Nucleo, fzf, Cursive, inquire, and dialoguer: researched but not selected.
- a separately installed integration/plugin package system: rejected as excessive for a small tool.
- `aresume`: rejected; the formal command is `resume`.
- child-process wrapper around the native agent: replaced by terminal restoration and Unix `exec`.
- accepting “continue latest” as full support: rejected; exact stable-ID/path Resume is required.
- automatically blocking Active Sessions: rejected; show evidence, confirm, and leave the decision to the user.
- silently replacing a deleted Workspace/worktree: rejected.
- default age limits, result limits, and pagination: rejected.
- generic XML deletion and generic secret redaction: rejected in favor of source-aware filtering, exact wrapper fallback, explicit Preview, and terminal safety.
- a `doctor`, `config path`, `config validate`, or `--quiet` surface: rejected.
- source-built Homebrew Formula and crates.io v0.1.0 publication: rejected.
