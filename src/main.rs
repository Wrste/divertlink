//! DivertLink binary entry point.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    match divertlink::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Write errors to stderr without panicking if the pipe is closed.
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "error: {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                let _ = writeln!(stderr, "  caused by: {cause}");
                source = cause.source();
            }
            if matches!(err, divertlink::Error::Config(_)) {
                let _ = writeln!(stderr);
                let _ = writeln!(
                    stderr,
                    "hint: run `divertlink init` to generate a working configuration,"
                );
                let _ = writeln!(
                    stderr,
                    "      or `divertlink check -c <file>` to validate one."
                );
            }
            ExitCode::FAILURE
        }
    }
}
