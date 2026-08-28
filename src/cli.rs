use std::{ffi::OsString, path::PathBuf, str::FromStr, time::SystemTime};

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Distance {
    Finite(usize),
    All,
}

impl FromStr for Distance {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("all") {
            Ok(Self::All)
        } else {
            value
                .parse()
                .map(Self::Finite)
                .map_err(|_| crate::errors::E1003.parser_message().to_string())
        }
    }
}

/// A `--since <duration|date|all>` cutoff. `All` means no filtering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Since {
    All,
    /// A relative duration such as `7d`, resolved against a reference time.
    Duration(std::time::Duration),
    /// An absolute `YYYY-MM-DD` date, resolved as UTC midnight.
    Date(SystemTime),
}

impl FromStr for Since {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }
        if let Some(duration) = crate::time::parse_relative_duration(value) {
            return Ok(Self::Duration(duration));
        }
        if let Some(date) = parse_since_date(value) {
            return Ok(Self::Date(date));
        }
        Err(crate::errors::E1001.parser_message().to_string())
    }
}

impl Since {
    /// Resolve to an inclusive cutoff `SystemTime`: Sessions with activity at
    /// or after this instant pass the filter. `All` never filters, so it has
    /// no cutoff.
    pub fn cutoff(&self, now: SystemTime) -> Option<SystemTime> {
        match self {
            Self::All => None,
            Self::Duration(duration) => Some(now.checked_sub(*duration).unwrap_or(now)),
            Self::Date(cutoff) => Some(*cutoff),
        }
    }
}

/// Parse a strict `YYYY-MM-DD` date (rejecting any other ISO-8601 shape, to
/// keep `--since` inputs unambiguous) as UTC midnight.
fn parse_since_date(value: &str) -> Option<SystemTime> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    crate::time::parse_iso8601(value)
}

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

/// Agent names accepted by `-a/--agent`. `opencode` requires a binary built
/// with the optional `opencode` feature; without it, discovery reports the
/// same unavailable-root diagnostic as a missing OpenCode database.
pub const SUPPORTED_AGENTS: [&str; 5] = ["codex", "claude", "pi", "omp", "opencode"];

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    #[command(
        about = "Inspect resume configuration",
        long_about = "\
Inspect resume configuration.

`resume` reads exactly one configuration file and never merges several.
The file is $XDG_CONFIG_HOME/resume/config.toml when that file exists;
otherwise it falls back to $HOME/.config/resume/config.toml when that file
exists, unless --config named a different path.

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
    #[command(
        about = "Choose the agents Resume scans",
        long_about = "\
Choose the coding-agent integrations Resume scans and save the selection in
`~/.resume/settings.json`. This subcommand requires an interactive terminal,
does not scan Sessions, and replaces the previous selection.\
"
    )]
    Setup,
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

Writes a complete, valid TOML document with every supported key. The
`agents` value is a conservative starter selection; the remaining values are
their runtime defaults. Redirect it into place to start from a known good
file:

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
Codex, Claude Code, Pi, OMP, and OpenCode -- collects the Sessions that
belong to the directory you are standing in, and hands the one you pick
back to its own agent using that agent's native resume invocation.

Nothing is copied, rewritten, indexed, or uploaded. Discovery is
read-only. Resume is an exec into the agent's own CLI with the recorded
working directory, argv, and environment restored.

By default the scope is the Git repository containing the current
directory, limited to the current worktree. Use --all-worktrees to widen
it to every linked worktree, -U/--up and -D/--down to walk the directory
tree instead, and --since to hide stale Sessions.

Without --list or --json, `resume` opens an interactive picker. With
either flag it prints once and exits, so it is safe in scripts and CI.\
",
    after_help = "\
SYNTAX
  resume [DIRECTORY] [OPTIONS]
  resume setup
  resume config example
  resume completions <bash|zsh|fish>

EXAMPLES
  resume                     Pick a Session in the current project
  resume --up all            Search every ancestor directory too
  resume -a codex --since 7d Recent Codex Sessions only
  resume --json              Machine-readable JSON v1 on stdout
  resume --tree              Interactive Session relationship tree
  resume --tree --list       Static tree to stdout
  resume --tree --json       Graph JSON to stdout

`resume --help` has full descriptions; `resume --man` has the manual.\
",
    after_long_help = "\
COMMON EXAMPLES
  resume
      Pick a Session from the current Git repository, limited to the
      current worktree.

  resume ~/src/api
      Pick a Session scoped to another directory without leaving here.

  resume --all-worktrees
      Widen the scope to every linked worktree of the current repository.

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

  resume --tree
      Show Session relationships as an interactive tree.

  resume --tree --list
      Print the Session relationship tree to stdout and exit.

  resume --tree --json
      Print the Session relationship graph as JSON to stdout.

  resume setup
      Choose the agent integrations to scan and save the selection.

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
        allow_hyphen_values = true,
        help = "Include ancestor directories up to N edges, or all"
    )]
    pub up: Option<Distance>,

    #[arg(
        short = 'D',
        long,
        value_name = "N|all",
        allow_hyphen_values = true,
        help = "Include descendant directories down to N edges, or all"
    )]
    pub down: Option<Distance>,

    #[arg(
        long,
        conflicts_with_all = ["up", "down"],
        help = "Default Scope: include every linked Git worktree, not only the current one"
    )]
    pub all_worktrees: bool,

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

    #[arg(long, help = "Show recorded Session relationships as a tree")]
    pub tree: bool,
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

    #[arg(long, exclusive = true, help = "Print the full manual page and exit")]
    pub man: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}
impl Cli {
    /// Whether any Session-query option or bare positional `DIRECTORY` was
    /// supplied. `config example` and `completions` must reject these: they
    /// dispatch before config/Scope/discovery ever run, so a query option
    /// would be silently ignored rather than honored, which the design
    /// documents as an error rather than a silent no-op.
    fn has_session_query_options(&self) -> bool {
        self.directory.is_some()
            || self.up.is_some()
            || self.down.is_some()
            || self.all_worktrees
            || !self.agent.is_empty()
            || self.since.is_some()
            || self.tree
            || self.list
            || self.json
            || self.verbose
            || self.config.is_some()
            || self.confirm_always
            || self.no_confirm
    }

    /// Whether `-U/--up` and `-D/--down` were both supplied. Checked by the
    /// caller (not by a declarative clap `conflicts_with`) so the E1002
    /// four-line `Report` block documented in `--man` is the message the
    /// user actually sees, rather than clap's own terse conflict text.
    pub fn direction_conflict(&self) -> bool {
        self.up.is_some() && self.down.is_some()
    }

    /// Reject argument combinations that are individually valid but
    /// meaningless together, per `docs/product-design.md` Â§7: `--list`
    /// (with or without `--json`) never opens a confirmation prompt, so
    /// `--confirm-always`/`--no-confirm` alongside it is a silent no-op
    /// rather than an honored setting; `config example` and `completions`
    /// dispatch before Scope/discovery, so any Session-query option
    /// alongside them would likewise be silently ignored.
    pub fn validate(&self) -> Result<(), clap::Error> {
        if (self.list || self.json) && (self.confirm_always || self.no_confirm) {
            return Err(Cli::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--confirm-always/--no-confirm have no effect with --list/--json and cannot be combined with them",
            ));
        }
        match &self.command {
            Some(Command::Config(_)) if self.has_session_query_options() => Err(Cli::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "`config example` does not scan Sessions and cannot be combined with Session-query options",
            )),
            Some(Command::Completions { .. }) if self.has_session_query_options() => Err(Cli::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "`completions` does not scan Sessions and cannot be combined with Session-query options",
            )),
            Some(Command::Setup) if self.has_session_query_options() => Err(Cli::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "`setup` does not scan Sessions and cannot be combined with Session-query options",
            )),
            _ => Ok(()),
        }
    }
}

pub fn command() -> clap::Command {
    Cli::command()
}

pub fn config_example() -> &'static str {
    r#"agents = ["codex", "claude", "pi", "omp", "opencode"]
since = "all"
confirm_always = false
preview = "hidden"
preview_position = "auto"
verbose = false
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn help_describes_every_option() {
        let help = Cli::command().render_long_help().to_string();
        for description in [
            "Directory whose Sessions to search (default: current dir)",
            "Include ancestor directories up to N edges, or all",
            "Include descendant directories down to N edges, or all",
            "Only this agent; repeatable; replaces configured agents",
            "Only Sessions active at or after this cutoff",
            "Show recorded Session relationships as a tree",
            "Print the plain table instead of opening the picker",
            "Print JSON v1 to stdout; implies --list",
            "Include redacted paths and error chains in diagnostics",
            "Read this config file instead of the discovered one",
            "Ask for confirmation before every Resume",
            "Skip ordinary confirmation; risk prompts still apply",
            "Print the full manual page and exit",
        ] {
            assert!(help.contains(description), "help missing {description:?}");
        }
    }

    #[test]
    fn short_help_is_a_strict_subset_of_long_help() {
        // cli-help-three-layers: -h (render_help, `about`/`after_help`) must
        // stay strictly shorter than --help (render_long_help,
        // `long_about`/`after_long_help`) and must never leak long-only
        // content (the COMMON ERRORS catalog, or long_about's prose).
        let short = Cli::command().render_help().to_string();
        let long = Cli::command().render_long_help().to_string();
        assert!(
            short.len() < long.len(),
            "short help must be strictly shorter"
        );
        assert!(short.contains("Find and resume coding-agent Sessions"));
        assert!(short.contains("SYNTAX"), "short help keeps after_help");
        assert!(
            !short.contains("Nothing is copied, rewritten, indexed, or uploaded"),
            "short help must not leak long_about prose"
        );
        assert!(
            !short.contains("COMMON ERRORS"),
            "short help must not leak the after_long_help error catalog"
        );
    }

    #[test]
    fn common_errors_block_lists_exactly_the_catalog_codes_in_order() {
        // cli-help-three-layers: the --help COMMON ERRORS block is rendered
        // by hand in after_long_help (src/cli.rs:227-242), separately from
        // crate::errors::CATALOG; this asserts it never drifts out of sync
        // (missing, extra, reordered, or misspelled codes).
        let help = Cli::command().render_long_help().to_string();
        let start = help.find("COMMON ERRORS").expect("COMMON ERRORS heading");
        let end = help[start..]
            .find("SEE ALSO")
            .map(|offset| start + offset)
            .expect("SEE ALSO heading");
        let block = &help[start..end];

        let mut cursor = 0;
        for spec in crate::errors::catalog() {
            let marker = format!("{} {}", spec.code, spec.slug);
            let found = block[cursor..]
                .find(&marker)
                .unwrap_or_else(|| panic!("{marker:?} missing or out of order in: {block}"));
            cursor += found + marker.len();
        }

        let code_lines = block
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.len() >= 5
                    && trimmed.starts_with('E')
                    && trimmed[1..5].bytes().all(|b| b.is_ascii_digit())
            })
            .count();
        assert_eq!(
            code_lines,
            crate::errors::catalog().len(),
            "COMMON ERRORS must list exactly the catalog codes, no more"
        );
    }

    #[test]
    fn man_is_exclusive_with_query_options() {
        for argv in [
            vec!["resume", "--man", "--json"],
            vec!["resume", "--man", "--list"],
            vec!["resume", "--man", "-a", "codex"],
            vec!["resume", "--man", "--up", "1"],
        ] {
            assert_eq!(Cli::try_parse_from(argv).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn direction_conflict_is_no_longer_a_parse_error() {
        // --up/--down conflict now surfaces as the E1002 four-line block
        // (see `direction_conflict` and `main.rs`), not clap's own
        // declarative `conflicts_with` parse failure. Parsing itself must
        // succeed so `direction_conflict()` can observe both values.
        let cli = Cli::try_parse_from(["resume", "--up", "1", "--down", "2"]).unwrap();
        assert!(cli.direction_conflict());
    }

    #[test]
    fn confirmation_conflict_is_usage_error() {
        let error =
            Cli::try_parse_from(["resume", "--confirm-always", "--no-confirm"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn invalid_distance_and_since_are_usage_errors() {
        let negative_distance = Cli::try_parse_from(["resume", "--up", "-1"]).unwrap_err();
        assert_eq!(negative_distance.exit_code(), 2);
        assert!(
            negative_distance
                .to_string()
                .contains("expected a non-negative integer or 'all'"),
            "unexpected error: {negative_distance}"
        );

        for argv in [
            vec!["resume", "--since", "yesterday"],
            vec!["resume", "--since", "-7d"],
            vec!["resume", "--since", "2026-13-40"],
        ] {
            assert_eq!(Cli::try_parse_from(argv).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn config_example_round_trips_through_config_schema() {
        let config: crate::config::Config = toml::from_str(config_example()).unwrap();
        // cli-config-subcommand-example: the example is the template users copy
        // into config.toml, and `agents` there is an exhaustive selection --
        // omitting a supported agent silently turns it off for anyone who
        // starts from the generated file.
        let mut listed = config.agents.clone().unwrap();
        listed.sort();
        let mut supported: Vec<String> = SUPPORTED_AGENTS.iter().map(|a| a.to_string()).collect();
        supported.sort();
        assert_eq!(listed, supported);
        assert_eq!(config.since, Some(Since::All));
        assert_eq!(config.confirm_always, Some(false));
        assert_eq!(config.preview, Some(crate::config::PreviewMode::Hidden));
        assert_eq!(
            config.preview_position,
            Some(crate::config::PreviewPosition::Auto)
        );
        assert_eq!(config.verbose, Some(false));
    }

    #[test]
    fn since_accepts_duration_date_and_all() {
        assert_eq!(
            Cli::try_parse_from(["resume", "--since", "7d"])
                .unwrap()
                .since,
            Some(Since::Duration(std::time::Duration::from_secs(7 * 86_400)))
        );
        assert_eq!(
            Cli::try_parse_from(["resume", "--since", "all"])
                .unwrap()
                .since,
            Some(Since::All)
        );
        assert!(matches!(
            Cli::try_parse_from(["resume", "--since", "2026-01-01"])
                .unwrap()
                .since,
            Some(Since::Date(_))
        ));
    }

    #[test]
    fn since_cutoff_all_never_filters() {
        assert_eq!(Since::All.cutoff(SystemTime::now()), None);
    }

    #[test]
    fn since_cutoff_duration_subtracts_from_now() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let since = Since::Duration(std::time::Duration::from_secs(100));
        assert_eq!(
            since.cutoff(now),
            Some(now - std::time::Duration::from_secs(100))
        );
    }

    #[test]
    fn repeated_agent_preserves_replacement_list() {
        let cli = Cli::try_parse_from(["resume", "-a", "pi", "--agent", "codex"]).unwrap();
        assert_eq!(cli.agent, [OsString::from("pi"), OsString::from("codex")]);
    }

    #[test]
    fn subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["resume", "config", "example"])
                .unwrap()
                .command,
            Some(Command::Config(ConfigArgs {
                command: ConfigCommand::Example
            }))
        ));
        assert!(matches!(
            Cli::try_parse_from(["resume", "completions", "fish"])
                .unwrap()
                .command,
            Some(Command::Completions { shell: Shell::Fish })
        ));
    }

    #[test]
    fn setup_parses_and_rejects_session_query_options() {
        assert!(matches!(
            Cli::try_parse_from(["resume", "setup"]).unwrap().command,
            Some(Command::Setup)
        ));
        let cli = Cli::try_parse_from(["resume", "-a", "pi", "setup"]).unwrap();
        assert_eq!(cli.validate().unwrap_err().exit_code(), 2);
    }
    /// `docs/product-design.md` Â§7: "list mode rejects confirmation
    /// options". `--list`/`--json` never open a confirmation prompt, so
    /// `--confirm-always`/`--no-confirm` alongside either must be a usage
    /// error (exit 2) rather than a silently-ignored no-op.
    #[test]
    fn list_or_json_with_confirmation_options_is_usage_error() {
        for argv in [
            vec!["resume", "--list", "--confirm-always"],
            vec!["resume", "--list", "--no-confirm"],
            vec!["resume", "--json", "--confirm-always"],
            vec!["resume", "--json", "--no-confirm"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            let error = cli.validate().unwrap_err();
            assert_eq!(error.exit_code(), 2, "{argv:?}");
        }
    }

    /// `docs/product-design.md` Â§7: "`config example` and `completions`
    /// reject Session-query options" because both subcommands dispatch
    /// before config/Scope/discovery ever run, so a query option would be
    /// silently ignored rather than honored.
    #[test]
    fn config_and_completions_reject_session_query_options() {
        for argv in [
            vec!["resume", "--up", "1", "config", "example"],
            vec!["resume", "--down", "2", "config", "example"],
            vec!["resume", "-a", "codex", "config", "example"],
            vec!["resume", "--since", "7d", "config", "example"],
            vec!["resume", "--verbose", "config", "example"],
            vec!["resume", "/tmp", "config", "example"],
            vec!["resume", "-a", "codex", "completions", "bash"],
            vec!["resume", "--up", "1", "completions", "zsh"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            let error = cli.validate().unwrap_err();
            assert_eq!(error.exit_code(), 2, "{argv:?}");
        }
    }

    /// Plain `config example` / `completions` (no Session-query options) and
    /// ordinary `--list`/`--json` (no confirmation options) remain valid;
    /// `validate` must not reject sessions-query-free or confirmation-free
    /// combinations.
    #[test]
    fn validate_accepts_ordinary_combinations() {
        for argv in [
            vec!["resume", "config", "example"],
            vec!["resume", "completions", "bash"],
            vec!["resume", "--list"],
            vec!["resume", "--json"],
            vec!["resume", "--list", "--json"],
            vec!["resume", "--up", "1"],
            vec!["resume", "--confirm-always"],
            vec!["resume", "--tree"],
            vec!["resume", "--tree", "--list"],
            vec!["resume", "--tree", "--json"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            assert!(cli.validate().is_ok(), "{argv:?}");
        }
    }

    #[test]
    fn tree_parses_as_bool_flag() {
        let cli = Cli::try_parse_from(["resume", "--tree"]).unwrap();
        assert!(cli.tree);
        assert!(!cli.list);
        assert!(!cli.json);
    }

    #[test]
    fn tree_with_list_and_json_parse() {
        let cli = Cli::try_parse_from(["resume", "--tree", "--list"]).unwrap();
        assert!(cli.tree);
        assert!(cli.list);

        let cli = Cli::try_parse_from(["resume", "--tree", "--json"]).unwrap();
        assert!(cli.tree);
        assert!(cli.json);
    }

    #[test]
    fn tree_is_a_session_query_option() {
        // --tree alongside config/completions/setup must be rejected
        // because it is a Session-query option.
        for argv in [
            vec!["resume", "--tree", "config", "example"],
            vec!["resume", "--tree", "completions", "bash"],
            vec!["resume", "--tree", "setup"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            assert_eq!(cli.validate().unwrap_err().exit_code(), 2, "{argv:?}");
        }
    }

    #[test]
    fn tree_is_exclusive_with_man() {
        assert_eq!(
            Cli::try_parse_from(["resume", "--man", "--tree"])
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
