//! DivertLink binary entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    match divertlink::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            if matches!(err, divertlink::Error::Config(_)) {
                eprintln!();
                eprintln!("hint: run `divertlink init` to generate a working configuration,");
                eprintln!("      or `divertlink check -c <file>` to validate one.");
            }
            ExitCode::FAILURE
        }
    }
}
