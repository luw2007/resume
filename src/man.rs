//! The static `resume --man` manual page.

/// The full manual, printed verbatim by `resume --man`.
pub fn page() -> &'static str {
    r#"RESUME(1)                       User Commands                      RESUME(1)

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
    starting directory, limited to its current worktree. If Git scope cannot
    be determined, the scope falls back to the exact directory and a
    `git_scope_discovery_failed` diagnostic is emitted; see E3002. Passing
    -U/--up or -D/--down replaces Git-derived scope with an explicit walk of
    the directory tree in exactly one direction. Passing --all-worktrees
    widens Git-derived scope to every linked worktree of the repository
    instead of only the current one.

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

    --all-worktrees
        Widen Git-derived default scope to every linked worktree of the
        current repository, not only the current one. Has no effect outside
        a Git repository or when -U/--up or -D/--down replaces the default
        scope; combining it with either is a usage error, exit 2. Off by
        default: resolving every linked worktree costs one additional `git
        worktree list` subprocess call, and most invocations only care about
        the current worktree's own Sessions.

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
        `--list` renders the separate Session update timestamp using a
        human-relative date; it never uses ACTIVITY as its update time.

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

        UPDATED    AGENT[PROFILE]     TITLE  BRANCH

    UPDATED is the latest native Session timestamp, falling back to the
    transcript file modification time. It renders minutes under one hour,
    hours under one day, days under seven days, local month/day later in the
    current year, and ISO date in another year. AGENT[PROFILE] receives 18
    columns; TITLE receives the remaining terminal budget, clamped to at
    least 16 and at most 60 columns. When no controlling terminal can be
    queried -- redirected stdout, a pipe, CI -- TITLE falls back to 48
    columns.

    The picker Preview shows a full local UPDATED timestamp and whether it
    came from an agent-native timestamp or file modification time.

    This table is NOT a stable machine format. Use --json for anything a
    program will read.

PICKER KEYS
    Enter       Resume the highlighted Session (subject to risk confirmation).
    Esc         Cancel, exit 0.
    Ctrl-C      Interrupt, exit 130.
    Ctrl-O      Toggle the Preview pane (hidden by default).
    Ctrl-R      No-op by design; see the v0.1.0 specifics below.
    Alt-P       Page to older Sessions in the current tab.
    Alt-N       Page to newer Sessions in the current tab.
    Alt-Left    Switch to the previous tab (wraps).
    Alt-Right   Switch to the next tab (wraps).

    The Picker opens once every configured agent has finished discovery
    (see PROGRESS below), with an `All` tab plus one tab per discovered
    agent. Each tab holds every Session for its scope, sorted oldest-first
    with the most recently updated Session last, split into pages of 50;
    the Picker opens on the newest page of the `All` tab. Paging or
    switching tabs relaunches a small, fresh view over the target
    tab/page; it never reorders or drops a Session already discovered.

PROGRESS
    Before the Picker opens, resume waits for every configured agent's
    discovery to finish and prints one line per agent to stderr as it
    completes, in actual completion order:

        resume: <agent> scanned (<elapsed>)

    This is diagnostic output, not a stable format.

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
            Pick a Session from the current Git repository, limited to the
            current worktree.

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

        resume --all-worktrees
            Widen the default Git scope to every linked worktree of the
            current repository, not only the current one.

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
        Fix:     Pick a resumable Session; `resume` never recreates a missing
                 worktree. Restore it yourself first.
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
            omp_discovery_failed, omp_skipped, proc_probe_failed,
            proc_probe_timeout, omp_breadcrumb_start_time_unavailable,
            unknown_agent, io_error

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
"#
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_section_matches_the_catalog() {
        let page = super::page();
        let normalized_page = page.split_whitespace().collect::<Vec<_>>().join(" ");
        for spec in crate::errors::catalog() {
            assert!(page.contains(spec.code), "man page missing {}", spec.code);
            assert!(page.contains(spec.slug), "man page missing {}", spec.slug);
            for (field, value) in [
                ("trigger", spec.trigger),
                ("fix", spec.fix),
                ("example", spec.example),
            ] {
                let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(
                    normalized_page.contains(&normalized),
                    "man page missing {field} for {}",
                    spec.code
                );
            }
            for category in spec.categories {
                assert!(
                    page.contains(category),
                    "man page missing category {category}"
                );
            }
        }
    }
}
