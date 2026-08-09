# Three-layer CLI help + unified E-code error catalog

Status: design, ready to implement
Scope: `src/cli.rs`, `src/main.rs`, `src/lib.rs`, new `src/errors.rs`, new `src/man.rs`, `src/app.rs`, `src/integration/**`, `docs/product-design.md`
Audience: the implementer. **Every user-visible string in this document is final text.** Copy it verbatim. Do not paraphrase, do not re-wrap, do not "improve" wording while pasting.

---

## 1. Problem statement

### 1.1 The verified defect

`src/cli.rs` declares twelve `Cli` fields and two subcommands. **Not one of them carries a doc comment or a `help = "..."` attribute.** Concretely, the current declarations are bare:

```rust
pub directory: Option<PathBuf>,

#[arg(short = 'U', long, value_name = "N|all", conflicts_with = "down", allow_hyphen_values = true)]
pub up: Option<Distance>,
```

Because clap derives per-argument descriptions from the doc comment or from `help =`, and neither exists, `cargo run -- --help` renders a column of flags with **blank descriptions**. The only text a user sees today is the `about` one-liner on `#[command(...)]`, `Find and resume coding-agent Sessions`, plus clap's own auto-generated `-h, --help` / `-V, --version` rows.

The same hole propagates into generated shell completions. `completions/_resume:152-153` is checked in and proves it:

```zsh
'config:' \
'completions:' \
'help:Print this message or the help of the given subcommand(s)' \
```

`config` and `completions` emit an empty description after the colon; only clap's built-in `help` subcommand has text, because clap supplies that string itself. A zsh user pressing `<TAB>` after `resume ` sees two undocumented commands.

There is also no manual page of any kind. `resume` has:

- four exit codes (`src/app.rs:30-33`) documented only in `docs/product-design.md:393-399`, and **not at all** in `README.md`;
- twelve explicit non-goals (`docs/product-design.md:656-669`) that a user will never encounter from the CLI;
- a stable JSON v1 contract (`docs/json-schema.md`) reachable only by reading the repository;
- six fatal error sites in `src/app.rs` (`:63`, `:70`, `:77`, `:266`, `:277`, `:294`) that print a bare `resume: {error}` with no code, no cause, and no remedy;
- roughly twenty aggregated discovery diagnostic categories that surface as opaque snake_case tokens such as `git_scope_discovery_failed` with no published meaning.

### 1.2 Goal of the three layers

Introduce **progressive disclosure**: three fixed levels of detail, each with a distinct audience, a distinct clap mechanism, and a distinct length budget.

1. **Layer 1 — `resume -h`.** Orientation in one screen. The user who typed `resume` for the first time, or who forgot the flag spelling. Syntax skeleton plus a handful of copy-paste examples. Must fit on a default 80x24 terminal without scrolling.
2. **Layer 2 — `resume --help`.** Complete reference for every flag. One `<=60`-character description per argument so the option column never wraps, plus a `long_about` that explains what a Session is, plus an `after_long_help` with common examples, the common failure modes keyed by E-code, and a `SEE ALSO`.
3. **Layer 3 — `resume --man`.** The full manual: prose per option, enumerations, the complete `--json` v1 schema, exit codes, all seven E-codes with trigger and fix, caveats, and compatibility rules. Static text, no roff, no runtime generation.

Alongside the three layers, this plan introduces a **single source of truth for user-facing error strings**: `const CATALOG: [ErrorSpec; 7]` in a new `src/errors.rs`. The same seven records feed the stderr error block, the man page `ERRORS` section, and the category -> code back-mapping. Nothing user-visible is written twice.

---

## 2. Layer model

| Layer | Command | Clap mechanism | Audience | Length budget |
|---|---|---|---|---|
| 1 | `resume -h` | `#[command(about = ..., after_help = ...)]` | first-time user, forgot a flag spelling | one screen, target <= 30 lines, hard ceiling 34 lines incl. clap's own option column |
| 2 | `resume --help` | `#[command(long_about = ..., after_long_help = ...)]` plus `#[arg(help = "...")]` on every field | regular user needing the full flag reference | 70-90 lines; every per-arg `help` <= 60 characters |
| 3 | `resume --man` | `#[arg(long, exclusive = true)] pub man: bool` -> `resume::man::page() -> &'static str` | integrator, scripter, someone debugging a diagnostic | 200-350 lines, static |

Rules that hold across all three layers:

- **Layer 1 text is a strict subset of Layer 2 text.** `about` is the first sentence of `long_about`; the `-h` `EXAMPLES` block is a subset of the `--help` `COMMON EXAMPLES` block.
- **Layer 2 text is a strict subset of Layer 3 text.** Every `--help` example appears in the man page `EXAMPLES` section; every E-code named in `--help` is fully specified in the man page `ERRORS` section.
- **Layer 3 is the only place a string may appear for the first time.** If a user-visible fact is not in the man page, it is not in the product.
- Layer 1 and Layer 2 never contain a fact the man page contradicts. When they disagree during review, the man page wins and the shorter layer is corrected.

Clap's behaviour that makes this work, and which the implementer must not fight:

- If `long_about` is set, `--help` uses it and `-h` uses `about`. If `about` alone is set, both use it.
- If `after_long_help` is set, `--help` uses it and `-h` uses `after_help`. If only `after_help` is set, both use it.
- Per-argument, clap uses `help` for `-h` and `long_help` for `--help`, falling back to `help` when `long_help` is absent. **This plan deliberately sets only `help`** — the long prose lives in the man page, not in `long_help`, so the `--help` option column stays scannable.

---

## 3. Files touched

| Path | New or modified | What changes |
|---|---|---|
| `src/cli.rs` | modified | Add `help = "..."` to all twelve `#[arg]`/positional fields on `Cli` (`directory`, `up`, `down`, `agent`, `since`, `list`, `json`, `verbose`, `config`, `confirm_always`, `no_confirm`) plus the **new** thirteenth field `man`. Replace the `#[command(...)]` attribute on `Cli` with the four-part `name`/`version`/`about`/`long_about`/`after_help`/`after_long_help` form from §5e. Add `#[command(about = ..., long_about = ...)]` to `Command::Config`, `Command::Completions`, `ConfigCommand::Example`, and `#[value(help = ...)]` to each `Shell` variant. Change `Distance::from_str` and `Since::from_str` to source their error strings from `crate::errors::E1003.parser_message()` and `crate::errors::E1001.parser_message()` — the returned bytes are identical to today's literals, so `src/cli.rs:243 invalid_distance_and_since_are_usage_errors` stays green. `config_example()` at `src/cli.rs:214` is untouched and remains the shape template for `man::page()`. |
| `src/main.rs` | modified | Insert one early branch immediately after `cli.validate()` and **before** the `match &cli.command` block: `if cli.man { print!("{}", resume::man::page()); return; }`. Nothing else in `main` changes; the `Config`/`Completions`/`None` arms are byte-identical. |
| `src/lib.rs` | modified | Two additions to the flat alphabetical `pub mod` block: `pub mod errors;` between `pub mod diagnostics;` and `pub mod injection;`, and `pub mod man;` between `pub mod launch;` and `pub mod message;`. |
| `src/errors.rs` | **new** | The complete catalog. `pub struct ErrorSpec`, `pub mod category` with the eighteen token consts, `pub const CATALOG: [ErrorSpec; 7]`, seven per-code consts (`E1001`..`E3003`), lookup functions `catalog()`, `by_code()`, `by_slug()`, `for_category()`, the constructors `ErrorSpec::parser_message()`, `ErrorSpec::report()`, `ErrorSpec::report_with()`, `pub struct Report` with `spec()`/`exit_code()`/`what()`/`emit()`, `Display` for both types rendering the four-line block, and the unit-test module. Full source in §6.4. |
| `src/man.rs` | **new** | Exactly one item: `pub fn page() -> &'static str` returning one raw string literal with a trailing newline. Modelled byte-for-byte on the shape of `src/cli.rs:214 config_example()`. Full text in §7.3. |
| `src/app.rs` | modified | Three localised changes. (a) The six fatal sites currently printing a bare `resume: {error}` — `:63` config load, `:70` unknown agent, `:77` build scope, `:266` unavailable session, `:277` revalidate failure, `:294` exec failure — are rewritten to `return crate::errors::E1004.report(error.to_string()).emit();` and siblings; `emit()` writes the four-line block to stderr and returns the spec's `exit_code`, preserving today's exit values (2, 2, 2, 2, 1, 1). (b) `render_diagnostic` at `src/app.rs:851-870` is **not changed**; the aggregated `resume: {category}: {count}` line stays byte-identical so `src/app.rs:1037 git_scope_failure_becomes_a_visible_diagnostic` stays green. (c) The `Cli` struct literal inside `src/app.rs:1053 unknown_agent_is_usage_error` gains one line, `man: false,`, because `Cli` gained a field. `print_json` at `src/app.rs:741` and the `JsonOutput`/`JsonSession`/`JsonError` structs at `src/app.rs:718-740` are **not changed** — see §6.2. |
| `src/integration/**` | modified (comments only) | No behavioural change. The category string literals in `src/integration/claude/mod.rs:197,425,532`, `src/integration/codex/mod.rs:443,906`, and `src/integration/codex/sqlite.rs:144-186,528,541` stay as bare `&'static str` literals. Optionally — and this is the only edit permitted here — replace each literal with the matching `crate::errors::category::*` const so the tokens have one definition site. This is a pure rename with zero output change; if `cargo test` is not green immediately after, revert it and keep the literals. |
| `docs/product-design.md` | modified | Two stale-line corrections, exact before/after in §9: line 424 (`--since YYYY-MM-DD` is UTC midnight, not local midnight) and line 448 (the `--list` column order is `STATUS AGENT[PROFILE] UPDATED TITLE BRANCH WORKSPACE`, not `STATUS AGENT UPDATED BRANCH TITLE WORKSPACE`). |
| `docs/json-schema.md` | **not touched in this PR** | `additionalProperties: false` at `docs/json-schema.md:60` (on `$defs/error`) is the reason no `code` field is added to `--json`. Relaxing it is separate follow-up work; see §10. |

---

## 4. Layer 1 — `resume -h`, verbatim

The `-h` output is `about`, then clap's auto-generated `Usage`/`Arguments`/`Options` block, then `after_help`. The implementer writes only the first and the last.

### 4.1 The `about` one-liner

Unchanged from today, deliberately: `Find and resume coding-agent Sessions`. It is already correct, it is already the `about` value in `src/cli.rs`, and keeping it byte-identical means the checked-in `completions/_resume` top-level description does not churn in this PR.

### 4.2 The literal Rust attribute

Paste this as the `#[command(...)]` attribute fragment on `Cli`. (The full attribute including Layer 2's `long_about`/`after_long_help` is in §5e; this block shows the Layer 1 halves in isolation so they can be reviewed on their own.)

```rust
#[command(
    name = "resume",
    version,
    about = "Find and resume coding-agent Sessions",
    after_help = "\
SYNTAX
  resume [DIRECTORY] [OPTIONS]
  resume config example
  resume completions <bash|zsh|fish>

EXAMPLES
  resume                     Pick a Session in the current project
  resume --up all            Search every ancestor directory too
  resume -a codex --since 7d Recent Codex Sessions only
  resume --json              Machine-readable JSON v1 on stdout

`resume --help` has full descriptions; `resume --man` has the manual.\
"
)]
```

### 4.3 Length budget check

| Part | Lines |
|---|---|
| `about` + blank | 2 |
| `Usage:` + blank | 2 |
| `Arguments:` heading + `[DIRECTORY]` row + blank | 3 |
| `Options:` heading + 13 declared rows (`-U`, `-D`, `-a`, `--since`, `--list`, `--json`, `--verbose`, `--config`, `--confirm-always`, `--no-confirm`, `--man`) + `-h` + `-V` + blank | 15 |
| `after_help` | 11 |
| **Total** | **33** |

Thirty-three lines, three over the 30-line target and one under the 34-line hard ceiling. If a reviewer wants it strictly under 30, the only sanctioned lever is deleting the `resume --up all` example line and the `resume config example` / `resume completions` lines from `SYNTAX`, which brings the total to 30. Do not shorten the per-argument `help` strings to save vertical space — they are one line each regardless.

---

## 5. Layer 2 — `resume --help`, verbatim

### 5a. Per-field help strings

Every string below is <= 60 characters. The `len` column is the exact character count; the implementer must not alter a string without re-counting.

| # | Field | clap attr | help text | len | Type | Required | Default | Constraints |
|---|---|---|---|---|---|---|---|---|
| 1 | `directory` | positional (no `#[arg]` beyond `help`) | `Directory whose Sessions to search (default: current dir)` | 57 | `Option<PathBuf>` | no | current working directory (`.`) | Must be an existing directory; canonicalised by `crate::scope::canonical_base`. Rejected alongside `config example` / `completions` by `Cli::validate`. |
| 2 | `up` | `short = 'U', long, value_name = "N|all", conflicts_with = "down", allow_hyphen_values = true` | `Include ancestor directories up to N edges, or all` | 50 | `Option<Distance>` | no | none (Git-derived scope) | `N` is a non-negative integer, or the literal `all`. Conflicts with `--down`. `allow_hyphen_values` exists so `--up -1` reaches the parser and produces E1003 rather than "unknown flag". |
| 3 | `down` | `short = 'D', long, value_name = "N|all", conflicts_with = "up", allow_hyphen_values = true` | `Include descendant directories down to N edges, or all` | 54 | `Option<Distance>` | no | none (Git-derived scope) | Same grammar as `--up`. Conflicts with `--up`. |
| 4 | `agent` | `short = 'a', long, action = clap::ArgAction::Append` | `Only this agent; repeatable; replaces configured agents` | 55 | `Vec<OsString>` | no | `["codex", "claude", "pi", "omp"]` or the config `agents` list | Case-insensitive. One of `codex`, `claude`, `pi`, `omp`. Any occurrence **replaces** the configured list rather than appending to it. Unknown name is exit 2. |
| 5 | `since` | `long, value_name = "duration|date|all"` | `Only Sessions active at or after this cutoff` | 44 | `Option<Since>` | no | config `since`, else `all` | `<N>m|h|d|w`, or `YYYY-MM-DD` (UTC midnight), or `all`. Replaces the configured `since`. When active, Sessions with unknown activity time are excluded. |
| 6 | `list` | `long` | `Print the plain table instead of opening the picker` | 51 | `bool` | no | `false` | Rejected together with `--confirm-always` / `--no-confirm`. |
| 7 | `json` | `long` | `Print JSON v1 to stdout; implies --list` | 39 | `bool` | no | `false` | Implies `--list`; passing both is accepted and redundant. Rejected together with `--confirm-always` / `--no-confirm`. |
| 8 | `verbose` | `long` | `Include redacted paths and error chains in diagnostics` | 54 | `bool` | no | config `verbose`, else `false` | Affects stderr only. Never adds fields to `--json`. |
| 9 | `config` | `long` | `Read this config file instead of the discovered one` | 51 | `Option<PathBuf>` | no | `$XDG_CONFIG_HOME/resume/config.toml`, else `$HOME/.config/resume/config.toml` | Configuration files are never merged; exactly one file is selected. A read or parse failure is E1004, exit 2. |
| 10 | `confirm_always` | `long, conflicts_with = "no_confirm"` | `Ask for confirmation before every Resume` | 40 | `bool` | no | config `confirm_always`, else `false` | Conflicts with `--no-confirm`. Rejected together with `--list` / `--json`. |
| 11 | `no_confirm` | `long, conflicts_with = "confirm_always"` | `Skip ordinary confirmation; risk prompts still apply` | 52 | `bool` | no | `false` | Conflicts with `--confirm-always`. Rejected together with `--list` / `--json`. **Never** suppresses a risk confirmation. |
| 12 | `man` | `long, exclusive = true` | `Print the full manual page and exit` | 35 | `bool` | no | `false` | `exclusive = true`: clap rejects `--man` in the presence of any other argument, exit 2. Handled in `main` before the subcommand match. |

Field 13, `command: Option<Command>`, keeps `#[command(subcommand)]` and carries no `help` of its own; its description comes from the per-variant `about` values in §5d.

### 5b. `long_about`, verbatim

```text
Find and resume coding-agent Sessions.

`resume` scans the local on-disk stores of the coding agents you use --
Codex, Claude Code, Pi, and OMP -- collects the Sessions that belong to
the directory you are standing in, and hands the one you pick back to
its own agent using that agent's native resume invocation.

Nothing is copied, rewritten, indexed, or uploaded. Discovery is
read-only. Resume is an exec into the agent's own CLI with the recorded
working directory, argv, and environment restored.

By default the scope is the Git repository containing the current
directory, including its linked worktrees. Use -U/--up and -D/--down to
walk the directory tree instead, and --since to hide stale Sessions.

Without --list or --json, `resume` opens an interactive picker. With
either flag it prints once and exits, so it is safe in scripts and CI.
```

### 5c. `after_long_help`, verbatim

```text
COMMON EXAMPLES
  resume
      Pick a Session from the current Git repository and its worktrees.

  resume ~/src/api
      Pick a Session scoped to another directory without leaving here.

  resume --up all
      Widen the scope to every ancestor directory, unbounded.

  resume --down 2
      Widen the scope to descendants at most two path edges away.

  resume -a codex -a claude --since 7d
      Only Codex and Claude Code Sessions active in the last week.

  resume --list
      Print the adaptive human table and exit; never opens the picker.

  resume --json | jq '.sessions[] | .agent, .title'
      Print JSON v1 and post-process it. --json implies --list.

  resume config example > ~/.config/resume/config.toml
      Write a commented starter configuration file.

  resume completions zsh > ~/.zfunc/_resume
      Install shell completions for zsh.

COMMON ERRORS
  E1001 INVALID_SINCE           --since value is not a duration, a
                                YYYY-MM-DD date, or `all`.
  E1002 CONFLICTING_DIRECTION   --up and --down were both supplied.
  E1003 INVALID_DISTANCE        --up/--down value is not a non-negative
                                integer or `all`.
  E1004 INVALID_CONFIG          The configuration file could not be read
                                or parsed.
  E3001 ROOT_UNAVAILABLE        An agent's on-disk store is missing or
                                unreadable.
  E3002 GIT_SCOPE_DISCOVERY_FAILED
                                Git scope could not be determined; the
                                scope fell back to the exact directory.
  E3003 WORKSPACE_UNAVAILABLE   The selected Session's workspace no
                                longer exists or is not resumable.

  Run `resume --man` for each code's trigger, fix, and worked example.

SEE ALSO
  resume --man            The full manual: options, enums, JSON schema,
                          exit codes, errors, caveats, compatibility.
  resume config example   A commented starter configuration file.
  resume completions      Shell completion scripts for bash, zsh, fish.
```

### 5d. Subcommand and value-enum text, verbatim

**`config`**

```text
about      = "Inspect resume configuration"
long_about = "Inspect resume configuration.

`resume` reads exactly one configuration file and never merges several.
The file is $XDG_CONFIG_HOME/resume/config.toml when XDG_CONFIG_HOME is
set, otherwise $HOME/.config/resume/config.toml, unless --config named a
different path.

This subcommand does not scan Sessions, so it rejects every
Session-query option and the bare DIRECTORY positional."
```

**`config example`**

```text
about      = "Print a commented example configuration file"
long_about = "Print a commented example configuration file.

Writes a complete, valid TOML document to stdout with every supported
key set to its default. Redirect it into place to start from a known
good file:

    resume config example > ~/.config/resume/config.toml

The output round-trips through the same deserializer the runtime uses,
so a file produced this way always parses."
```

**`completions`**

```text
about      = "Print a shell completion script to stdout"
long_about = "Print a shell completion script to stdout.

The script is generated from the live command definition, so it always
matches the flags this binary actually accepts. Redirect it to the
location your shell expects:

    resume completions bash > /etc/bash_completion.d/resume
    resume completions zsh  > ~/.zfunc/_resume
    resume completions fish > ~/.config/fish/completions/resume.fish

This subcommand does not scan Sessions, so it rejects every
Session-query option and the bare DIRECTORY positional."
```

**`Shell` value enum** — one `#[value(help = "...")]` per variant, each <= 60 characters:

| Variant | Value | `#[value(help = ...)]` | len |
|---|---|---|---|
| `Bash` | `bash` | `Bash completion script for bash-completion` | 42 |
| `Zsh` | `zsh` | `Zsh completion script for the zsh compsys autoloader` | 52 |
| `Fish` | `fish` | `Fish completion script for fish shell` | 37 |

### 5e. The fully annotated `Cli` struct and `Command` enum

Paste this over the existing definitions in `src/cli.rs`. It is complete: the `#[command]` attribute carries both Layer 1 and Layer 2 text, every field carries its `help`, and the new `man` field is in place.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Shell {
    /// Bash completion script for bash-completion
    #[value(help = "Bash completion script for bash-completion")]
    Bash,
    /// Zsh completion script for the zsh compsys autoloader
    #[value(help = "Zsh completion script for the zsh compsys autoloader")]
    Zsh,
    /// Fish completion script for fish shell
    #[value(help = "Fish completion script for fish shell")]
    Fish,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    #[command(
        about = "Inspect resume configuration",
        long_about = "\
Inspect resume configuration.

`resume` reads exactly one configuration file and never merges several.
The file is $XDG_CONFIG_HOME/resume/config.toml when XDG_CONFIG_HOME is
set, otherwise $HOME/.config/resume/config.toml, unless --config named a
different path.

This subcommand does not scan Sessions, so it rejects every
Session-query option and the bare DIRECTORY positional.\
"
    )]
    Config(ConfigArgs),

    #[command(
        about = "Print a shell completion script to stdout",
        long_about = "\
Print a shell completion script to stdout.

The script is generated from the live command definition, so it always
matches the flags this binary actually accepts. Redirect it to the
location your shell expects:

    resume completions bash > /etc/bash_completion.d/resume
    resume completions zsh  > ~/.zfunc/_resume
    resume completions fish > ~/.config/fish/completions/resume.fish

This subcommand does not scan Sessions, so it rejects every
Session-query option and the bare DIRECTORY positional.\
"
    )]
    Completions { shell: Shell },
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ConfigCommand {
    #[command(
        about = "Print a commented example configuration file",
        long_about = "\
Print a commented example configuration file.

Writes a complete, valid TOML document to stdout with every supported
key set to its default. Redirect it into place to start from a known
good file:

    resume config example > ~/.config/resume/config.toml

The output round-trips through the same deserializer the runtime uses,
so a file produced this way always parses.\
"
    )]
    Example,
}

#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(
    name = "resume",
    version,
    about = "Find and resume coding-agent Sessions",
    long_about = "\
Find and resume coding-agent Sessions.

`resume` scans the local on-disk stores of the coding agents you use --
Codex, Claude Code, Pi, and OMP -- collects the Sessions that belong to
the directory you are standing in, and hands the one you pick back to
its own agent using that agent's native resume invocation.

Nothing is copied, rewritten, indexed, or uploaded. Discovery is
read-only. Resume is an exec into the agent's own CLI with the recorded
working directory, argv, and environment restored.

By default the scope is the Git repository containing the current
directory, including its linked worktrees. Use -U/--up and -D/--down to
walk the directory tree instead, and --since to hide stale Sessions.

Without --list or --json, `resume` opens an interactive picker. With
either flag it prints once and exits, so it is safe in scripts and CI.\
",
    after_help = "\
SYNTAX
  resume [DIRECTORY] [OPTIONS]
  resume config example
  resume completions <bash|zsh|fish>

EXAMPLES
  resume                     Pick a Session in the current project
  resume --up all            Search every ancestor directory too
  resume -a codex --since 7d Recent Codex Sessions only
  resume --json              Machine-readable JSON v1 on stdout

`resume --help` has full descriptions; `resume --man` has the manual.\
",
    after_long_help = "\
COMMON EXAMPLES
  resume
      Pick a Session from the current Git repository and its worktrees.

  resume ~/src/api
      Pick a Session scoped to another directory without leaving here.

  resume --up all
      Widen the scope to every ancestor directory, unbounded.

  resume --down 2
      Widen the scope to descendants at most two path edges away.

  resume -a codex -a claude --since 7d
      Only Codex and Claude Code Sessions active in the last week.

  resume --list
      Print the adaptive human table and exit; never opens the picker.

  resume --json | jq '.sessions[] | .agent, .title'
      Print JSON v1 and post-process it. --json implies --list.

  resume config example > ~/.config/resume/config.toml
      Write a commented starter configuration file.

  resume completions zsh > ~/.zfunc/_resume
      Install shell completions for zsh.

COMMON ERRORS
  E1001 INVALID_SINCE           --since value is not a duration, a
                                YYYY-MM-DD date, or `all`.
  E1002 CONFLICTING_DIRECTION   --up and --down were both supplied.
  E1003 INVALID_DISTANCE        --up/--down value is not a non-negative
                                integer or `all`.
  E1004 INVALID_CONFIG          The configuration file could not be read
                                or parsed.
  E3001 ROOT_UNAVAILABLE        An agent's on-disk store is missing or
                                unreadable.
  E3002 GIT_SCOPE_DISCOVERY_FAILED
                                Git scope could not be determined; the
                                scope fell back to the exact directory.
  E3003 WORKSPACE_UNAVAILABLE   The selected Session's workspace no
                                longer exists or is not resumable.

  Run `resume --man` for each code's trigger, fix, and worked example.

SEE ALSO
  resume --man            The full manual: options, enums, JSON schema,
                          exit codes, errors, caveats, compatibility.
  resume config example   A commented starter configuration file.
  resume completions      Shell completion scripts for bash, zsh, fish.\
"
)]
pub struct Cli {
    #[arg(help = "Directory whose Sessions to search (default: current dir)")]
    pub directory: Option<PathBuf>,

    #[arg(
        short = 'U',
        long,
        value_name = "N|all",
        conflicts_with = "down",
        allow_hyphen_values = true,
        help = "Include ancestor directories up to N edges, or all"
    )]
    pub up: Option<Distance>,

    #[arg(
        short = 'D',
        long,
        value_name = "N|all",
        conflicts_with = "up",
        allow_hyphen_values = true,
        help = "Include descendant directories down to N edges, or all"
    )]
    pub down: Option<Distance>,

    #[arg(
        short = 'a',
        long,
        action = clap::ArgAction::Append,
        help = "Only this agent; repeatable; replaces configured agents"
    )]
    pub agent: Vec<OsString>,

    #[arg(
        long,
        value_name = "duration|date|all",
        help = "Only Sessions active at or after this cutoff"
    )]
    pub since: Option<Since>,

    #[arg(long, help = "Print the plain table instead of opening the picker")]
    pub list: bool,
    #[arg(long, help = "Print JSON v1 to stdout; implies --list")]
    pub json: bool,
    #[arg(long, help = "Include redacted paths and error chains in diagnostics")]
    pub verbose: bool,
    #[arg(long, help = "Read this config file instead of the discovered one")]
    pub config: Option<PathBuf>,

    #[arg(
        long,
        conflicts_with = "no_confirm",
        help = "Ask for confirmation before every Resume"
    )]
    pub confirm_always: bool,
    #[arg(
        long,
        conflicts_with = "confirm_always",
        help = "Skip ordinary confirmation; risk prompts still apply"
    )]
    pub no_confirm: bool,

    #[arg(
        long,
        exclusive = true,
        help = "Print the full manual page and exit"
    )]
    pub man: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}
```

---

## 6. E-code table and `src/errors.rs`

### 6.1 The adjudicated design

Three decisions are settled and must not be relitigated during implementation.

1. **`const CATALOG: [ErrorSpec; 7]` in a new `src/errors.rs` is the single source of truth for every user-facing error string.** The stderr block, the man page `ERRORS` section, the `--help` `COMMON ERRORS` block, and the clap value-parser hints all read from it. No user-facing error string is written twice anywhere in the tree.

2. **`Diagnostic.category: &'static str` is NOT changed.** `src/session.rs:77-83` keeps its shape. Aggregated discovery diagnostics keep emitting `resume: {category}: {count}` from `src/app.rs:851-870` byte-identically. This is what keeps `src/app.rs:1037`, and the three `tests/step9_app.rs` category-token tests, green without edits.

3. **NO `code` field is added to `--json`.** Two independent reasons:
   - `docs/json-schema.md:60` sets `additionalProperties: false` on `$defs/error` (and also on the envelope and on `$defs/session`). Adding a `code` key would make every existing v1 document that a consumer validates against the published schema fail, or would force a `schemaVersion` bump for a purely cosmetic gain.
   - Only 7 of roughly 20 category tokens map to an E-code. The remaining 13 — `pi_skipped`, `pi_discovery_failed`, `claude_discovery_failed`, `claude_missing_workspace`, `claude_no_session_id`, `codex_io`, `codex_invalid_session`, `codex_sqlite_id_mismatch`, `codex_sqlite_workspace_mismatch`, `omp_discovery_failed`, `omp_skipped`, `unknown_agent`, `io_error` — have none. A `code` field that is null or absent on two-thirds of entries is worse than no field: it invites consumers to key on it and then break.

### 6.2 The two-channel model

`resume` has two structurally different error channels, and the catalog serves both without merging them.

**Channel A — fatal, one per run.** A fatal error ends the process. There is at most one per run, it has a specific cause, and the user can usually act on it. These are the six sites in `src/app.rs` (`:63`, `:70`, `:77`, `:266`, `:277`, `:294`) plus the two clap value-parser failures in `src/cli.rs`. Rendering:

```text
ERROR [E1001] INVALID_SINCE: expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'
  Trigger: --since was given a value that is not a relative duration, a YYYY-MM-DD date, or the literal `all`.
  Fix:     Use <N>m|h|d|w for a relative window, YYYY-MM-DD for an absolute UTC-midnight cutoff, or `all` to disable filtering.
  Example: resume --since 7d
```

Four lines: `ERROR [<code>] <SLUG>: <what>`, then `Trigger:`, `Fix:`, `Example:` each indented two spaces with the labels padded to a common column. `<what>` is the runtime-specific detail — the failing path, the offending value, the OS error — supplied by the call site. `Trigger`, `Fix`, and `Example` come verbatim from the `ErrorSpec`.

**Channel B — aggregated discovery diagnostics.** Discovery is best-effort and parallel; a single run can produce hundreds of individual failures across four agents. These are collapsed by category into `Diagnostic { category, count, .. }` and rendered on stderr as `resume: {category}: {count}`, unchanged. `--json` carries them as `errors[].category` and `errors[].count`, unchanged.

The bridge between the channels is `ErrorSpec::categories: &'static [&'static str]`. Each of the three `E3xxx` specs lists the category tokens that roll up under it, so a user who sees `resume: codex_root_unavailable: 1` can look up `E3001 ROOT_UNAVAILABLE` in the man page. **This back-mapping is published only in the man page `ERRORS` section — never injected into stderr or JSON at runtime.**

### 6.3 The E-code table

| Code | Slug | Title | Trigger | Fix | Example | Exit | Parser hint | Categories |
|---|---|---|---|---|---|---|---|---|
| `E1001` | `INVALID_SINCE` | Invalid `--since` value | `--since` was given a value that is not a relative duration, a YYYY-MM-DD date, or the literal `all`. | Use `<N>m\|h\|d\|w` for a relative window, `YYYY-MM-DD` for an absolute UTC-midnight cutoff, or `all` to disable filtering. | `resume --since 7d` | 2 | `expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'` | (none) |
| `E1002` | `CONFLICTING_DIRECTION` | `--up` and `--down` cannot be combined | Both `-U/--up` and `-D/--down` were supplied. Scope walks in exactly one direction. | Keep the one you want. Use `--up` for ancestors, `--down` for descendants, or neither to let the Git repository define the scope. | `resume --up all` | 2 | (none) | (none) |
| `E1003` | `INVALID_DISTANCE` | Invalid `--up`/`--down` distance | `--up` or `--down` was given a value that is not a non-negative integer or the literal `all`. | Pass a whole number of path edges, such as `2`, or `all` for an unbounded walk in that direction. | `resume --down 2` | 2 | `expected a non-negative integer or 'all'` | (none) |
| `E1004` | `INVALID_CONFIG` | Configuration file is unreadable or invalid | The selected configuration file could not be read, or its TOML could not be parsed into the expected schema. | Fix the reported line and column, or regenerate a known-good file with `resume config example`. Configuration files are never merged, so only one file is ever at fault. | `resume config example > ~/.config/resume/config.toml` | 2 | (none) | (none) |
| `E3001` | `ROOT_UNAVAILABLE` | An agent store is missing or unreadable | An agent's on-disk Session store does not exist, is not a directory, or cannot be read with the current permissions. | This is usually benign: the agent is simply not installed or has never run. Narrow the run with `-a/--agent` to silence it, or check the store's permissions. | `resume -a codex -a claude` | 1 | (none) | `pi_root_unavailable`, `claude_root_unavailable`, `codex_root_unavailable`, `omp_root_unavailable` |
| `E3002` | `GIT_SCOPE_DISCOVERY_FAILED` | Git scope could not be determined | The Git executable was unavailable, or the current directory is not inside a Git working tree, so the repository-plus-worktrees scope could not be computed. | The scope falls back to the exact directory automatically. Pass `-U/--up` or `-D/--down` to define the scope explicitly, or run from inside a Git working tree. | `resume --up 1` | 1 | (none) | `git_scope_discovery_failed` |
| `E3003` | `WORKSPACE_UNAVAILABLE` | Selected Session cannot be resumed | The Session you selected is not Supported, has no launch specification, or its recorded workspace no longer validates at resume time. | Pick a Session whose STATUS column reads READY or ACTIVE. `resume` never recreates a missing worktree; restore it yourself first. | `resume --list` | 2 | (none) | `claude_missing_workspace`, `codex_sqlite_workspace_mismatch` |

Notes on the table:

- `E1001` and `E1003`'s `parser_hint` values are **byte-identical to today's literals** in `src/cli.rs:49` and `src/cli.rs:21`. `src/cli.rs:243 invalid_distance_and_since_are_usage_errors` pins the `E1003` string verbatim, so it must not drift by a single character — including the ASCII apostrophes around `'all'`.
- `E3001`'s exit code is 1 because a total discovery failure reaches `discovery_exit` at `src/app.rs:871-877`, which returns `EXIT_ERROR`. A partial store failure produces a diagnostic and exit 0; the spec's `exit_code` describes the fatal case only.
- `E3003`'s exit code is 2, matching the existing `src/app.rs:266` and `src/app.rs:273` sites which both `return EXIT_USAGE`.

#### Reserved codes — the one decision the implementer must confirm

Two codes are **reserved but not implemented** in this PR:

| Reserved code | Slug | Site | Current behaviour |
|---|---|---|---|
| `E1005` | `UNKNOWN_AGENT` | `src/app.rs:110` — `return Err(format!("unknown agent {agent:?}"))`, surfaced at `src/app.rs:70` as `resume: unknown agent "bogus"`, exit 2 | Bare message, no code |
| `E3004` | `DIRECTORY_UNREADABLE` | `src/app.rs:138` (via `build_scope` -> `crate::scope::canonical_base`), surfaced at `src/app.rs:77`, exit 2 | Bare `io::Error` message, no code |

They are reserved rather than adopted because promoting them changes stderr text that two tests observe indirectly:

- `tests/step9_app.rs:154 no_sessions_is_success_and_invalid_agent_is_usage_error` asserts only `status.code() == Some(2)`, so the `E1005` text change is safe today — but the assertion is loose enough that a future tightening would break.
- `src/app.rs:1053 unknown_agent_is_usage_error` asserts only `effective_options(...).is_err()`, so it is likewise insensitive to the message.

**Decision the implementer must confirm before writing code:** either

- **(a)** ship `CATALOG` with 7 entries as specified here, leaving `E1005`/`E3004` as a comment block in `src/errors.rs` marking the codes reserved so nothing else claims them; or
- **(b)** ship `CATALOG` with 9 entries, promoting both sites in the same PR and accepting that `src/app.rs:70` and `src/app.rs:77` now emit the four-line block.

This document specifies **(a)** as the default because it keeps the diff auditable and no test currently depends on the richer text. If the reviewer prefers (b), the two extra specs are mechanical additions and the array length and the `catalog_has_seven_entries` unit test both change to 9.

### 6.4 `src/errors.rs`, complete source

```rust
//! Unified user-facing error catalog.
//!
//! [`CATALOG`] is the single source of truth for every user-facing error
//! string in `resume`. The four-line stderr block, the `--help` COMMON
//! ERRORS list, and the man page ERRORS section are all rendered from these
//! seven records; no user-facing error string is written twice in the tree.
//!
//! Two channels consume the catalog differently:
//!
//! * **Fatal, one per run.** [`ErrorSpec::report`] builds a [`Report`] whose
//!   [`Display`] is the four-line `ERROR [E1001] INVALID_SINCE: <what>` /
//!   `Trigger:` / `Fix:` / `Example:` block, and [`Report::emit`] writes it
//!   to stderr and returns the process exit code.
//! * **Aggregated discovery diagnostics.** These keep their existing
//!   `resume: {category}: {count}` rendering and their existing
//!   `errors[].category` JSON shape. [`ErrorSpec::categories`] provides the
//!   category-to-code back-mapping, which is published only in the man page
//!   and never injected into stderr or JSON at runtime.
//!
//! Reserved but not implemented: `E1005 UNKNOWN_AGENT` for `src/app.rs:110`
//! and `E3004 DIRECTORY_UNREADABLE` for `src/app.rs:138`. Both sites still
//! print a bare message today. The codes are reserved here so nothing else
//! claims them; promoting them is a deliberate follow-up decision because it
//! changes stderr text.

use std::fmt;

/// One user-facing error, fully specified.
///
/// Every field is `&'static str` so the whole catalog lives in `.rodata` and
/// no allocation happens on the error path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorSpec {
    /// Stable identifier, e.g. `"E1001"`. `E1xxx` is a usage/input error,
    /// `E3xxx` is an environment/discovery error.
    pub code: &'static str,
    /// SCREAMING_SNAKE_CASE mnemonic, e.g. `"INVALID_SINCE"`.
    pub slug: &'static str,
    /// One-line human title, sentence case, no trailing period.
    pub title: &'static str,
    /// What the user did, or what the environment did, to reach this error.
    pub trigger: &'static str,
    /// The concrete remedy. Always actionable, never "check your input".
    pub fix: &'static str,
    /// A single command line that demonstrates the fixed form.
    pub example: &'static str,
    /// Process exit code when this error is fatal.
    pub exit_code: i32,
    /// The exact string a clap value parser must return for this error.
    /// `None` when no parser produces this code.
    pub parser_hint: Option<&'static str>,
    /// Aggregated `Diagnostic::category` tokens that roll up under this
    /// code. Empty for usage errors, which are never aggregated.
    pub categories: &'static [&'static str],
}

/// Aggregated diagnostic category tokens.
///
/// These are the exact `&'static str` values stored in
/// `crate::session::Diagnostic::category` and serialized as
/// `errors[].category` in `--json`. They are named here so the catalog's
/// back-mapping and the integration call sites share one definition.
pub mod category {
    pub const PI_ROOT_UNAVAILABLE: &str = "pi_root_unavailable";
    pub const PI_DISCOVERY_FAILED: &str = "pi_discovery_failed";
    pub const PI_SKIPPED: &str = "pi_skipped";
    pub const CLAUDE_ROOT_UNAVAILABLE: &str = "claude_root_unavailable";
    pub const CLAUDE_DISCOVERY_FAILED: &str = "claude_discovery_failed";
    pub const CLAUDE_MISSING_WORKSPACE: &str = "claude_missing_workspace";
    pub const CLAUDE_NO_SESSION_ID: &str = "claude_no_session_id";
    pub const CLAUDE_SKIPPED: &str = "claude_skipped";
    pub const CODEX_ROOT_UNAVAILABLE: &str = "codex_root_unavailable";
    pub const CODEX_IO: &str = "codex_io";
    pub const CODEX_INVALID_SESSION: &str = "codex_invalid_session";
    pub const CODEX_SQLITE_ID_MISMATCH: &str = "codex_sqlite_id_mismatch";
    pub const CODEX_SQLITE_WORKSPACE_MISMATCH: &str = "codex_sqlite_workspace_mismatch";
    pub const OMP_ROOT_UNAVAILABLE: &str = "omp_root_unavailable";
    pub const OMP_DISCOVERY_FAILED: &str = "omp_discovery_failed";
    pub const OMP_SKIPPED: &str = "omp_skipped";
    pub const GIT_SCOPE_DISCOVERY_FAILED: &str = "git_scope_discovery_failed";
    pub const UNKNOWN_AGENT: &str = "unknown_agent";
    pub const IO_ERROR: &str = "io_error";
}

/// The catalog. Single source of truth; nothing else defines these strings.
pub const CATALOG: [ErrorSpec; 7] = [
    ErrorSpec {
        code: "E1001",
        slug: "INVALID_SINCE",
        title: "Invalid --since value",
        trigger: "--since was given a value that is not a relative duration, a YYYY-MM-DD date, or the literal `all`.",
        fix: "Use <N>m|h|d|w for a relative window, YYYY-MM-DD for an absolute UTC-midnight cutoff, or `all` to disable filtering.",
        example: "resume --since 7d",
        exit_code: 2,
        parser_hint: Some(
            "expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'",
        ),
        categories: &[],
    },
    ErrorSpec {
        code: "E1002",
        slug: "CONFLICTING_DIRECTION",
        title: "--up and --down cannot be combined",
        trigger: "Both -U/--up and -D/--down were supplied. Scope walks in exactly one direction.",
        fix: "Keep the one you want. Use --up for ancestors, --down for descendants, or neither to let the Git repository define the scope.",
        example: "resume --up all",
        exit_code: 2,
        parser_hint: None,
        categories: &[],
    },
    ErrorSpec {
        code: "E1003",
        slug: "INVALID_DISTANCE",
        title: "Invalid --up/--down distance",
        trigger: "--up or --down was given a value that is not a non-negative integer or the literal `all`.",
        fix: "Pass a whole number of path edges, such as 2, or `all` for an unbounded walk in that direction.",
        example: "resume --down 2",
        exit_code: 2,
        parser_hint: Some("expected a non-negative integer or 'all'"),
        categories: &[],
    },
    ErrorSpec {
        code: "E1004",
        slug: "INVALID_CONFIG",
        title: "Configuration file is unreadable or invalid",
        trigger: "The selected configuration file could not be read, or its TOML could not be parsed into the expected schema.",
        fix: "Fix the reported line and column, or regenerate a known-good file with `resume config example`. Configuration files are never merged, so only one file is ever at fault.",
        example: "resume config example > ~/.config/resume/config.toml",
        exit_code: 2,
        parser_hint: None,
        categories: &[],
    },
    ErrorSpec {
        code: "E3001",
        slug: "ROOT_UNAVAILABLE",
        title: "An agent store is missing or unreadable",
        trigger: "An agent's on-disk Session store does not exist, is not a directory, or cannot be read with the current permissions.",
        fix: "This is usually benign: the agent is simply not installed or has never run. Narrow the run with -a/--agent to silence it, or check the store's permissions.",
        example: "resume -a codex -a claude",
        exit_code: 1,
        parser_hint: None,
        categories: &[
            category::PI_ROOT_UNAVAILABLE,
            category::CLAUDE_ROOT_UNAVAILABLE,
            category::CODEX_ROOT_UNAVAILABLE,
            category::OMP_ROOT_UNAVAILABLE,
        ],
    },
    ErrorSpec {
        code: "E3002",
        slug: "GIT_SCOPE_DISCOVERY_FAILED",
        title: "Git scope could not be determined",
        trigger: "The Git executable was unavailable, or the current directory is not inside a Git working tree, so the repository-plus-worktrees scope could not be computed.",
        fix: "The scope falls back to the exact directory automatically. Pass -U/--up or -D/--down to define the scope explicitly, or run from inside a Git working tree.",
        example: "resume --up 1",
        exit_code: 1,
        parser_hint: None,
        categories: &[category::GIT_SCOPE_DISCOVERY_FAILED],
    },
    ErrorSpec {
        code: "E3003",
        slug: "WORKSPACE_UNAVAILABLE",
        title: "Selected Session cannot be resumed",
        trigger: "The Session you selected is not Supported, has no launch specification, or its recorded workspace no longer validates at resume time.",
        fix: "Pick a Session whose STATUS column reads READY or ACTIVE. `resume` never recreates a missing worktree; restore it yourself first.",
        example: "resume --list",
        exit_code: 2,
        parser_hint: None,
        categories: &[
            category::CLAUDE_MISSING_WORKSPACE,
            category::CODEX_SQLITE_WORKSPACE_MISMATCH,
        ],
    },
];

/// Invalid `--since` value.
pub const E1001: &ErrorSpec = &CATALOG[0];
/// `--up` and `--down` cannot be combined.
pub const E1002: &ErrorSpec = &CATALOG[1];
/// Invalid `--up`/`--down` distance.
pub const E1003: &ErrorSpec = &CATALOG[2];
/// Configuration file is unreadable or invalid.
pub const E1004: &ErrorSpec = &CATALOG[3];
/// An agent store is missing or unreadable.
pub const E3001: &ErrorSpec = &CATALOG[4];
/// Git scope could not be determined.
pub const E3002: &ErrorSpec = &CATALOG[5];
/// Selected Session cannot be resumed.
pub const E3003: &ErrorSpec = &CATALOG[6];

/// The whole catalog, in declaration order.
pub fn catalog() -> &'static [ErrorSpec] {
    &CATALOG
}

/// Look up a spec by its `E`-prefixed code, e.g. `"E1001"`.
pub fn by_code(code: &str) -> Option<&'static ErrorSpec> {
    CATALOG.iter().find(|spec| spec.code == code)
}

/// Look up a spec by its SCREAMING_SNAKE_CASE slug, e.g. `"INVALID_SINCE"`.
pub fn by_slug(slug: &str) -> Option<&'static ErrorSpec> {
    CATALOG.iter().find(|spec| spec.slug == slug)
}

/// Back-map an aggregated `Diagnostic::category` token to its spec.
///
/// Returns `None` for the majority of categories, which intentionally have
/// no code. Callers must treat `None` as normal, not as a bug.
pub fn for_category(category: &str) -> Option<&'static ErrorSpec> {
    CATALOG
        .iter()
        .find(|spec| spec.categories.contains(&category))
}

impl ErrorSpec {
    /// The exact string a clap value parser must return for this code.
    ///
    /// Falls back to [`ErrorSpec::title`] for specs with no parser, so the
    /// return type stays `&'static str` and call sites need no `unwrap`.
    pub fn parser_message(&'static self) -> &'static str {
        match self.parser_hint {
            Some(hint) => hint,
            None => self.title,
        }
    }

    /// Build a fatal [`Report`] whose first line ends with `what`.
    pub fn report(&'static self, what: impl Into<String>) -> Report {
        Report {
            spec: self,
            what: what.into(),
        }
    }

    /// Build a fatal [`Report`] from any `Display` cause, e.g. an
    /// `io::Error` or a `ConfigError`.
    pub fn report_with(&'static self, cause: impl fmt::Display) -> Report {
        Report {
            spec: self,
            what: cause.to_string(),
        }
    }
}

/// One fatal, one-per-run error ready to be written to stderr.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    spec: &'static ErrorSpec,
    what: String,
}

impl Report {
    /// The catalog entry behind this report.
    pub fn spec(&self) -> &'static ErrorSpec {
        self.spec
    }

    /// The process exit code this error implies.
    pub fn exit_code(&self) -> i32 {
        self.spec.exit_code
    }

    /// The runtime-specific detail rendered on the first line.
    pub fn what(&self) -> &str {
        &self.what
    }

    /// Write the four-line block to stderr and return the exit code, so a
    /// call site reads `return E1004.report_with(error).emit();`.
    pub fn emit(&self) -> i32 {
        eprint!("{self}");
        self.exit_code()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "ERROR [{}] {}: {}",
            self.spec.code, self.spec.slug, self.what
        )?;
        writeln!(f, "  Trigger: {}", self.spec.trigger)?;
        writeln!(f, "  Fix:     {}", self.spec.fix)?;
        writeln!(f, "  Example: {}", self.spec.example)
    }
}

impl fmt::Display for ErrorSpec {
    /// Renders the same four-line block with [`ErrorSpec::title`] standing in
    /// for the runtime detail. Used to generate the man page ERRORS section.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ERROR [{}] {}: {}", self.code, self.slug, self.title)?;
        writeln!(f, "  Trigger: {}", self.trigger)?;
        writeln!(f, "  Fix:     {}", self.fix)?;
        writeln!(f, "  Example: {}", self.example)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_has_seven_entries() {
        assert_eq!(catalog().len(), 7);
    }

    #[test]
    fn codes_and_slugs_are_unique() {
        let codes: HashSet<_> = CATALOG.iter().map(|s| s.code).collect();
        let slugs: HashSet<_> = CATALOG.iter().map(|s| s.slug).collect();
        assert_eq!(codes.len(), CATALOG.len(), "duplicate code in CATALOG");
        assert_eq!(slugs.len(), CATALOG.len(), "duplicate slug in CATALOG");
    }

    #[test]
    fn every_spec_is_fully_populated() {
        for spec in catalog() {
            assert!(spec.code.starts_with('E'), "{}: code must start with E", spec.code);
            assert!(!spec.slug.is_empty(), "{}: empty slug", spec.code);
            assert!(!spec.title.is_empty(), "{}: empty title", spec.code);
            assert!(!spec.trigger.is_empty(), "{}: empty trigger", spec.code);
            assert!(!spec.fix.is_empty(), "{}: empty fix", spec.code);
            assert!(
                spec.example.starts_with("resume "),
                "{}: example must be a resume command line",
                spec.code
            );
            assert!(
                matches!(spec.exit_code, 1 | 2),
                "{}: fatal exit code must be 1 or 2",
                spec.code
            );
        }
    }

    #[test]
    fn per_code_consts_point_at_the_catalog() {
        assert_eq!(E1001.code, "E1001");
        assert_eq!(E1002.code, "E1002");
        assert_eq!(E1003.code, "E1003");
        assert_eq!(E1004.code, "E1004");
        assert_eq!(E3001.code, "E3001");
        assert_eq!(E3002.code, "E3002");
        assert_eq!(E3003.code, "E3003");
    }

    #[test]
    fn lookup_by_code_and_slug_round_trips() {
        for spec in catalog() {
            assert_eq!(by_code(spec.code).map(|s| s.slug), Some(spec.slug));
            assert_eq!(by_slug(spec.slug).map(|s| s.code), Some(spec.code));
        }
        assert!(by_code("E9999").is_none());
        assert!(by_slug("NOT_A_SLUG").is_none());
    }

    /// `src/cli.rs` returns these strings from its value parsers, and
    /// `src/cli.rs::invalid_distance_and_since_are_usage_errors` pins the
    /// distance one verbatim. A single changed character breaks that test.
    #[test]
    fn parser_hints_are_byte_identical_to_the_shipped_strings() {
        assert_eq!(
            E1003.parser_message(),
            "expected a non-negative integer or 'all'"
        );
        assert_eq!(
            E1001.parser_message(),
            "expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'"
        );
    }

    #[test]
    fn parser_message_falls_back_to_title() {
        assert_eq!(E1002.parser_message(), E1002.title);
        assert!(E1002.parser_hint.is_none());
    }

    #[test]
    fn category_back_mapping_resolves_the_documented_tokens() {
        assert_eq!(
            for_category(category::CODEX_ROOT_UNAVAILABLE).map(|s| s.code),
            Some("E3001")
        );
        assert_eq!(
            for_category(category::GIT_SCOPE_DISCOVERY_FAILED).map(|s| s.code),
            Some("E3002")
        );
        assert_eq!(
            for_category(category::CLAUDE_MISSING_WORKSPACE).map(|s| s.code),
            Some("E3003")
        );
    }

    /// Most categories deliberately have no code; `None` is the normal
    /// answer and must never be treated as a defect.
    #[test]
    fn uncoded_categories_return_none() {
        for token in [
            category::PI_SKIPPED,
            category::PI_DISCOVERY_FAILED,
            category::CLAUDE_DISCOVERY_FAILED,
            category::CLAUDE_NO_SESSION_ID,
            category::CLAUDE_SKIPPED,
            category::CODEX_IO,
            category::CODEX_INVALID_SESSION,
            category::CODEX_SQLITE_ID_MISMATCH,
            category::OMP_DISCOVERY_FAILED,
            category::OMP_SKIPPED,
            category::UNKNOWN_AGENT,
            category::IO_ERROR,
        ] {
            assert!(for_category(token).is_none(), "{token} should have no code");
        }
    }

    #[test]
    fn no_category_token_is_claimed_by_two_codes() {
        let mut seen = HashSet::new();
        for spec in catalog() {
            for token in spec.categories {
                assert!(seen.insert(*token), "{token} claimed twice");
            }
        }
    }

    #[test]
    fn report_renders_the_four_line_block() {
        let rendered = E1001.report("bad value 'yesterday'").to_string();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            "ERROR [E1001] INVALID_SINCE: bad value 'yesterday'"
        );
        assert!(lines[1].starts_with("  Trigger: "));
        assert!(lines[2].starts_with("  Fix:     "));
        assert_eq!(lines[3], "  Example: resume --since 7d");
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn report_carries_its_spec_and_exit_code() {
        let report = E3003.report_with(std::io::Error::other("gone"));
        assert_eq!(report.spec().code, "E3003");
        assert_eq!(report.exit_code(), 2);
        assert_eq!(report.what(), "gone");
    }

    #[test]
    fn error_spec_display_substitutes_the_title() {
        let rendered = E3002.to_string();
        assert!(rendered.starts_with(
            "ERROR [E3002] GIT_SCOPE_DISCOVERY_FAILED: Git scope could not be determined\n"
        ));
        assert_eq!(rendered.lines().count(), 4);
    }
}
```

### 6.5 How the single table serves both outputs

| String kind | Defined at | Read by stderr | Read by `--json` | Read by `--man` |
|---|---|---|---|---|
| E-code (`E1001`) | `errors::CATALOG[i].code` | yes — first line of the fatal block | **no** — `additionalProperties: false` blocks it | yes — `ERRORS` section heading |
| Slug (`INVALID_SINCE`) | `errors::CATALOG[i].slug` | yes — first line of the fatal block | no | yes — `ERRORS` section heading |
| Title | `errors::CATALOG[i].title` | no — replaced by the runtime `what` | no | yes — `ERRORS` section heading |
| Trigger | `errors::CATALOG[i].trigger` | yes — line 2 | no | yes — `ERRORS` section body |
| Fix | `errors::CATALOG[i].fix` | yes — line 3 | no | yes — `ERRORS` section body |
| Example | `errors::CATALOG[i].example` | yes — line 4 | no | yes — `ERRORS` section body |
| Parser hint | `errors::CATALOG[i].parser_hint` | yes — via clap's own usage error, exit 2 | no | yes — quoted in `ERRORS` |
| Runtime `what` | the call site, at runtime | yes — tail of line 1 | no | n/a |
| Category token | `errors::category::*`, stored in `Diagnostic::category` | yes — `resume: {category}: {count}` | yes — `errors[].category` | yes — the code back-mapping list |
| Count | `Diagnostic::count`, at runtime | yes — `resume: {category}: {count}` | yes — `errors[].count` | n/a |
| Redacted path / chain | `Diagnostic::verbose_path` / `verbose_chain` | yes, `--verbose` only | **never** | n/a |

Read the table column-wise:

- **stderr** is the only channel that sees the full four-line block, and the only channel where the E-code appears at runtime.
- **`--json` is frozen.** It sees exactly two catalog-adjacent strings — the category token and the count — both of which it already saw before this change. The v1 document is byte-identical before and after.
- **the man page** is the only place the whole catalog is published, including the category-to-code back-mapping that neither runtime channel emits.

---

## 7. Layer 3 — `resume --man`

### 7.1 Interface decision

**Chosen: a flag.**

```rust
#[arg(
    long,
    exclusive = true,
    help = "Print the full manual page and exit"
)]
pub man: bool,
```

`exclusive = true` makes clap reject `--man` in the presence of any other argument, producing a usage error and exit 2. That single attribute is what keeps the blast radius near zero:

- `Cli::has_session_query_options()` needs **no change**. It enumerates the eleven query fields; `man` is not one of them and never can be, because clap rejects `--man --json` before `has_session_query_options` is ever called.
- `Cli::validate()` needs **no change**. Its two rules — list-mode versus confirmation options, and subcommands versus query options — cannot be reached with `man` set, again because `exclusive` fires first.
- The only structural cost is one added line, `man: false,`, in the `Cli` struct literal at `src/app.rs:1053 unknown_agent_is_usage_error`, which builds `Cli` field-by-field rather than with `..Default::default()`.
- The only wiring cost is one early branch in `src/main.rs`, placed after `cli.validate()` and before `match &cli.command`:

```rust
fn main() {
    let cli = Cli::parse();
    if let Err(error) = cli.validate() {
        error.exit();
    }
    if cli.man {
        print!("{}", resume::man::page());
        return;
    }
    match &cli.command {
        // ... unchanged ...
    }
}
```

Why the flag rather than a subcommand: **it matches the requested UX**, `resume --man`, and it gives parity with `--help` and `--version`, which are also flags that print and exit. A user who has just read `-h` and sees `--man` listed one row below `--help` in the same option column will reach for the same shape.

### 7.2 Alternative considered: `resume man` as a subcommand

Recorded and rejected. Its exact code would have been:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    Config(ConfigArgs),
    Completions { shell: Shell },
    #[command(
        about = "Print the full manual page",
        long_about = "\
Print the full manual page.

The manual covers every option in prose, the value enumerations, the
complete --json v1 schema, exit codes, every error code, the v0.1.0
caveats, and the compatibility rules. It is static text; pipe it to a
pager or to a file.

This subcommand does not scan Sessions, so it rejects every
Session-query option and the bare DIRECTORY positional.\
"
    )]
    Man,
}
```

plus, in `src/main.rs`:

```rust
Some(Command::Man) => print!("{}", resume::man::page()),
```

plus — and this is the cost — a change to `Cli::validate()` to add a third rejection arm so `resume --json man` is a usage error rather than a silently-ignored query option:

```rust
Some(Command::Man) if self.has_session_query_options() => Err(Cli::command().error(
    clap::error::ErrorKind::ArgumentConflict,
    "`man` does not scan Sessions and cannot be combined with Session-query options",
)),
```

Rejected for three reasons: it does not match the requested `resume --man` spelling; it needs a real change to `validate()` where the flag needs none; and it breaks parity with `--help`/`--version`, which are the two closest analogues of "print static text and exit".

**Known nuance to accept, not fix.** clap's `exclusive` governs arguments, not subcommands, so `resume --man config example` parses. The early-return branch in `main` runs before the subcommand match, so `--man` wins and the manual is printed. This is the intended precedence and needs no extra guard.

### 7.3 Implementation shape

Follow `src/cli.rs:214 config_example()` exactly. One function, one raw string literal, trailing newline, single `print!` call site.

```rust
//! The static `resume --man` manual page.

/// The full manual, printed verbatim by `resume --man`.
///
/// Static text by design: no roff, no `clap_mangen`, no dynamic generation
/// from the command tree. The manual is written for a human reader and says
/// things clap cannot infer, so generating it would lose more than it saves.
/// The trailing newline is part of the literal, matching
/// `crate::cli::config_example`; the single call site in `src/main.rs` uses
/// `print!`, not `println!`.
pub fn page() -> &'static str {
    r#"...the block in §7.4, verbatim, including its trailing newline..."#
}
```

No roff. No `clap_mangen`. No dependency added to `Cargo.toml`. No dynamic generation from the clap command tree.

### 7.4 The manual page, complete and final

```text
RESUME(1)                       User Commands                      RESUME(1)

NAME
    resume - find and resume coding-agent Sessions

SYNOPSIS
    resume [DIRECTORY] [OPTIONS]
    resume config example
    resume completions <bash|zsh|fish>
    resume --man
    resume --help
    resume --version

DESCRIPTION
    `resume` scans the local on-disk stores of the coding agents you use --
    Codex, Claude Code, Pi, and OMP -- collects the Sessions that belong to
    the directory you are standing in, and hands the one you pick back to
    its own agent using that agent's native resume invocation.

    Discovery is read-only. `resume` never copies, rewrites, indexes,
    uploads, repairs, migrates, merges, or deletes a Session. It never kills,
    terminates, steals, attaches to, or switches to a running agent process.

    Resume is an exec into the agent's own CLI. After you select a Session,
    `resume` restores the terminal, looks the Session up by opaque key,
    revalidates its launch evidence, changes to the recorded workspace, and
    replaces itself with the agent using discrete program, argv, and
    environment values -- never a shell command string. After a successful
    exec, the agent determines the eventual exit status.

    SCOPE. By default the scope is the Git repository containing the
    starting directory, including its linked worktrees. If Git scope cannot
    be determined, the scope falls back to the exact directory and a
    `git_scope_discovery_failed` diagnostic is emitted; see E3002. Passing
    -U/--up or -D/--down replaces Git-derived scope with an explicit walk of
    the directory tree in exactly one direction.

    MODES. Without --list and without --json, `resume` opens an interactive
    picker. With either flag it prints once and exits, which makes it safe in
    scripts and CI. --json implies --list; passing both is accepted and
    redundant.

OPTIONS
    [DIRECTORY]
        The directory whose Sessions to search. Defaults to the current
        working directory. The path is canonicalised before scope is
        computed, so symlinks resolve to their targets and a Session
        recorded under a different spelling of the same directory still
        matches. A nonexistent or unreadable path is a usage error, exit 2.

    -U, --up <N|all>
        Widen the scope upward, toward the filesystem root, by at most N
        path edges. `--up 0` is the starting directory alone. `--up 2`
        includes the parent and grandparent. `--up all` walks to the root
        unbounded. Supplying --up replaces Git-derived scope entirely.
        Conflicts with --down; see E1002. A value that is neither a
        non-negative integer nor `all` is E1003. Negative values reach the
        parser rather than being read as flags, so `--up -1` reports E1003
        instead of an unknown-argument error.

    -D, --down <N|all>
        Widen the scope downward, into subdirectories, by at most N path
        edges. Same grammar and same error codes as --up. Conflicts with
        --up. Descending is bounded by whatever the filesystem permits;
        directories that cannot be read are skipped and counted as
        diagnostics rather than aborting the run.

    -a, --agent <AGENT>
        Restrict discovery to one agent. Repeatable: `-a codex -a claude`
        scans exactly those two. Names are case-insensitive. Valid names are
        `codex`, `claude`, `pi`, and `omp`; anything else is a usage error,
        exit 2.

        If any --agent occurs, the command-line list COMPLETELY REPLACES the
        configured `agents` list rather than appending to it. This mirrors
        how --since replaces a configured `since`.

        A supported integration still scans its standard store even when the
        agent's own CLI is not installed, because discovery reads files
        rather than shelling out.

    --since <duration|date|all>
        Keep only Sessions whose activity is at or after a cutoff.

        A relative duration is <N> followed by `m` (minutes), `h` (hours),
        `d` (days), or `w` (weeks): `30m`, `12h`, `7d`, `2w`.

        An absolute cutoff is `YYYY-MM-DD`, interpreted as UTC midnight.
        This is UTC, not local time. No other ISO-8601 shape is accepted for
        --since, deliberately, so the input stays unambiguous.

        `all` clears filtering without changing scope, and is the default.

        The cutoff compares against each Session's best-available activity
        signal, falling back to the transcript file's own modification time
        when no other signal exists. When --since is active, Sessions whose
        activity time is unknown are excluded. A value that parses as none
        of the three forms is E1001.

        A command-line --since replaces a configured `since`.

    --list
        Print the adaptive human table and exit instead of opening the
        picker. The table is described under `LIST OUTPUT` below. It is not
        a stable machine format. It does not switch layout automatically
        when stdout is redirected; it only adapts to a real terminal's
        width. Combining --list with --confirm-always or --no-confirm is a
        usage error, because list mode never opens a confirmation prompt and
        silently ignoring the flag would be worse than refusing it.

    --json
        Print one compact JSON v1 document to stdout and exit. Implies
        --list, so `resume --list --json` is accepted and redundant.
        Discovery diagnostics still go to stderr, so stdout stays parseable.
        The document contains Session metadata and aggregate error counts
        only; it never contains a `messages` array or raw transcript
        content. See `SCHEMA` below. Combining --json with
        --confirm-always or --no-confirm is a usage error, for the same
        reason as --list.

    --verbose
        Include a redacted filesystem path and a redacted error chain on
        each diagnostic line written to stderr. Redaction removes URLs and
        Git remotes before printing. Verbose output affects stderr only; it
        never adds fields to the --json document. A configured `verbose =
        true` has the same effect, and the command-line flag ORs with it.

    --config <PATH>
        Read this configuration file instead of the discovered one.

        Discovery order without --config: $XDG_CONFIG_HOME/resume/config.toml
        when XDG_CONFIG_HOME is set, otherwise $HOME/.config/resume/
        config.toml. If neither exists, built-in defaults are used and no
        error is reported.

        Configuration files are NEVER merged. Exactly one file is selected,
        so exactly one file can ever be at fault. A read failure or a parse
        failure is E1004, exit 2; the parse error carries the offending
        file's line and column.

        Supported keys, all optional:

            agents           list of strings, default
                             ["codex", "claude", "pi", "omp"]
            since            string, same grammar as --since, default "all"
            confirm_always   boolean, default false
            preview          "hidden" | "visible", default "hidden"
            preview_position "auto" | "right" | "bottom", default "auto"
            verbose          boolean, default false

        Run `resume config example` for a ready-to-edit file with every key
        set to its default.

    --confirm-always
        Ask for confirmation before every Resume, including ordinary ready
        Sessions that would otherwise launch on Enter. Conflicts with
        --no-confirm. Rejected alongside --list and --json. A configured
        `confirm_always = true` has the same effect, and the command-line
        flag ORs with it.

    --no-confirm
        Suppress the ordinary always-confirm behaviour. It NEVER skips a
        risk confirmation: Active, Workspace changed, Conflicting metadata,
        and Broad workspace always prompt regardless of this flag.
        Conflicts with --confirm-always. Rejected alongside --list and
        --json.

    --man
        Print this manual to stdout and exit 0. Exclusive: combining --man
        with any other argument is a usage error, exit 2.

    -h, --help
        Print the short help with -h, or the full option reference with
        --help, and exit 0.

    -V, --version
        Print the version and exit 0.

SUBCOMMANDS
    config example
        Print a commented example configuration file to stdout. The output
        round-trips through the same deserializer the runtime uses, so a file
        produced this way always parses. This subcommand does not scan
        Sessions and rejects every Session-query option and the bare
        DIRECTORY positional, exit 2.

    completions <bash|zsh|fish>
        Print a shell completion script to stdout. The script is generated
        from the live command definition, so it always matches the flags
        this binary actually accepts. This subcommand does not scan Sessions
        and rejects every Session-query option and the bare DIRECTORY
        positional, exit 2.

ENUMS
    Distance -- the value of -U/--up and -D/--down

        N       A non-negative decimal integer: the maximum number of path
                edges to traverse in that direction. 0 means the starting
                directory only.
        all     Unbounded traversal in that direction.

        Anything else is E1003, with the message:
            expected a non-negative integer or 'all'

    Since -- the value of --since

        <N>m    N minutes before now.
        <N>h    N hours before now.
        <N>d    N days before now.
        <N>w    N weeks before now.
        YYYY-MM-DD
                UTC midnight on that date. Strictly ten characters with
                dashes at positions 5 and 8; no other ISO-8601 shape is
                accepted here.
        all     No time filtering. This is the default.

        Anything else is E1001, with the message:
            expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'

    Shell -- the value of `completions`

        bash    Bash completion script for bash-completion.
        zsh     Zsh completion script for the zsh compsys autoloader.
        fish    Fish completion script for fish shell.

    STATUS -- first column of --list, derived from support and activity

        READY        Supported, and no running process was observed.
        ACTIVE       Supported, and a running process was observed. ACTIVE
                     does not forbid Resume; you are shown the evidence and
                     asked.
        DISCOVER     Discover Only: the Session was found and can be listed,
                     but this build cannot resume it.
        UNSUPPORTED  The Session's shape is understood but not resumable.
        UNAVAILABLE  The Session was found but its store or workspace is not
                     currently usable. Selecting it is E3003.

    SUPPORT -- the --json `support` field, Rust debug form

        Supported
        DiscoverOnly
        Unsupported
        Unavailable

    ACTIVITY -- the --json `activity` field, Rust debug form

        Active { observed_at: SystemTime { .. } }
        Inactive { observed_at: SystemTime { .. } }
        Unknown

        Note that the debug form embeds the observed timestamp inside the
        string. Consumers must not assume this field is a lower-case enum.
        In --list the same information is rendered in the UPDATED column as
        seconds since the Unix epoch, or the literal `unknown`.

    RISK -- the --json `risk` field, Rust debug form

        Normal              No elevated risk; ordinary confirmation rules
                            apply.
        BroadWorkspace      The recorded workspace is unusually broad, e.g.
                            a home directory. Always confirms.
        WorkspaceChanged    The workspace on disk differs from the one
                            recorded in the Session. Always confirms.
        ConflictingMetadata Two sources disagree about the Session's
                            identity or workspace. Always confirms.

LIST OUTPUT
    `resume --list` prints one row per Session:

        STATUS    AGENT[PROFILE]     UPDATED    TITLE BRANCH WORKSPACE

    STATUS is left-aligned in 9 columns, AGENT[PROFILE] in 18, UPDATED in
    10. TITLE and WORKSPACE each receive half of the remaining terminal
    width, clamped to at least 16 and at most 60 columns. When no controlling
    terminal can be queried -- redirected stdout, a pipe, CI -- both fall
    back to a fixed 48 columns so scripted output stays stable.

    BRANCH is always the literal `-` in v0.1.0. The column exists so the
    layout does not shift when branch reporting lands.

    This table is NOT a stable machine format. Use --json for anything a
    program will read.

SCHEMA
    `resume --json` writes exactly one compact JSON document to stdout. The
    envelope is `{schemaVersion, sessions, errors}` and nothing else;
    `additionalProperties` is false on the envelope and on both member
    object types.

    Top level
        schemaVersion   integer, const 1. Required.
        sessions        array of session objects. Required. May be empty.
        errors          array of error objects. Required. May be empty.

    sessions[] -- all eight properties are required
        agent      string. The agent name, e.g. "codex".
        profile    string or null. The agent profile when one applies;
                   null otherwise.
        id         string. The opaque resumable identifier, converted from
                   its OS-native form to a display string.
        title      string or null. Either an explicit native title or a
                   bounded, truncated summary excerpt derived from the first
                   user message. Treat titles as potentially
                   conversation-derived text. Null when unavailable.
        workspace  string or null. The recorded workspace directory as a
                   display string. Null when unavailable.
        support    string. The Rust debug form of the support status; see
                   SUPPORT under ENUMS.
        activity   string. The Rust debug form of the activity status; see
                   ACTIVITY under ENUMS.
        risk       string. The Rust debug form of the risk status; see RISK
                   under ENUMS.

    errors[] -- both properties are required
        category   string. A redacted aggregate category token, e.g.
                   "codex_root_unavailable". No enum is declared, so new
                   tokens may appear without a schemaVersion change.
                   Consumers must tolerate unknown tokens. See ERRORS for
                   the tokens that map to an error code.
        count      integer, minimum 0. How many individual failures rolled
                   up into this category during the run.

    Serialization notes
        - profile, title, and workspace are JSON null when unavailable, not
          absent and not the empty string.
        - support, activity, and risk are Rust debug-form strings. An active
          value embeds its observed timestamp inside the string. Do not
          assume they are lower-case enums.
        - Paths and native identifiers are converted to display strings for
          JSON. The native launch boundary keeps OS-native path and argument
          values separately, so a path that is not valid UTF-8 still
          launches correctly even though its JSON rendering is lossy.
        - errors entries expose only a redacted category and an aggregate
          count. Verbose paths and error chains stay on stderr and never
          enter JSON, with or without --verbose.
        - The sessions array is deterministically sorted once
          non-interactive discovery completes. This says nothing about the
          exact visible order inside the asynchronously loaded interactive
          picker.
        - There is deliberately NO `code` field on errors[]. Only seven of
          roughly twenty categories map to an error code, and the published
          schema sets additionalProperties to false. The mapping is
          published in ERRORS below instead.

EXAMPLES
    Basic
        resume
            Pick a Session from the current Git repository and its linked
            worktrees.

        resume ~/src/api
            Pick a Session scoped to another directory without leaving the
            one you are in.

    Scope
        resume --up all
            Widen the scope to every ancestor directory, unbounded.

        resume --up 2
            Include the parent and grandparent directories.

        resume --down 2
            Include descendants at most two path edges away.

    Filtering
        resume -a codex
            Only Codex Sessions.

        resume -a codex -a claude --since 7d
            Only Codex and Claude Code Sessions active in the last week.

        resume --since 2026-01-01
            Only Sessions active at or after UTC midnight on 2026-01-01.

        resume --since all
            Ignore a configured `since` for this run only.

    Machine-readable
        resume --list
            Print the adaptive human table and exit.

        resume --json
            Print JSON v1 to stdout and exit.

        resume --json | jq '.sessions[] | select(.support == "Supported")'
            Keep only resumable Sessions.

        resume --json | jq -r '.errors[] | "\(.category) \(.count)"'
            Summarise the run's aggregate diagnostics.

        resume --json --verbose 2> diagnostics.log
            Keep stdout parseable while capturing verbose diagnostics.

    Configuration
        resume config example
            Print a commented example configuration file.

        resume config example > ~/.config/resume/config.toml
            Write a starter configuration file into place.

        resume --config ./ci-resume.toml --json
            Use a checked-in configuration instead of the discovered one.

    Completions
        resume completions bash > /etc/bash_completion.d/resume
        resume completions zsh  > ~/.zfunc/_resume
        resume completions fish > ~/.config/fish/completions/resume.fish

EXIT CODES
    0    Success. Includes: a Session was listed or printed; the picker was
         dismissed with Esc; there were no results at all; a risk
         confirmation was declined. Declining exits 0 and does not reopen
         the picker.

    1    Runtime failure. Includes: every integration failed, so nothing
         could be discovered; final validation failed at resume time; the
         process launch itself failed.

    2    Usage failure. Includes: an invalid command line; an invalid
         configuration file; an accepted candidate that turns out to be
         unavailable when the picker cannot keep it.

    130  Interrupted. Ctrl+C, including Ctrl+C during a confirmation
         prompt.

    After a successful exec, the resumed agent determines the eventual exit
    status and `resume` no longer exists as a process.

ERRORS
    Fatal errors -- at most one per run -- are printed to stderr as a
    four-line block:

        ERROR [CODE] SLUG: what happened
          Trigger: why it happened
          Fix:     what to do about it
          Example: a command line that works

    Aggregated discovery diagnostics are different: discovery is best-effort
    and parallel, so failures are collapsed by category and printed as

        resume: CATEGORY: COUNT

    with the same category and count appearing in --json as
    errors[].category and errors[].count. Most categories have no error
    code, by design. The ones that do are listed under each code below.

    E1001 INVALID_SINCE
        Invalid --since value.
        Trigger: --since was given a value that is not a relative duration,
                 a YYYY-MM-DD date, or the literal `all`.
        Fix:     Use <N>m|h|d|w for a relative window, YYYY-MM-DD for an
                 absolute UTC-midnight cutoff, or `all` to disable
                 filtering.
        Example: resume --since 7d
        Exit:    2
        Message: expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD
                 date, or 'all'

    E1002 CONFLICTING_DIRECTION
        --up and --down cannot be combined.
        Trigger: Both -U/--up and -D/--down were supplied. Scope walks in
                 exactly one direction.
        Fix:     Keep the one you want. Use --up for ancestors, --down for
                 descendants, or neither to let the Git repository define
                 the scope.
        Example: resume --up all
        Exit:    2

    E1003 INVALID_DISTANCE
        Invalid --up/--down distance.
        Trigger: --up or --down was given a value that is not a
                 non-negative integer or the literal `all`.
        Fix:     Pass a whole number of path edges, such as 2, or `all` for
                 an unbounded walk in that direction.
        Example: resume --down 2
        Exit:    2
        Message: expected a non-negative integer or 'all'

    E1004 INVALID_CONFIG
        Configuration file is unreadable or invalid.
        Trigger: The selected configuration file could not be read, or its
                 TOML could not be parsed into the expected schema.
        Fix:     Fix the reported line and column, or regenerate a
                 known-good file with `resume config example`.
                 Configuration files are never merged, so only one file is
                 ever at fault.
        Example: resume config example > ~/.config/resume/config.toml
        Exit:    2

    E3001 ROOT_UNAVAILABLE
        An agent store is missing or unreadable.
        Trigger: An agent's on-disk Session store does not exist, is not a
                 directory, or cannot be read with the current permissions.
        Fix:     This is usually benign: the agent is simply not installed
                 or has never run. Narrow the run with -a/--agent to
                 silence it, or check the store's permissions.
        Example: resume -a codex -a claude
        Exit:    1 when every integration failed, otherwise the run
                 continues and this appears only as a diagnostic.
        Categories: pi_root_unavailable, claude_root_unavailable,
                 codex_root_unavailable, omp_root_unavailable

    E3002 GIT_SCOPE_DISCOVERY_FAILED
        Git scope could not be determined.
        Trigger: The Git executable was unavailable, or the current
                 directory is not inside a Git working tree, so the
                 repository-plus-worktrees scope could not be computed.
        Fix:     The scope falls back to the exact directory automatically.
                 Pass -U/--up or -D/--down to define the scope explicitly,
                 or run from inside a Git working tree.
        Example: resume --up 1
        Exit:    1 when nothing could be discovered, otherwise the run
                 continues with the fallback scope.
        Categories: git_scope_discovery_failed

    E3003 WORKSPACE_UNAVAILABLE
        Selected Session cannot be resumed.
        Trigger: The Session you selected is not Supported, has no launch
                 specification, or its recorded workspace no longer
                 validates at resume time.
        Fix:     Pick a Session whose STATUS column reads READY or ACTIVE.
                 `resume` never recreates a missing worktree; restore it
                 yourself first.
        Example: resume --list
        Exit:    2
        Categories: claude_missing_workspace,
                 codex_sqlite_workspace_mismatch

    Categories with no error code
        These appear in stderr diagnostics and in --json errors[] but map to
        no code. They are informational aggregates:

            pi_discovery_failed, pi_skipped, claude_discovery_failed,
            claude_no_session_id, claude_skipped, codex_io,
            codex_invalid_session, codex_sqlite_id_mismatch,
            omp_discovery_failed, omp_skipped, unknown_agent, io_error

        Codex SQLite degradation adds sub-tokens: locked, corrupt,
        unreadable, schema_unreadable, unsupported_schema, query_failed.

        New tokens may appear without a schemaVersion change. Consumers
        must tolerate tokens they do not recognise.

CAVEATS
    Explicit non-goals. These are not missing features; they are decisions.

        - Windows support.
        - Machine-wide Session scanning or listing.
        - Project configuration.
        - Dynamic integration plugins or agent installation.
        - Session modification, repair, migration, deletion, import, merge,
          or cross-agent deduplication.
        - Automatic replacement of a missing worktree.
        - Continuous watching or refresh.
        - Persistent transcript or full-text index.
        - External pager or editor.
        - Custom Skim fork or direct Ratatui UI.
        - Automatic agent install, process termination, terminal takeover,
          or Resume fallback to "latest".
        - Cursor, OpenCode, Grok, and Gemini in v0.1.0.

    v0.1.0 specifics.

        - macOS and Linux only. There is no Windows build.
        - The BRANCH column in --list is always the literal `-`. The column
          is reserved, not populated.
        - Ctrl-R does not reload, refresh, or switch views live. It is bound
          to a no-op on purpose: a channel-fed reload can execute its
          default filesystem command, which would list the filesystem by
          accident. Preview instead always renders a dual-section
          normalized-plus-terminal-safe-raw fallback.
        - --list is not a stable machine format. Its column widths adapt to
          the terminal, and its content is chosen for a human reader. Use
          --json.
        - Candidates stream into the picker as discovery completes. A
          candidate that arrives late can change the rank order of results
          you are already looking at. This is inherent to streaming
          discovery and is not a defect.

COMPATIBILITY
    schemaVersion 1 is stable. Within schemaVersion 1:

        - No required property is removed.
        - No property changes type.
        - No property changes nullability.
        - New aggregate category tokens MAY appear in errors[].category
          without a version change, because that property declares no enum.
          Consumers must tolerate unknown tokens.
        - The envelope, sessions[], and errors[] all declare
          additionalProperties: false. Adding a property to any of them
          therefore requires a schemaVersion bump, which is why no `code`
          field exists on errors[].

        schemaVersion changes only when an incompatible representation is
        introduced. Consumers should ignore unknown future fields rather
        than failing on them.

    --json is the only stable machine interface. --list output, picker
    output, stderr diagnostic wording, and the four-line fatal error block
    are all human-facing and may change in any release.

    Error codes are stable identifiers. Once published, an E-code is never
    reused for a different meaning. A code may be retired, and its Trigger,
    Fix, and Example text may be reworded, but its identity does not move.

SEE ALSO
    resume --help           The full option reference.
    resume -h               A one-screen summary.
    resume config example   A commented starter configuration file.
    resume completions      Shell completion scripts for bash, zsh, fish.

    Project documentation in the repository:
        README.md               Installation and quick start.
        docs/product-design.md  The full product design.
        docs/json-schema.md     The published JSON Schema for --json v1.
        docs/completions.md     Completion installation notes.
```

---

## 8. Backward compatibility

### 8.1 Per-test verdict

| Test | File:line | Verdict |
|---|---|---|
| `codex_corrupt_rollout_is_diagnosed_while_valid_sibling_survives` | `tests/step9_app.rs:199` | **pass** — asserts a category token appears in both the JSON document and stderr. `render_diagnostic` and `JsonError` are untouched, so both channels are byte-identical. |
| `codex_cross_root_symlink_rejection_is_diagnosed` | `tests/step9_app.rs:228` | **pass** — same reasoning. |
| `codex_sqlite_precedence_diagnostic_reaches_json_and_stderr` | `tests/step9_app.rs:302` | **pass** — same reasoning. This is the strictest of the three because it asserts the token in *both* channels; both are unchanged. |
| `git_scope_failure_becomes_a_visible_diagnostic` | `src/app.rs:1037` | **pass** — pins `render_diagnostic(&warning, false) == "resume: git_scope_discovery_failed: 1"`. `render_diagnostic` at `src/app.rs:851-870` is explicitly not modified, and `E3002` is a man-page back-mapping only, never injected into the aggregated line. |
| `invalid_distance_and_since_are_usage_errors` | `src/cli.rs:243` | **pass** — pins `"expected a non-negative integer or 'all'"` verbatim. `E1003.parser_hint` carries that exact byte sequence and `errors::parser_hints_are_byte_identical_to_the_shipped_strings` guards it independently. |
| `unknown_agent_is_usage_error` | `src/app.rs:1053` | **needs-additive-update** — builds `Cli` field-by-field with no struct-update syntax, so the new `man` field must be added. One line: `man: false,` between `no_confirm: false,` and `command: None,`. No assertion changes. |
| `direction_conflict_is_usage_error` | `src/cli.rs:222` | **pass** — asserts only `exit_code() == 2`. `conflicts_with` is unchanged. |
| `confirmation_conflict_is_usage_error` | `src/cli.rs:228` | **pass** — asserts only `exit_code() == 2`. |
| `config_example_round_trips_through_config_schema` | `src/cli.rs:262` | **pass** — `config_example()` is not touched. |
| `since_accepts_duration_date_and_all` | `src/cli.rs:279` | **pass** — parser behaviour is unchanged; only the error string's *source* moves into the catalog. |
| `since_cutoff_all_never_filters` | `src/cli.rs:305` | **pass** — no relation to help text. |
| `since_cutoff_duration_subtracts_from_now` | `src/cli.rs:310` | **pass** — no relation to help text. |
| `repeated_agent_preserves_replacement_list` | `src/cli.rs:321` | **pass** — `ArgAction::Append` is unchanged. |
| `subcommands_parse` | `src/cli.rs:327` | **pass** — adding `about`/`long_about` to a `Subcommand` variant does not change its parsed shape. |
| `list_or_json_with_confirmation_options_is_usage_error` | `src/cli.rs:348` | **pass** — `validate()` is unchanged; `man` is not a query option and `exclusive` prevents the combination from ever reaching `validate()`. |
| `config_and_completions_reject_session_query_options` | `src/cli.rs:371` | **pass** — `has_session_query_options()` is unchanged. |
| `validate_accepts_ordinary_combinations` | `src/cli.rs:395` | **pass** — `validate()` is unchanged. |
| `json_discovers_all_four_and_stdout_is_only_schema` | `tests/step9_app.rs:121` | **pass** — the JSON envelope is unchanged. This is the strongest single guarantee that no `code` field leaked into `--json`. |
| `list_output_uses_status_agent_updated_title_branch_workspace_priority` | `tests/step9_app.rs:142` | **pass** — `picker_candidate`'s `format!` at `src/app.rs:654-662` is unchanged. |
| `no_sessions_is_success_and_invalid_agent_is_usage_error` | `tests/step9_app.rs:154` | **pass** — asserts only exit codes. Would still pass under reserved-code option (b). |
| `meaningless_option_combinations_are_usage_errors` | `tests/step9_app.rs:172` | **pass** — asserts only exit codes, through the compiled binary, exercising `main`'s `validate()` wiring. The new `if cli.man` branch sits after `validate()`, so it cannot change any of these outcomes. |
| `malformed_since_value_is_usage_error` | `tests/step9_app.rs:349` | **pass** — asserts only exit code 2. |
| `since_all_matches_since_flag_absent` | `tests/step9_app.rs:360` | **pass** — no relation. |
| `since_duration_filters_out_stale_transcripts_across_all_four_agents` | `tests/step9_app.rs:375` | **pass** — no relation. |
| `omp_import_badge_is_visible_without_origin_secrets` | `tests/step9_app.rs:263` | **pass** — no relation. |
| `tests/parser_properties.rs` (whole file) | — | **pass** — property tests operate on integration parsers, not on CLI text. |
| `tests/picker_spike.rs` (whole file) | — | **pass** — picker behaviour is unchanged. |

### 8.2 Required test edits

Exactly one edit is **required**:

- `src/app.rs:1053`, inside `unknown_agent_is_usage_error`, add `man: false,` to the `Cli` struct literal, between `no_confirm: false,` and `command: None,`.

Everything else is **optional and additive**. Recommended new cases, none of which modify an existing assertion:

```rust
/// `--man` prints the manual and exits 0.
#[test]
fn man_flag_prints_the_manual() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    let output = run(tmp.path(), &ws, &["--man"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("RESUME(1)"));
    for heading in [
        "NAME", "SYNOPSIS", "DESCRIPTION", "OPTIONS", "ENUMS", "SCHEMA",
        "EXAMPLES", "EXIT CODES", "ERRORS", "CAVEATS", "COMPATIBILITY",
        "SEE ALSO",
    ] {
        assert!(text.contains(heading), "man page missing {heading}");
    }
    assert!(text.ends_with('\n'));
}

/// `--man` is exclusive: any companion argument is a usage error.
#[test]
fn man_flag_is_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir(&ws).unwrap();
    for args in [
        vec!["--man", "--json"],
        vec!["--man", "--list"],
        vec!["--man", "-a", "codex"],
        vec!["--man", "--up", "1"],
    ] {
        assert_eq!(run(tmp.path(), &ws, &args).status.code(), Some(2), "{args:?}");
    }
}

/// Every code named in the man page's ERRORS section exists in the catalog.
#[test]
fn man_page_errors_section_matches_the_catalog() {
    let page = resume::man::page();
    for spec in resume::errors::catalog() {
        assert!(page.contains(spec.code), "man page missing {}", spec.code);
        assert!(page.contains(spec.slug), "man page missing {}", spec.slug);
    }
}
```

The third case is the one that keeps §6 and §7 from drifting apart over time; add it.

---

## 9. Doc corrections

Two stale lines in `docs/product-design.md`, both proven wrong by the code, fixed in this same PR.

### 9.1 `docs/product-design.md:424` — `--since` date is UTC, not local

Proof: `src/cli.rs:46` routes a `YYYY-MM-DD` value through `parse_since_date`, which calls `crate::time::parse_iso8601`. `src/time.rs:75-81` computes `epoch_days * 86_400` and adds it to `UNIX_EPOCH` with no timezone term anywhere. `src/time.rs:41-44` states outright that "all inputs are treated as UTC wall-clock". `README.md:128` already says UTC and is correct; `docs/product-design.md:424` is the only wrong statement in the tree.

Before:

```markdown
- local date `YYYY-MM-DD`, beginning at local midnight;
```

After:

```markdown
- absolute date `YYYY-MM-DD`, beginning at UTC midnight;
```

### 9.2 `docs/product-design.md:448` — `--list` column order

Proof: `src/app.rs:654-662` formats the row as

```rust
format!(
    "{:<9} {:<18} {:<10} {} {} {}",
    status, agent, updated,
    text::truncate_to_width(title, column_width),
    branch,
    text::truncate_to_width(&workspace, column_width)
)
```

so TITLE precedes BRANCH, not the other way round. `agent` is `agent_label(session)`, which renders `agent[profile]` when a profile is present. `branch` is the hardcoded `"-"` at `src/app.rs:652`. `tests/step9_app.rs:142 list_output_uses_status_agent_updated_title_branch_workspace_priority` already asserts the code's order, so the test and the doc currently disagree.

Before:

```text
STATUS AGENT UPDATED BRANCH TITLE WORKSPACE
```

After:

```text
STATUS AGENT[PROFILE] UPDATED TITLE BRANCH WORKSPACE
```

Neither correction touches any `.rs` file.

---

## 10. Implementation order

Each step is independently committable and has its own gate. Do not proceed past a red gate.

**Step 1 — `src/errors.rs` plus `pub mod errors;`.**
Add the new module verbatim from §6.4 and insert `pub mod errors;` into `src/lib.rs` between `pub mod diagnostics;` and `pub mod injection;`. Change no other file. Nothing imports `errors` yet, so this is purely additive.
Gate: `cargo test` with **zero test edits**. If anything outside `errors::tests` runs differently, the module is not additive and something else was touched.
Also run: `cargo clippy --all-targets -- -D warnings`.

**Step 2 — route the two parser strings through the catalog.**
In `src/cli.rs`, change `Distance::from_str`'s `.map_err(|_| "expected a non-negative integer or 'all'".into())` to `.map_err(|_| crate::errors::E1003.parser_message().to_string())`, and `Since::from_str`'s final `Err(...)` to `Err(crate::errors::E1001.parser_message().to_string())`.
Gate: `cargo test cli::tests::invalid_distance_and_since_are_usage_errors` passes **without editing the test**. This is the byte-identity proof.

**Step 3 — Layer 1 and Layer 2 help text.**
Paste §5e over the `Cli`, `Command`, `ConfigArgs`, `ConfigCommand`, and `Shell` definitions, **excluding** the `man` field for now so this step stays free of struct-literal churn.
Gate: `cargo run -- -h | wc -l` reports <= 34 (12 without `--man`'s row, so expect 32). `cargo run -- --help` shows a non-empty description on every option row. `cargo test` green with no test edits.
Also verify: `cargo run -- completions zsh | grep -E "^'(config|completions):"` now shows non-empty descriptions, fixing the `completions/_resume:152-153` defect. Regenerate the checked-in completion files if the repository's release process expects them to be current.

**Step 4 — `src/man.rs` plus `pub mod man;`.**
Add the new module with §7.4's text as the single raw string literal, and insert `pub mod man;` into `src/lib.rs` between `pub mod launch;` and `pub mod message;`. No caller yet.
Gate: `cargo build` succeeds. `cargo test` green with no test edits. Sanity-check the raw string delimiter: §7.4 contains `"` and `#` characters, so use `r#"..."#` and confirm no `"#` sequence appears inside the body — if one does, escalate the delimiter to `r##"..."##`.

**Step 5 — the `--man` flag and its wiring.**
Add the `man` field from §5e to `Cli`, add the early branch to `src/main.rs`, and add `man: false,` to the `Cli` literal at `src/app.rs:1053`.
Gate: `cargo test` green — this is the one step where a test edit is expected, and it is the only one. `cargo run -- --man | head -1` prints `RESUME(1)                       User Commands                      RESUME(1)`. `cargo run -- --man --json; echo $?` prints `2`.

**Step 6 — the six fatal sites in `src/app.rs`.**
Rewrite `:63` to `E1004`, `:70` to a bare message (option (a)) or `E1005` (option (b)), `:77` to a bare message (option (a)) or `E3004` (option (b)), `:266` and `:273` to `E3003`, `:277` and `:294` to the appropriate runtime code. Under option (a), only `:63`, `:266`, `:273` change in this step.
Gate: `cargo test` green with no further test edits. `cargo run -- --config /nonexistent/x.toml; echo $?` prints the four-line `ERROR [E1004] INVALID_CONFIG:` block and exits 2.

**Step 7 — the optional `src/integration/**` category-const rename.**
Replace each `&'static str` category literal with the matching `crate::errors::category::*` const.
Gate: `cargo test` green with no test edits. If it is not green on the first run, **revert this step entirely** and keep the literals; the rename has no user-visible benefit and is not worth debugging.

**Step 8 — the two `docs/product-design.md` corrections from §9.**
Gate: `git diff --stat` shows exactly one file and exactly two changed lines.

**Step 9 — the optional additive tests from §8.2.**
Gate: `cargo test` green; the new `man_page_errors_section_matches_the_catalog` case passes.

### Risk to flag for follow-up

`docs/json-schema.md:60` sets `additionalProperties: false` on `$defs/error`, and the envelope and `$defs/session` do the same. That is why this plan adds no `code` field to `--json`, and the reasoning holds today. But it also means **any** future additive JSON field — a `code`, a `branch`, an `agentVersion` — forces a `schemaVersion` bump, which is a heavy price for a purely additive change that well-behaved consumers would ignore anyway. The document already tells consumers to "ignore unknown future fields", which directly contradicts the strictness the schema declares.

Recommend, as **separate follow-up work outside this PR**: relax `additionalProperties` from `false` to `true` on the envelope, on `$defs/session`, and on `$defs/error`, while keeping every `required` list as-is. That preserves all current validation strength for the fields that exist, matches the stated consumer contract, and unblocks additive evolution without a version bump. Do not bundle it here — it is a published-contract change and deserves its own review.
