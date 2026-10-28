//! kausa: a tiny Kubernetes controller that explains failing workloads with an LLM. It reads the
//! cluster and writes nothing but its own Events.
//!
//! Pre-alpha. `kausa run` watches the cluster and logs incidents and, with `--endpoint` or
//! `--dry-run`, sends or logs a redacted request for each; with `--endpoint`, the explanation is
//! also an Event on the failing object unless `--no-events`.

mod budget;
mod context;
mod detect;
mod events;
mod explain;
mod fingerprint;
mod incident;
mod log;
mod metrics;
mod model;
mod owner;
mod prompt;
mod record;
mod redact;
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
    "incidents; with --endpoint it explains each one with the model, within a call\n",
    "budget, and writes the explanation as an Event on the failing workload.\n",
    "\n",
    "Usage:\n",
    "  kausa run [OPTIONS]    Run the controller\n",
    "  kausa -h, --help       Print this help\n",
    "  kausa -V, --version    Print the version\n",
    "\n",
    "Options of kausa run:\n",
    "  --settle-seconds <N>   How long a failure must persist (0-3600, default 60)\n",
    "  --endpoint <URL>       OpenAI-compatible base URL, e.g. https://host/v1;\n",
    "                         each incident's redacted context is sent there\n",
    "  --model <NAME>         The model to ask (needed with --endpoint)\n",
    "  --api-key-file <PATH>  A file with the API key (or KAUSA_API_KEY)\n",
    "  --allow-http           Allow a plain http:// endpoint (local or in-cluster)\n",
    "  --dry-run              Log the redacted request instead of sending it\n",
    "  --no-logs              Do not read container logs\n",
    "  --no-json-schema       Do not ask for structured output (response_format)\n",
    "  --no-events            Do not write Events (log the explanations only)\n",
    "  --max-calls-per-hour <N>  Model calls allowed per hour (default 30)\n",
    "  --max-calls-per-day <N>   Model calls allowed per day (default 200)\n",
    "  --max-input-tokens <N>    Cap on a request's estimated tokens (default 6000)\n",
    "  --dedup-hours <N>         Reuse an explanation for N hours (default 6)\n",
    "  --metrics-address <ADDR>  Serve /metrics, /healthz and /readyz there\n",
    "                            (for example 0.0.0.0:9090)\n",
    "\n",
    "  kausa run uses the in-cluster service account when it runs in a Pod, and the\n",
    "  current context of the kubeconfig otherwise. It watches Pods and Warning\n",
    "  events, groups the failures of one workload into one incident once they\n",
    "  persist for the settle window, and writes one JSON object per line to\n",
    "  stdout. It stops on SIGTERM or Ctrl-C.\n",
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
    let mut endpoint: Option<String> = None;
    let mut model: Option<String> = None;
    let mut api_key_file: Option<OsString> = None;
    let (mut allow_http, mut dry_run, mut no_logs, mut no_json_schema) =
        (false, false, false, false);
    let mut no_events = false;
    let mut metrics_address: Option<String> = None;
    let mut limits = Limits::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy();
        let mut value = || -> Result<String, ExitCode> {
            match args.next().map(|v| v.to_str()) {
                Some(Some(v)) if !v.is_empty() => Ok(v.to_owned()),
                _ => Err(usage_error(&format!("{flag} needs a value"))),
            }
        };
        match flag.as_ref() {
            "--settle-seconds" => match value() {
                Ok(v) => match v.parse::<u32>() {
                    Ok(n) if n <= 3600 => settle_seconds = n,
                    _ => {
                        return usage_error(&format!(
                            "--settle-seconds takes a number of seconds from 0 to 3600, not '{v}'"
                        ));
                    }
                },
                Err(code) => return code,
            },
            "--endpoint" => match value() {
                Ok(v) => endpoint = Some(v),
                Err(code) => return code,
            },
            "--model" => match value() {
                Ok(v) => model = Some(v),
                Err(code) => return code,
            },
            "--api-key-file" => match args.next() {
                Some(path) => api_key_file = Some(path.clone()),
                None => return usage_error("--api-key-file needs a value"),
            },
            "--max-calls-per-hour"
            | "--max-calls-per-day"
            | "--max-input-tokens"
            | "--dedup-hours" => {
                let name = flag.to_string();
                let parsed = match value() {
                    Ok(v) => v.parse::<usize>().ok().filter(|&n| n <= 1_000_000),
                    Err(code) => return code,
                };
                let Some(n) = parsed else {
                    return usage_error(&format!("{name} takes a whole number from 0 to 1000000"));
                };
                match name.as_str() {
                    "--max-calls-per-hour" => limits.per_hour = n,
                    "--max-calls-per-day" => limits.per_day = n,
                    "--max-input-tokens" => limits.input_tokens = n,
                    _ => limits.dedup_hours = n,
                }
            }
            "--metrics-address" => match value() {
                Ok(v) if v.parse::<std::net::SocketAddr>().is_ok() => metrics_address = Some(v),
                Ok(v) => {
                    return usage_error(&format!(
                        "--metrics-address takes an address and port such as 127.0.0.1:9090, not '{v}'"
                    ));
                }
                Err(code) => return code,
            },
            "--no-events" => no_events = true,
            "--allow-http" => allow_http = true,
            "--dry-run" => dry_run = true,
            "--no-logs" => no_logs = true,
            "--no-json-schema" => no_json_schema = true,
            _ => return usage_error(&format!("unexpected argument '{flag}'")),
        }
    }
    let explain = match explain_settings(
        endpoint,
        model,
        api_key_file,
        allow_http,
        dry_run,
        no_logs,
        no_json_schema,
        no_events,
        &limits,
    ) {
        Ok(settings) => settings,
        Err(message) => return usage_error(&message),
    };
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
        explain,
        metrics_address,
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

/// The explanation settings from the flags: `None` when neither `--endpoint` nor `--dry-run` is
/// given (kausa then only logs incidents). The API key comes from `--api-key-file` or the
/// environment variable `KAUSA_API_KEY`, never from an argument.
/// The cost limits from the flags.
struct Limits {
    per_hour: usize,
    per_day: usize,
    input_tokens: usize,
    dedup_hours: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_hour: 30,
            per_day: 200,
            input_tokens: 6000,
            dedup_hours: 6,
        }
    }
}

#[allow(clippy::too_many_arguments)] // one argument per flag, called once
fn explain_settings(
    endpoint: Option<String>,
    model: Option<String>,
    api_key_file: Option<OsString>,
    allow_http: bool,
    dry_run: bool,
    no_logs: bool,
    no_json_schema: bool,
    no_events: bool,
    limits: &Limits,
) -> Result<Option<explain::Settings>, String> {
    if endpoint.is_none() && !dry_run {
        if model.is_some() || api_key_file.is_some() || allow_http || no_json_schema {
            return Err(
                "--model, --api-key-file, --allow-http and --no-json-schema need \
                        --endpoint or --dry-run"
                    .into(),
            );
        }
        return Ok(None);
    }
    let api_key = match api_key_file {
        Some(path) => {
            let key = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "cannot read the API key file '{}': {e}",
                    path.to_string_lossy()
                )
            })?;
            Some(key.trim().to_owned()).filter(|k| !k.is_empty())
        }
        None => std::env::var("KAUSA_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .map(|k| k.trim().to_owned()),
    };
    let model_client = match (&endpoint, dry_run) {
        (Some(url), false) => Some(model::ModelClient::new(url, api_key, allow_http)?),
        _ => None,
    };
    let model_name = match (model, &model_client) {
        (Some(name), _) => name,
        (None, None) => "unset".to_owned(),
        (None, Some(_)) => return Err("--endpoint needs --model".into()),
    };
    Ok(Some(explain::Settings {
        model: model_client,
        model_name,
        logs: !no_logs,
        mode: if no_json_schema {
            prompt::OutputMode::Instructions
        } else {
            prompt::OutputMode::JsonSchema
        },
        max_tokens: 800,
        max_input_tokens: limits.input_tokens,
        events: !no_events && !dry_run,
        calls_per_hour: limits.per_hour,
        calls_per_day: limits.per_day,
        dedup_ttl: std::time::Duration::from_secs(limits.dedup_hours as u64 * 3600),
        instance: std::env::var("POD_NAME")
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "kausa".to_owned()),
    }))
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

// The cap of 32 namespaces lives in `MAX_NAMESPACES` (docs/adr/0013-namespaced-install-mode.md);
// the help line spells it out, so the two must change together.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_line_names_the_namespace_cap() {
        assert!(
            HELP.contains(&format!("repeat for more (up to {MAX_NAMESPACES})\n")),
            "{HELP}"
        );
    }
}
