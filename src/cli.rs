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
                .map_err(|_| "expected a non-negative integer or 'all'".into())
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
        Err("expected a duration (e.g. 7d, 2h, 30m, 1w), a YYYY-MM-DD date, or 'all'".into())
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
    Bash,
    Zsh,
    Fish,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    Config(ConfigArgs),
    Completions { shell: Shell },
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ConfigCommand {
    Example,
}

#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(
    name = "resume",
    version,
    about = "Find and resume coding-agent Sessions"
)]
pub struct Cli {
    pub directory: Option<PathBuf>,

    #[arg(
        short = 'U',
        long,
        value_name = "N|all",
        conflicts_with = "down",
        allow_hyphen_values = true
    )]
    pub up: Option<Distance>,

    #[arg(
        short = 'D',
        long,
        value_name = "N|all",
        conflicts_with = "up",
        allow_hyphen_values = true
    )]
    pub down: Option<Distance>,

    #[arg(short = 'a', long, action = clap::ArgAction::Append)]
    pub agent: Vec<OsString>,

    #[arg(long, value_name = "duration|date|all")]
    pub since: Option<Since>,

    #[arg(long)]
    pub list: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub verbose: bool,
    #[arg(long)]
    pub config: Option<PathBuf>,

    #[arg(long, conflicts_with = "no_confirm")]
    pub confirm_always: bool,
    #[arg(long, conflicts_with = "confirm_always")]
    pub no_confirm: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

pub fn command() -> clap::Command {
    Cli::command()
}

pub fn config_example() -> &'static str {
    r#"agents = ["codex", "claude", "pi", "omp"]
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
    fn direction_conflict_is_usage_error() {
        let error = Cli::try_parse_from(["resume", "--up", "1", "--down", "2"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
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
        assert_eq!(
            config.agents,
            Some(vec![
                "codex".into(),
                "claude".into(),
                "pi".into(),
                "omp".into()
            ])
        );
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
}
