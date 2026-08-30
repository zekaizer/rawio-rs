use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use rawio::app;
use rawio::cli::Cli;
use rawio_core::trace::Trace;

use rawio_core::platform;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let trace = Trace::new();
    let mut stdout = std::io::stdout().lock();
    let result = platform::backend()
        .and_then(|backend| app::run(&cli, backend.as_ref(), &mut stdout, &trace));
    let _ = stdout.flush();

    // On failure the trace is unconditional - a single run has to be enough to
    // locate the failing step without reproducing it.
    match result {
        Ok(()) => {
            if cli.trace && !trace.is_empty() {
                eprint!("{}", trace.render());
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            if !trace.is_empty() {
                eprintln!("device access trace:");
                eprint!("{}", trace.render());
            }
            ExitCode::from(err.exit_code())
        }
    }
}
