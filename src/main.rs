//! Command-line entry point for `focus-stack`.

use std::process::ExitCode;

use clap::Parser;
use focus_stack::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = u8::try_from(error.exit_code()).unwrap_or(1);
            let _ = error.print();
            return ExitCode::from(code);
        }
    };

    let result =
        match &cli.command {
            Some(Command::Retouch(args)) => focus_stack::retouch::run(args)
                .map_err(|error| (error.to_string(), error.exit_code())),
            Some(Command::Eval(args)) => focus_stack::quality::metrics::run(args)
                .map_err(|error| (error.to_string(), error.exit_code())),
            None => focus_stack::pipeline::run(&cli)
                .map_err(|error| (error.to_string(), error.exit_code())),
        };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((message, code)) => {
            eprintln!("focus-stack: {message}");
            ExitCode::from(code)
        }
    }
}
