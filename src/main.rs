//! kausa: a tiny, read-only Kubernetes controller that explains failing workloads with an LLM.
//!
//! Pre-alpha skeleton. Only `--help` and `--version` work; the controller itself is planned for
//! v0.1 and not implemented yet.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

const VERSION_LINE: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"), "\n");

const HELP: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    "\n",
    env!("CARGO_PKG_DESCRIPTION"),
    ".\n",
    "\n",
    "Status: pre-alpha. Only --help and --version work; nothing is watched or explained yet.\n",
    "\n",
    "Usage:\n",
    "  kausa -h, --help       Print this help\n",
    "  kausa -V, --version    Print the version\n",
    "\n",
    "Planned for v0.1 (NOT IMPLEMENTED YET; names and flags may change):\n",
    "  kausa run [--config <FILE>] [--dry-run]\n",
    "\n",
    "    Run in the cluster as a controller. Watch Warning events and pod status,\n",
    "    group the failures of one workload into one incident, redact the context\n",
    "    and send it to one OpenAI-compatible endpoint, within a call and token\n",
    "    budget. Write the explanation as a Kubernetes Event on the failing object.\n",
    "    --dry-run builds and logs the redacted request and sends nothing.\n",
    "\n",
    "By design, kausa never reads Secrets or ConfigMaps, writes nothing but Events\n",
    "and sends nothing outside the cluster that has not been redacted first.\n",
    "\n",
    "Exit status: 0 on success, 2 on a usage or output error.\n",
);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    run(&args)
}

fn run(args: &[OsString]) -> ExitCode {
    let Some((first, rest)) = args.split_first() else {
        return usage_error("no command or option given");
    };
    let output = match first.to_str() {
        Some("-h" | "--help") => HELP,
        Some("-V" | "--version") => VERSION_LINE,
        _ => {
            return usage_error(&format!(
                "unknown command or option '{}'",
                first.to_string_lossy()
            ));
        }
    };
    if let Some(extra) = rest.first() {
        return usage_error(&format!(
            "unexpected argument '{}'",
            extra.to_string_lossy()
        ));
    }
    write_stdout(output)
}

fn usage_error(message: &str) -> ExitCode {
    // If stderr itself cannot be written there is nowhere left to report it; the exit code
    // still tells the caller.
    let _ = write!(
        io::stderr().lock(),
        "error: {message}\n\n\
         Run 'kausa --help' for usage. No controller command is implemented yet.\n"
    );
    ExitCode::from(2)
}

fn write_stdout(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (`kausa --help | head -n 1`): not an error.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        // The same code as a usage error: the planned controller keeps 1 for a failure while
        // running (see the exit codes in docs/explanation/architecture.md).
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "error: cannot write to stdout: {error}"
            );
            ExitCode::from(2)
        }
    }
}
