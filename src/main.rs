//! kausa: a tiny Kubernetes controller that explains failing workloads with an LLM. It reads the
//! cluster and writes nothing but its own Events and, with `--history`, its own Explanation
//! objects.
//!
//! Pre-alpha. `kausa run` watches the cluster and logs incidents and, with `--endpoint` or
//! `--dry-run`, sends or logs a redacted request for each; with `--endpoint`, the explanation is
//! also an Event on the failing object unless `--no-events`, and, with `--history`, kept in an
//! Explanation object in its namespace, and, with `--notify-url-file`, posted to a webhook.

mod budget;
mod context;
mod detect;
mod events;
mod explain;
mod fingerprint;
mod history;
mod incident;
mod log;
mod metrics;
mod model;
mod notification;
mod notify;
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

/// How many days `kausa run --history` keeps an explanation without `--history-days`
/// (docs/adr/0014-explanation-history.md).
const DEFAULT_HISTORY_DAYS: u32 = 7;

/// The most namespaces `kausa run --namespace` watches (docs/adr/0013-namespaced-install-mode.md).
/// The help line spells the number out, so the two must change together.
const MAX_NAMESPACES: usize = 32;

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
    "  --namespace <NAME>     Watch only this namespace; repeat for more (up to 32)\n",
    "  --no-rollouts          Do not watch Deployments for stalled rollouts (D014)\n",
    "  --endpoint <URL>       OpenAI-compatible base URL, e.g. https://host/v1;\n",
    "                         each incident's redacted context is sent there\n",
    "  --model <NAME>         The model to ask (needed with --endpoint)\n",
    "  --api-key-file <PATH>  A file with the API key (or KAUSA_API_KEY)\n",
    "  --allow-http           Allow a plain http:// endpoint (local or in-cluster)\n",
    "  --dry-run              Log the redacted request instead of sending it\n",
    "  --no-logs              Do not read container logs\n",
    "  --no-json-schema       Do not ask for structured output (response_format)\n",
    "  --no-events            Do not write Events (log the explanations only)\n",
    "  --history              Keep each incident's explanations in an Explanation\n",
    "                         object in its namespace (needs the CRD)\n",
    "  --history-days <N>     Delete history older than N days (1-30, default 7)\n",
    "  --notify-url-file <PATH>  Post each new explanation to the webhook whose URL is\n",
    "                            in this file (needs --endpoint)\n",
    "  --notify-format <FORMAT>  json (default), slack or telegram\n",
    "  --notify-telegram-chat-id <ID>  telegram's chat: an integer or @channel\n",
    "  --notify-token-file <PATH>  json only: a bearer token for the webhook\n",
    "  --notify-allow-http       Allow a plain http:// webhook (local or in-cluster)\n",
    "  --max-calls-per-hour <N>  Model calls allowed per hour (default 30)\n",
    "  --max-calls-per-day <N>   Model calls allowed per day (default 200)\n",
    "  --max-input-tokens <N>    Cap on a request's estimated tokens (default 6000)\n",
    "  --dedup-hours <N>         Reuse an explanation for N hours (default 6)\n",
    "  --metrics-address <ADDR>  Serve /metrics, /healthz and /readyz there\n",
    "                            (for example 0.0.0.0:9090)\n",
    "\n",
    "  kausa run uses the in-cluster service account when it runs in a Pod, and the\n",
    "  current context of the kubeconfig otherwise. It watches Pods, Warning events\n",
    "  and Deployments, groups the failures of one workload into one incident once\n",
    "  they persist for the settle window, and writes one JSON object per line to\n",
    "  stdout. It stops on SIGTERM or Ctrl-C.\n",
    "\n",
    "By design, kausa never reads Secrets or ConfigMaps, writes nothing but Events\n",
    "and, with --history, its own Explanation objects, and sends nothing unredacted\n",
    "to the model. With --notify-url-file, only the documented fields of each new\n",
    "explanation go to the webhook.\n",
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
    let mut no_rollouts = false;
    let mut history = false;
    let mut history_days: Option<u32> = None;
    let mut metrics_address: Option<String> = None;
    let mut namespaces: Vec<String> = Vec::new();
    let mut notify_flags = NotifyFlags::default();
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
            "--namespace" => match value() {
                Ok(v) => namespaces.push(v),
                Err(code) => return code,
            },
            "--metrics-address" => match value() {
                Ok(v) if v.parse::<std::net::SocketAddr>().is_ok() => metrics_address = Some(v),
                Ok(v) => {
                    return usage_error(&format!(
                        "--metrics-address takes an address and port such as 127.0.0.1:9090, not '{v}'"
                    ));
                }
                Err(code) => return code,
            },
            "--history-days" => match value() {
                Ok(v) => match v.parse::<u32>() {
                    Ok(n) if (1..=30).contains(&n) => history_days = Some(n),
                    _ => {
                        return usage_error(&format!(
                            "--history-days takes a whole number of days from 1 to 30, not '{v}'"
                        ));
                    }
                },
                Err(code) => return code,
            },
            "--notify-url-file" => match args.next() {
                Some(path) => notify_flags.url_file = Some(path.clone()),
                None => return usage_error("--notify-url-file needs a value"),
            },
            "--notify-token-file" => match args.next() {
                Some(path) => notify_flags.token_file = Some(path.clone()),
                None => return usage_error("--notify-token-file needs a value"),
            },
            "--notify-format" => match value() {
                Ok(v) => match NotifyFormat::parse(&v) {
                    Some(format) => notify_flags.format = Some(format),
                    None => {
                        return usage_error(&format!(
                            "--notify-format takes json, slack or telegram, not '{v}'"
                        ));
                    }
                },
                Err(code) => return code,
            },
            "--notify-telegram-chat-id" => match value() {
                Ok(v) => notify_flags.chat_id = Some(v),
                Err(code) => return code,
            },
            "--notify-allow-http" => notify_flags.allow_http = true,
            "--history" => history = true,
            "--no-events" => no_events = true,
            "--no-rollouts" => no_rollouts = true,
            "--allow-http" => allow_http = true,
            "--dry-run" => dry_run = true,
            "--no-logs" => no_logs = true,
            "--no-json-schema" => no_json_schema = true,
            _ => return usage_error(&format!("unexpected argument '{flag}'")),
        }
    }
    if let Err(message) = check_namespaces(&namespaces) {
        return usage_error(&message);
    }
    if let Err(message) = check_history(history, history_days, dry_run, endpoint.is_some()) {
        return usage_error(message);
    }
    let notifier = match check_notify(notify_flags, dry_run, endpoint.is_some()) {
        Ok(notifier) => notifier,
        Err(message) => return usage_error(&message),
    };
    let explain = match explain_settings(
        endpoint,
        model,
        api_key_file,
        allow_http,
        dry_run,
        no_logs,
        no_json_schema,
        no_events,
        history.then(|| history_days.unwrap_or(DEFAULT_HISTORY_DAYS)),
        &limits,
        notifier,
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
        namespaces,
        rollouts: !no_rollouts,
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

/// Checks the `--namespace` values in argument order: each must be a namespace name, the first
/// repeat is refused, and then the count against [`MAX_NAMESPACES`].
fn check_namespaces(namespaces: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for name in namespaces {
        if !is_dns_label(name) {
            return Err(format!(
                "--namespace takes a namespace name (lowercase letters, digits and '-', starting \
                 and ending with a letter or digit, at most 63 characters), not '{name}'"
            ));
        }
        if !seen.insert(name.as_str()) {
            return Err(format!("--namespace '{name}' is given twice"));
        }
    }
    if namespaces.len() > MAX_NAMESPACES {
        return Err(format!(
            "--namespace is given {} times; kausa watches at most {MAX_NAMESPACES} namespaces \
             (watch the whole cluster instead)",
            namespaces.len()
        ));
    }
    Ok(())
}

/// Checks `--history` and `--history-days` against the other flags, in this order: the days need
/// `--history`, which is not used with `--dry-run` and needs `--endpoint`.
fn check_history(
    history: bool,
    days: Option<u32>,
    dry_run: bool,
    endpoint: bool,
) -> Result<(), &'static str> {
    if days.is_some() && !history {
        return Err("--history-days needs --history");
    }
    if history && dry_run {
        return Err("--history is not used with --dry-run: no explanation is written");
    }
    if history && !endpoint {
        return Err("--history needs --endpoint");
    }
    Ok(())
}

/// The `--notify-*` flags as given (docs/adr/0015-notifications.md). The two files are paths
/// only: what they hold is read by [`check_notify`] and never echoed.
#[derive(Default)]
struct NotifyFlags {
    url_file: Option<OsString>,
    format: Option<NotifyFormat>,
    chat_id: Option<String>,
    token_file: Option<OsString>,
    allow_http: bool,
}

/// The value of `--notify-format`; the Telegram chat comes from its own flag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NotifyFormat {
    Json,
    Slack,
    Telegram,
}

impl NotifyFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "json" => Some(Self::Json),
            "slack" => Some(Self::Slack),
            "telegram" => Some(Self::Telegram),
            _ => None,
        }
    }
}

/// Checks the `--notify-*` flags and builds the webhook's client, in this order: every other
/// notify flag needs `--notify-url-file` (the first one named in the order format, chat id, token
/// file, allow-http), which is not used with `--dry-run` and needs `--endpoint`; the chat id is
/// for `telegram` only and the token file for `json` only; `telegram` needs a well-formed chat
/// id; then the URL file is read and its URL checked, and then the token file is read. `None`
/// when notifications are off. No message carries what either file holds.
fn check_notify(
    flags: NotifyFlags,
    dry_run: bool,
    endpoint: bool,
) -> Result<Option<notify::Notifier>, String> {
    let Some(url_file) = flags.url_file else {
        let given = [
            ("--notify-format", flags.format.is_some()),
            ("--notify-telegram-chat-id", flags.chat_id.is_some()),
            ("--notify-token-file", flags.token_file.is_some()),
            ("--notify-allow-http", flags.allow_http),
        ];
        return match given.into_iter().find(|&(_, given)| given) {
            Some((flag, _)) => Err(format!("{flag} needs --notify-url-file")),
            None => Ok(None),
        };
    };
    if dry_run {
        return Err(
            "--notify-url-file is not used with --dry-run: no explanation is written".into(),
        );
    }
    if !endpoint {
        return Err("--notify-url-file needs --endpoint".into());
    }
    let format = flags.format.unwrap_or(NotifyFormat::Json);
    if flags.chat_id.is_some() && format != NotifyFormat::Telegram {
        return Err("--notify-telegram-chat-id is used only with --notify-format telegram".into());
    }
    if flags.token_file.is_some() && format != NotifyFormat::Json {
        return Err("--notify-token-file is used only with --notify-format json".into());
    }
    let format = match format {
        NotifyFormat::Json => notify::Format::Json,
        NotifyFormat::Slack => notify::Format::Slack,
        NotifyFormat::Telegram => {
            let Some(id) = flags.chat_id else {
                return Err("--notify-format telegram needs --notify-telegram-chat-id".into());
            };
            match notification::parse_chat_id(&id) {
                Some(chat) => notify::Format::Telegram(chat),
                None => {
                    return Err(format!(
                        "--notify-telegram-chat-id takes an integer or @ and a channel's \
                         username, not '{id}'"
                    ));
                }
            }
        }
    };
    let url_text = read_secret_file("--notify-url-file", &url_file)?;
    let url = notify::check_url(&url_text, &format, flags.allow_http).map_err(|problem| {
        format!(
            "the URL in --notify-url-file '{}' {problem}",
            url_file.to_string_lossy()
        )
    })?;
    let token = match &flags.token_file {
        Some(path) => Some(read_secret_file("--notify-token-file", path)?),
        None => None,
    };
    notify::Notifier::new(url, format, token, flags.allow_http).map(Some)
}

/// The trimmed content of the file `path` given to `flag`. The error names the flag and the path,
/// never the content.
fn read_secret_file(flag: &str, path: &OsString) -> Result<String, String> {
    let shown = path.to_string_lossy();
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("{flag} '{shown}' cannot be read: {e}"))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(format!("{flag} '{shown}' is empty"));
    }
    Ok(trimmed.to_owned())
}

/// A DNS-1123 label, what Kubernetes takes as a namespace name: 1 to 63 characters out of
/// `[a-z0-9-]`, starting and ending with a letter or digit.
fn is_dns_label(name: &str) -> bool {
    let bytes = name.as_bytes();
    let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (1..=63).contains(&bytes.len())
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes.iter().all(|b| edge(b) || *b == b'-')
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
    history_days: Option<u32>,
    limits: &Limits,
    notify: Option<notify::Notifier>,
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
        history_days,
        calls_per_hour: limits.per_hour,
        calls_per_day: limits.per_day,
        dedup_ttl: std::time::Duration::from_secs(limits.dedup_hours as u64 * 3600),
        instance: std::env::var("POD_NAME")
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "kausa".to_owned()),
        notify,
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
