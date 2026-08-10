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
//!   its `Display` implementation is the four-line `ERROR [E1001] INVALID_SINCE: <what>` /
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
    pub const PROC_PROBE_FAILED: &str = "proc_probe_failed";
    pub const PROC_PROBE_TIMEOUT: &str = "proc_probe_timeout";
    pub const OMP_BREADCRUMB_START_TIME_UNAVAILABLE: &str = "omp_breadcrumb_start_time_unavailable";
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
        fix: "Pick a resumable Session; `resume` never recreates a missing worktree. Restore it yourself first.",
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
            assert!(
                spec.code.starts_with('E'),
                "{}: code must start with E",
                spec.code
            );
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
        for informational in [
            category::PROC_PROBE_FAILED,
            category::PROC_PROBE_TIMEOUT,
            category::OMP_BREADCRUMB_START_TIME_UNAVAILABLE,
        ] {
            assert!(for_category(informational).is_none());
        }
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
