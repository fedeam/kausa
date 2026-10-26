//! kausa: a tiny, read-only Kubernetes controller that explains failing workloads with an LLM.
//!
//! Pre-alpha. `kausa run` watches the cluster and logs incidents (first implementation weekend);
//! the request to the model and the Event on the failing object come with the next weekends.

mod detect;
mod fingerprint;
mod incident;
mod log;
mod owner;
mod record;
mod run;

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use k8s_openapi::jiff::SignedDuration;
use serde_json::json;

const VERSION_LINE: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"), "\n");

/// The default settle window: a failure has to persist this long to be an incident.
const DEFAULT_SETTLE_SECONDS: u32 = 60;

const HELP: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    "\n",
    env!("CARGO_PKG_DESCRIPTION"),
    ".\n",
    "\n",
    "Status: pre-alpha. `kausa run` detects and groups failures and logs them as\n",
    "incidents; nothing is sent to a model and no Event is written yet.\n",
    "\n",
    "Usage:\n",
    "  kausa run [--settle-seconds <N>]   Run the controller\n",
    "  kausa -h, --help                   Print this help\n",
    "  kausa -V, --version                Print the version\n",
    "\n",
    "  kausa run uses the in-cluster service account when it runs in a Pod, and the\n",
    "  current context of the kubeconfig otherwise. It watches Pods and Warning\n",
    "  events, groups the failures of one workload into one incident once they\n",
    "  persist for the settle window (default 60 seconds), and writes one JSON\n",
    "  object per line to stdout. It stops on SIGTERM or Ctrl-C.\n",
    "\n",
    "Planned for v0.1 (NOT IMPLEMENTED YET; names and flags may change):\n",
    "  kausa run [--config <FILE>] [--dry-run]\n",
    "\n",
    "    Redact the context of each incident and send it to one OpenAI-compatible\n",
    "    endpoint, within a call and token budget, and write the explanation as a\n",
    "    Kubernetes Event on the failing object. --dry-run builds and logs the\n",
    "    redacted request and sends nothing.\n",
    "\n",
    "By design, kausa never reads Secrets or ConfigMaps, writes nothing but Events\n",
    "and sends nothing outside the cluster that has not been redacted first.\n",
    "\n",
    "Exit status: 0 on success or a clean stop, 1 when it cannot start or a watch\n",
    "ends (watch errors are logged and retried), 2 on a usage error or when stdout\n",
    "cannot be written.\n",
);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    run_cli(&args)
}

fn run_cli(args: &[OsString]) -> ExitCode {
    let Some((first, rest)) = args.split_first() else {
        return usage_error("no command or option given");
    };
    let output = match first.to_str() {
        Some("-h" | "--help") => HELP,
        Some("-V" | "--version") => VERSION_LINE,
        Some("run") => return run_command(rest),
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

fn run_command(args: &[OsString]) -> ExitCode {
    let mut settle_seconds = DEFAULT_SETTLE_SECONDS;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--settle-seconds") => {
                let Some(value) = args.next() else {
                    return usage_error("--settle-seconds needs a value");
                };
                match value.to_str().and_then(|v| v.parse::<u32>().ok()) {
                    Some(n) if n <= 3600 => settle_seconds = n,
                    _ => {
                        return usage_error(&format!(
                            "--settle-seconds takes a number of seconds from 0 to 3600, not '{}'",
                            value.to_string_lossy()
                        ));
                    }
                }
            }
            _ => {
                return usage_error(&format!("unexpected argument '{}'", arg.to_string_lossy()));
            }
        }
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = log::error(
                "cannot start the runtime",
                json!({"error": error.to_string()}),
            );
            return ExitCode::FAILURE;
        }
    };
    let options = run::Options {
        settle: SignedDuration::from_secs(i64::from(settle_seconds)),
    };
    match runtime.block_on(run::run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(run::RunError::Failure(error)) => {
            let _ = log::error("stopped on an error", json!({"error": error}));
            ExitCode::FAILURE
        }
        // The log itself cannot be written: say so on stderr, the only place left.
        Err(run::RunError::Output(error)) => {
            let _ = writeln!(
                io::stderr().lock(),
                "error: cannot write to stdout: {error}"
            );
            ExitCode::from(2)
        }
    }
}

fn usage_error(message: &str) -> ExitCode {
    // If stderr itself cannot be written there is nowhere left to report it; the exit code
    // still tells the caller.
    let _ = write!(
        io::stderr().lock(),
        "error: {message}\n\nRun 'kausa --help' for usage.\n"
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
        // The same code as a usage error: 1 is a failure while running (see the exit codes in
        // docs/explanation/architecture.md).
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "error: cannot write to stdout: {error}"
            );
            ExitCode::from(2)
        }
    }
}
