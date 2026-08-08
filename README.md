# resume

**Find and resume the right coding-agent session from your current project or worktree.**

`resume` discovers local Pi, Claude Code, Codex, and OMP Sessions in a directory-derived Scope, lets you inspect and fuzzy-filter them in a Skim picker, and then replaces itself with the selected agent's native Resume command. Discovery and Preview are read-only. It never performs a machine-wide scan.

> v0.1.0 targets macOS and Linux terminals. Windows support is not claimed.

## Install

Rust 1.91 or newer is required:

```sh
cargo install --git https://github.com/luw2007/resume
```

The `resume` crate is **not published to crates.io for v0.1.0**.

## Quick start

```sh
resume                         # picker for the default Scope
resume /path/to/worktree       # derive Scope from another directory
resume --list                  # stable text listing after discovery completes
resume --json                  # machine-readable v1 output
resume -a pi -a codex          # replace the configured agent list
resume --since 7d              # only Sessions active in the last 7 days
resume --since 2026-01-01      # only Sessions active on or after a date
resume --since all             # no time filtering (default)
```

The interactive picker receives candidates asynchronously. Late candidates can change Skim's visible rank order, but selection remains attached to an opaque Session identity rather than a row number. Do not rely on exact global display order while loading.

## Scope and Directory Distance

A **Workspace** is the directory recorded by an agent for a Session. A **Scope** is the set of Sessions considered for listing; `resume` reads only the known Session roots of enabled integrations and filters their recorded Workspaces. It does not recursively search the machine, unrelated sibling trees, or arbitrary directories for transcripts.

### Default Scope

Inside Git, the default Scope includes Workspaces at any depth in the current repository's main and linked worktrees. A distinct nested repository is excluded.

```text
/repo                 included
/repo/services/api    included
/linked-worktree      included
/repo/vendor/other    excluded when it is a distinct nested repository
/unrelated            excluded
```

Outside Git—or if Git Scope discovery fails—the fallback is the exact real base directory only, not its children. A Git failure is reported as a diagnostic.

### Explicit direction

`-U/--up` and `-D/--down` are mutually exclusive and override the Git default. **Directory Distance** counts real path-component edges in one direction:

```sh
resume --up 0          # current real directory only
resume --up 2          # current directory and at most two ancestors
resume --up all        # current directory through filesystem root
resume --down 0        # current real directory only
resume --down 2        # current directory and descendants at most two edges deep
resume --down all      # every descendant of the current directory
```

Upward Scope never includes children; downward Scope never includes ancestors or siblings. At `/`, `--down all` covers all descendants and is therefore broad, but it is still a directory-derived Scope rather than a separate machine-wide mode. Input paths are canonicalized, so symlinks use their real targets; a missing base directory is a usage error. Sessions whose recorded Workspace no longer exists may be diagnosed as unavailable and cannot be resumed.

## Picker and Preview

The Skim picker starts with Preview hidden unless config says otherwise.

| Key | Behavior |
|---|---|
| `Ctrl-O` | Toggle Preview |
| `Ctrl-R` | Intentionally ignored |
| `Esc` | Cancel without resuming |
| `Ctrl-C` | Interrupt (exit 130) |

Preview uses a safe dual-section fallback: normalized and raw-but-terminal-safe sections are shown together. **Ctrl-R does not reload, refresh, or switch views live.** A channel-fed Skim `reload` can execute its default filesystem command, so `resume` explicitly binds Ctrl-R to `ignore` to prevent an accidental filesystem listing. Preview currently presents the Session metadata/title available to the assembled picker; integration parsers and text-safety foundations have broader user-input coverage, but this README does not claim a full native transcript viewer.

## List and JSON output

```sh
resume --list
resume --json
resume /path/to/repo --json -a claude -a codex
```

`--list` waits for discovery, sorts the collected Sessions deterministically, and prints one terminal-safe row per Session. `--json` writes exactly one JSON document to stdout. Diagnostics go to stderr, so stdout can be piped safely:

```sh
resume --json 2>resume.errors | jq '.sessions[] | {agent, id, workspace}'
```

The v1 envelope is:

```json
{"schemaVersion":1,"sessions":[],"errors":[]}
```

Session objects contain `agent`, `profile`, `id`, `title`, `workspace`, `support`, `activity`, and `risk`. Error objects contain only `category` and `count`. JSON output contains no message bodies. See [`docs/json-schema.md`](docs/json-schema.md) for the complete schema and serialization notes.

## Configuration

Configuration is strict TOML. Exactly one file is loaded; files are not merged. Precedence is:

1. `--config <PATH>`
2. `$XDG_CONFIG_HOME/resume/config.toml`, when it exists
3. `~/.config/resume/config.toml`, when it exists
4. built-in defaults

Unknown fields and invalid values are rejected. Print a complete example with:

```sh
resume config example
```

```toml
agents = ["codex", "claude", "pi", "omp"]
since = "all"                    # duration (7d, 2h, 30m, 1w) | YYYY-MM-DD | all
confirm_always = false
preview = "hidden"               # hidden | visible
preview_position = "auto"        # auto | right | bottom
verbose = false
```

A repeatable `-a/--agent <AGENT>` replaces, rather than extends, the configured `agents` list. `--confirm-always` requests confirmation for every Resume. `--no-confirm` suppresses ordinary confirmation but cannot bypass a risk prompt. CLI `--verbose` enables verbose diagnostics.

`--since <duration|date|all>` filters Sessions to those active at or after a cutoff, overriding a configured `since` the same way `--agent` overrides `agents`. A relative duration is `<N>` followed by `m` (minutes), `h` (hours), `d` (days), or `w` (weeks); an absolute cutoff is `YYYY-MM-DD` (UTC midnight); `all` (the default) applies no filtering. The cutoff compares against each Session's best-available activity signal, falling back to the transcript file's own modification time when no other signal is available — this is Discovery-time filtering, not a claim that every agent's own activity timestamp is used.

## Native Resume boundary

After selection, `resume` restores the terminal, looks up the structured Session by opaque key, revalidates launch evidence, changes to the recorded Workspace, and uses discrete program/argv/environment values—never a shell command string.

| Agent | Native invocation constructed by the integration | Isolation preserved when applicable |
|---|---|---|
| Pi | `pi --session <absolute-jsonl-path>` | custom `--session-dir`; never `--session-id` |
| Claude Code | `claude --resume <uuid>` | nondefault `CLAUDE_CONFIG_DIR` |
| Codex | `codex -C <workspace> resume <uuid>` | nondefault `CODEX_HOME` |
| OMP default profile | `omp --resume <id>` | config/root environment and custom `--session-dir` |
| OMP named profile | `omp --profile <name> --resume <id>` | profile, config/root environment, and custom `--session-dir` |

The recorded Workspace is the child working directory. Missing or changed Workspaces and other risky evidence prevent or confirm Resume as appropriate. These are exact launcher contracts tested by fake native executables; they are not claims that `resume` reproduces each agent's native title-ranking behavior.

## Support list

“Supported” below means the corresponding integration tests prove that capability. Active detection is positive-evidence-only: failure to prove Active remains `Unknown`, never `Inactive`. The assembled app currently supplies no live-correlation evidence to Pi or OMP, and Codex/Claude Sessions therefore normally report `Unknown`.

| Agent | Discovery | Preview parsing | Exact Resume | Profiles | Active Detection |
|---|---|---|---|---|---|
| Pi | Supported | Supported | Supported | Not applicable | Conditional: validated ID + Session path evidence; Unknown by default |
| Claude Code | Supported | Supported | Supported | Not applicable | Unknown (no proven correlation) |
| Codex | Supported | Supported | Supported | Not applicable | Unknown by default; integration design permits only exact rollout/process positive evidence |
| OMP | Supported | Supported | Supported | Supported | Conditional: live process + TTY + matching breadcrumb; Unknown by default |

Preview parsing here means read-only extraction and terminal-safe normalization are covered by integration/foundation tests. It does not promise native title precedence or a full transcript in the current production picker.

## Privacy and diagnostics

- Discovery and Preview do not modify Session files, indexes, databases, mtimes, or directory entries.
- No telemetry is collected.
- Message bodies are not logged and are absent from JSON output.
- Normal diagnostics contain redacted categories and counts. `--verbose` may add source paths and error chains to **stderr**, but still does not log message bodies or sensitive remotes/URLs.
- Preview and list text neutralize terminal control sequences; raw Preview remains terminal-safe.
- There is no persistent Preview cache and no machine-wide scan.

If one integration fails, the others can still return Sessions; failures are isolated and summarized. Use `resume --verbose --list` or `resume --verbose --json` when diagnosing discovery.

## Shell completions

Generate Bash, Zsh, or Fish completion scripts with the built-in subcommand:

```sh
resume completions bash
resume completions zsh
resume completions fish
```

Installation examples and the repository-generated scripts are documented in [`docs/completions.md`](docs/completions.md). Completion generation dispatches before application startup, so it does not load configuration or discover/scan Sessions.

## License

MIT
