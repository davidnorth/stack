//! Command-line entry point for `focus-stack`.

use std::process::ExitCode;

use clap::Parser;
use focus_stack::cli::Cli;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = u8::try_from(error.exit_code()).unwrap_or(1);
            let _ = error.print();
            return ExitCode::from(code);
        }
    };

    match focus_stack::pipeline::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("focus-stack: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
