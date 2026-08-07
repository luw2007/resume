use std::{ffi::OsString, path::PathBuf, str::FromStr};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Since {
    All,
    Duration(String),
    Date(String),
}

impl FromStr for Since {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }
        if valid_date(value) {
            return Ok(Self::Date(value.into()));
        }
        if valid_duration(value) {
            return Ok(Self::Duration(value.into()));
        }
        Err("expected duration (for example 7d), YYYY-MM-DD, or 'all'".into())
    }
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let year: u16 = value[..4].parse().unwrap();
    let month: usize = value[5..7].parse().unwrap();
    let day: u8 = value[8..].parse().unwrap();
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    month > 0 && month <= 12 && day > 0 && day <= days[month - 1]
}

fn valid_duration(value: &str) -> bool {
    let split = value.find(|c: char| !c.is_ascii_digit());
    matches!(split, Some(i) if i > 0 && i + 1 == value.len() && matches!(&value[i..], "m" | "h" | "d" | "w"))
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

    #[arg(short = 'U', long, value_name = "N|all", conflicts_with = "down")]
    pub up: Option<Distance>,

    #[arg(short = 'D', long, value_name = "N|all", conflicts_with = "up")]
    pub down: Option<Distance>,

    #[arg(short = 'a', long, action = clap::ArgAction::Append)]
    pub agent: Vec<OsString>,

    #[arg(long)]
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
since = "30d"
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
        for argv in [
            vec!["resume", "--up", "-1"],
            vec!["resume", "--since", "yesterday"],
        ] {
            assert_eq!(Cli::try_parse_from(argv).unwrap_err().exit_code(), 2);
        }
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
