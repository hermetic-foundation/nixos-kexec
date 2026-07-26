use std::{io, process::ExitCode};

use clap::Parser;
use nixos_kexec::{run, Cli};

fn main() -> ExitCode {
    let mut stdout = io::stdout().lock();
    match run(Cli::parse(), &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
