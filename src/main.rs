use std::io;

use clap::Parser;
use clap_complete::{generate, shells};
use resume::cli::{Cli, Command, ConfigCommand, Shell};

fn main() {
    let cli = Cli::parse();
    if cli.direction_conflict() {
        std::process::exit(
            resume::errors::E1002
                .report("both -U/--up and -D/--down were supplied")
                .emit(),
        );
    }
    if let Err(error) = cli.validate() {
        error.exit();
    }
    if cli.man {
        print!("{}", resume::man::page());
        return;
    }
    match &cli.command {
        Some(Command::Config(config)) => match config.command {
            ConfigCommand::Example => print!("{}", resume::cli::config_example()),
        },
        Some(Command::Setup) => match resume::settings::run_setup() {
            Ok(_) => {}
            Err(error) => {
                eprintln!("resume: {error}");
                std::process::exit(2);
            }
        },
        Some(Command::Completions { shell }) => {
            let mut command = resume::cli::command();
            let name = command.get_name().to_owned();
            match shell {
                Shell::Bash => generate(shells::Bash, &mut command, name, &mut io::stdout()),
                Shell::Zsh => generate(shells::Zsh, &mut command, name, &mut io::stdout()),
                Shell::Fish => generate(shells::Fish, &mut command, name, &mut io::stdout()),
            }
        }
        None => std::process::exit(resume::app::run(cli)),
    }
}
