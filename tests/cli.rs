//! Runs the built binary the way a user would and checks what it prints today.

use std::process::{Command, Output};

/// Runs the binary with no cluster to reach: no kubeconfig and no in-cluster environment, so a
/// test that starts `kausa run` by mistake fails at once instead of watching a real cluster.
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kausa"));
    command
        .env("KUBECONFIG", "/nonexistent/kausa-tests/kubeconfig")
        .env("HOME", "/nonexistent/kausa-tests")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("KUBERNETES_SERVICE_PORT")
        .env_remove("KAUSA_API_KEY");
    command
}

fn kausa(args: &[&str]) -> Output {
    command()
        .args(args)
        .output()
        .expect("the kausa binary should start")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8")
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8")
}

#[test]
fn version_prints_name_and_version() {
    for flag in ["--version", "-V"] {
        let output = kausa(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(
            stdout(&output),
            concat!("kausa ", env!("CARGO_PKG_VERSION"), "\n"),
            "{flag}"
        );
        assert_eq!(stderr(&output), "", "{flag}");
    }
}

#[test]
fn help_shows_usage_and_the_options_of_run() {
    for flag in ["--help", "-h"] {
        let output = kausa(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let help = stdout(&output);
        assert!(help.starts_with(concat!("kausa ", env!("CARGO_PKG_VERSION"), "\n")));
        assert!(help.contains("Usage:"), "{flag}: {help}");
        assert!(help.contains("--endpoint <URL>"), "{flag}: {help}");
        assert!(help.lines().all(|l| l.len() <= 84), "{flag}: {help}");
        assert_eq!(stderr(&output), "", "{flag}");
    }
}

#[test]
fn unknown_command_is_a_usage_error() {
    let output = kausa(&["explain"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("error: unknown command or option 'explain'"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn no_arguments_is_a_usage_error() {
    let output = kausa(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
}

// A failed write exits with 2, like a usage error. /dev/full, where every write fails with "No
// space left on device", exists on Linux, where CI runs the tests.
#[cfg(target_os = "linux")]
#[test]
fn output_error_exits_with_2() {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full should open for writing");
    let output = command()
        .arg("--help")
        .stdout(full)
        .output()
        .expect("the kausa binary should start");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).starts_with("error: cannot write to stdout: "),
        "{}",
        stderr(&output)
    );
}

#[test]
fn extra_arguments_are_a_usage_error() {
    let output = kausa(&["--version", "extra"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("error: unexpected argument 'extra'"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn run_without_a_cluster_fails_fast_with_1() {
    let output = kausa(&["run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("cannot configure the Kubernetes client"));
}

#[test]
fn run_rejects_unknown_arguments_and_bad_settle_values() {
    for args in [
        &["run", "--bogus"][..],
        &["run", "--settle-seconds"][..],
        &["run", "--settle-seconds", "soon"][..],
        &["run", "--settle-seconds", "3601"][..],
        // The explanation flags, checked before anything connects.
        &["run", "--endpoint", "https://llm.example.com/v1"][..],
        &["run", "--model", "m"][..],
        &[
            "run",
            "--endpoint",
            "http://127.0.0.1:11434/v1",
            "--model",
            "m",
        ][..],
        &["run", "--endpoint", "ftp://example.com", "--model", "m"][..],
        &[
            "run",
            "--dry-run",
            "--api-key-file",
            "/nonexistent/kausa-tests/key",
        ][..],
        &["run", "--max-calls-per-hour", "many"][..],
        &["run", "--dedup-hours", "-1"][..],
        &["run", "--metrics-address", "localhost"][..],
        &["run", "--metrics-address"][..],
    ] {
        let output = kausa(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout(&output), "", "{args:?}");
        assert!(
            stderr(&output).starts_with("error: "),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

// `kausa run --namespace` (docs/adr/0013-namespaced-install-mode.md): the accepted forms get past
// the checks and fail only on the missing cluster; the refused ones are usage errors with the
// exact message, before anything connects.

const NAMESPACE_HELP_LINE: &str =
    "  --namespace <NAME>     Watch only this namespace; repeat for more (up to 32)";

fn not_a_namespace_name(value: &str) -> String {
    format!(
        "--namespace takes a namespace name (lowercase letters, digits and '-', starting and \
         ending with a letter or digit, at most 63 characters), not '{value}'"
    )
}

/// `run` followed by one `--namespace <name>` per name.
fn run_with_namespaces<S: AsRef<str>>(names: &[S]) -> Vec<String> {
    let mut args = vec!["run".to_owned()];
    for name in names {
        args.push("--namespace".to_owned());
        args.push(name.as_ref().to_owned());
    }
    args
}

fn distinct_names(count: usize) -> Vec<String> {
    (1..=count).map(|i| format!("team-{i}")).collect()
}

fn kausa_owned(args: &[String]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    kausa(&args)
}

fn assert_usage_error(output: &Output, message: &str, case: &str) {
    assert_eq!(output.status.code(), Some(2), "{case}: {}", stderr(output));
    assert_eq!(stdout(output), "", "{case}");
    assert_eq!(
        stderr(output),
        format!("error: {message}\n\nRun 'kausa --help' for usage.\n"),
        "{case}"
    );
}

fn assert_gets_past_the_flags(output: &Output, case: &str) {
    assert_eq!(output.status.code(), Some(1), "{case}: {}", stderr(output));
    assert!(
        stdout(output).contains("cannot configure the Kubernetes client"),
        "{case}: {}",
        stdout(output)
    );
    assert_eq!(stderr(output), "", "{case}");
}

#[test]
fn run_accepts_namespaces_and_fails_only_on_the_missing_cluster() {
    let long = "a".repeat(63);
    let cases: Vec<Vec<String>> = vec![
        run_with_namespaces(&["shop"]),
        run_with_namespaces(&["shop", "web"]),
        run_with_namespaces(&distinct_names(32)),
        run_with_namespaces(&[long.as_str()]),
        run_with_namespaces(&["a", "0", "2-shop", "shop-2", "a-b-c"]),
    ];
    for args in &cases {
        assert_gets_past_the_flags(&kausa_owned(args), &format!("{args:?}"));
    }
    // The flag mixes with the others in any order.
    let mixed = [
        "run",
        "--settle-seconds",
        "5",
        "--namespace",
        "shop",
        "--dry-run",
        "--namespace",
        "web",
    ];
    assert_gets_past_the_flags(&kausa(&mixed), &format!("{mixed:?}"));
}

#[test]
fn run_refuses_a_namespace_flag_without_a_value() {
    for args in [
        &["run", "--namespace"][..],
        &["run", "--namespace", ""][..],
        &["run", "--namespace", "shop", "--namespace"][..],
    ] {
        assert_usage_error(
            &kausa(args),
            "--namespace needs a value",
            &format!("{args:?}"),
        );
    }
}

#[cfg(unix)]
#[test]
fn run_refuses_a_namespace_that_is_not_utf_8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let output = command()
        .args(["run", "--namespace"])
        .arg(OsStr::from_bytes(b"sh\xffop"))
        .output()
        .expect("the kausa binary should start");
    assert_usage_error(&output, "--namespace needs a value", "a non-UTF-8 value");
}

#[test]
fn run_refuses_a_namespace_that_is_not_a_dns_label() {
    let too_long = "a".repeat(64);
    for value in [
        "Shop",
        "SHOP",
        too_long.as_str(),
        "-shop",
        "shop-",
        "shop_1",
        "shop.web",
        "shop,web",
        " shop",
        "shop ",
        "shöp",
        "--dry-run",
    ] {
        let args = ["run", "--namespace", value];
        assert_usage_error(&kausa(&args), &not_a_namespace_name(value), value);
    }
}

#[test]
fn run_refuses_a_namespace_given_twice() {
    for names in [&["shop", "shop"][..], &["shop", "web", "shop"][..]] {
        assert_usage_error(
            &kausa_owned(&run_with_namespaces(names)),
            "--namespace 'shop' is given twice",
            &format!("{names:?}"),
        );
    }
}

#[test]
fn run_refuses_more_than_32_namespaces() {
    for count in [33, 40] {
        assert_usage_error(
            &kausa_owned(&run_with_namespaces(&distinct_names(count))),
            &format!(
                "--namespace is given {count} times; kausa watches at most 32 namespaces \
                 (watch the whole cluster instead)"
            ),
            &format!("{count} names"),
        );
    }
}

#[test]
fn run_checks_the_names_before_the_count() {
    // An invalid name among 33 is reported as such, not as one too many.
    let mut names = distinct_names(32);
    names.push("Shop".into());
    assert_usage_error(
        &kausa_owned(&run_with_namespaces(&names)),
        &not_a_namespace_name("Shop"),
        "33 names, the last one invalid",
    );
    // So is a repeat that makes the 33rd occurrence.
    let mut names = distinct_names(32);
    names.push("team-7".into());
    assert_usage_error(
        &kausa_owned(&run_with_namespaces(&names)),
        "--namespace 'team-7' is given twice",
        "32 names and a repeat",
    );
    // An invalid name comes before a later repeat.
    assert_usage_error(
        &kausa_owned(&run_with_namespaces(&["Shop", "web", "web"])),
        &not_a_namespace_name("Shop"),
        "an invalid name, then a repeat",
    );
}

#[test]
fn the_namespace_flag_has_no_other_form() {
    assert_usage_error(
        &kausa(&["run", "--namespace=shop"]),
        "unexpected argument '--namespace=shop'",
        "--namespace=shop",
    );
    // It belongs to `kausa run` only.
    assert_usage_error(
        &kausa(&["--namespace", "shop"]),
        "unknown command or option '--namespace'",
        "kausa --namespace shop",
    );
}

#[test]
fn help_lists_the_namespace_flag_under_the_options_of_run() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(
        options.contains(&NAMESPACE_HELP_LINE),
        "the help line is missing under 'Options of kausa run:': {help}"
    );
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
}

// `kausa run --history` and `--history-days` (docs/adr/0014-explanation-history.md): usage errors
// with the exact message, in the order of the checks, before anything connects; the accepted
// forms get past the flags and fail only on the missing cluster.

const ENDPOINT: &str = "https://api.example.com/v1";
const HISTORY_NEEDS_ENDPOINT: &str = "--history needs --endpoint";
const HISTORY_NOT_IN_DRY_RUN: &str =
    "--history is not used with --dry-run: no explanation is written";
const DAYS_NEED_HISTORY: &str = "--history-days needs --history";

fn not_a_number_of_days(value: &str) -> String {
    format!("--history-days takes a whole number of days from 1 to 30, not '{value}'")
}

#[test]
fn history_needs_an_endpoint() {
    for args in [
        &["run", "--history"][..],
        &["run", "--history", "--model", "m"][..],
        &["run", "--history", "--history-days", "7"][..],
        &["run", "--history", "--no-events"][..],
        &["run", "--history", "--namespace", "shop"][..],
    ] {
        assert_usage_error(&kausa(args), HISTORY_NEEDS_ENDPOINT, &format!("{args:?}"));
    }
}

#[test]
fn history_is_not_used_with_dry_run() {
    for args in [
        &["run", "--history", "--dry-run"][..],
        &["run", "--dry-run", "--history"][..],
        &["run", "--history", "--endpoint", ENDPOINT, "--dry-run"][..],
        &[
            "run",
            "--history",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
            "--dry-run",
        ][..],
        &["run", "--history", "--history-days", "3", "--dry-run"][..],
    ] {
        assert_usage_error(&kausa(args), HISTORY_NOT_IN_DRY_RUN, &format!("{args:?}"));
    }
}

#[test]
fn history_days_need_history() {
    for args in [
        &["run", "--history-days", "7"][..],
        &[
            "run",
            "--history-days",
            "30",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
        ][..],
        // Before the dry-run check.
        &["run", "--history-days", "7", "--dry-run"][..],
    ] {
        assert_usage_error(&kausa(args), DAYS_NEED_HISTORY, &format!("{args:?}"));
    }
}

#[test]
fn history_days_need_a_value() {
    for args in [
        &["run", "--history-days"][..],
        &["run", "--history-days", ""][..],
        &["run", "--history", "--endpoint", ENDPOINT, "--history-days"][..],
    ] {
        assert_usage_error(
            &kausa(args),
            "--history-days needs a value",
            &format!("{args:?}"),
        );
    }
}

#[test]
fn history_days_take_a_whole_number_from_1_to_30() {
    for value in [
        "0", "31", "abc", "-1", "7.5", "100", "1e1", " 7", "7 ", "seven",
    ] {
        let message = not_a_number_of_days(value);
        // Checked as the flag is read: before every other check.
        for args in [
            &["run", "--history-days", value][..],
            &[
                "run",
                "--history",
                "--endpoint",
                ENDPOINT,
                "--history-days",
                value,
            ][..],
            &["run", "--history-days", value, "--namespace", "Shop"][..],
            &["run", "--history-days", value, "--dry-run"][..],
        ] {
            assert_usage_error(&kausa(args), &message, &format!("{args:?}"));
        }
    }
}

#[test]
fn the_history_checks_come_in_order() {
    // --namespace first, as today.
    assert_usage_error(
        &kausa(&["run", "--namespace", "Shop", "--history"]),
        &not_a_namespace_name("Shop"),
        "a bad namespace and --history",
    );
    // Then --history-days without --history, then --dry-run, then the endpoint.
    assert_usage_error(
        &kausa(&["run", "--history-days", "7", "--dry-run", "--model", "m"]),
        DAYS_NEED_HISTORY,
        "--history-days, --dry-run",
    );
    assert_usage_error(
        &kausa(&["run", "--history", "--dry-run", "--model", "m"]),
        HISTORY_NOT_IN_DRY_RUN,
        "--history, --dry-run, no endpoint",
    );
    // Then the explanation settings, as today.
    assert_usage_error(
        &kausa(&["run", "--history", "--endpoint", ENDPOINT]),
        "--endpoint needs --model",
        "--history --endpoint without --model",
    );
    let output = kausa(&[
        "run",
        "--history",
        "--endpoint",
        "http://127.0.0.1:11434/v1",
        "--model",
        "m",
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        !stderr(&output).contains("--history"),
        "a plain http endpoint is refused by the endpoint check: {}",
        stderr(&output)
    );
}

#[test]
fn run_accepts_the_history_flags_and_fails_only_on_the_missing_cluster() {
    let cases: [&[&str]; 6] = [
        &["run", "--history", "--endpoint", ENDPOINT, "--model", "m"],
        &[
            "run",
            "--history",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
            "--history-days",
            "30",
        ],
        &[
            "run",
            "--history",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
            "--history-days",
            "30",
            "--no-events",
        ],
        &[
            "run",
            "--history-days",
            "1",
            "--model",
            "m",
            "--history",
            "--endpoint",
            ENDPOINT,
        ],
        &[
            "run",
            "--history",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
            "--history-days",
            "7",
            "--namespace",
            "shop",
        ],
        &[
            "run",
            "--history",
            "--endpoint",
            "http://127.0.0.1:11434/v1",
            "--allow-http",
            "--model",
            "m",
        ],
    ];
    for args in cases {
        assert_gets_past_the_flags(&kausa(args), &format!("{args:?}"));
    }
}

#[test]
fn help_lists_the_history_flags_and_what_kausa_writes() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let lines: Vec<&str> = help.lines().collect();
    let expected = [
        "  --no-events            Do not write Events (log the explanations only)",
        "  --history              Keep each incident's explanations in an Explanation",
        "                         object in its namespace (needs the CRD)",
        "  --history-days <N>     Delete history older than N days (1-30, default 7)",
    ];
    assert!(
        lines.windows(expected.len()).any(|w| w == expected),
        "the history lines do not follow --no-events: {help}"
    );
    let collapsed = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed
            .contains("writes nothing but Events and, with --history, its own Explanation objects"),
        "{help}"
    );
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
}

// `kausa run --notify-*` (docs/adr/0015-notifications.md): usage errors with the exact message,
// in the order of the checks, before anything connects, and never with the content of the URL
// or token file; the accepted setups get past the flags and fail only on the missing cluster.

const NOTIFY_URL_NOT_IN_DRY_RUN: &str =
    "--notify-url-file is not used with --dry-run: no explanation is written";
const NOTIFY_URL_NEEDS_ENDPOINT: &str = "--notify-url-file needs --endpoint";
const CHAT_ID_ONLY_TELEGRAM: &str =
    "--notify-telegram-chat-id is used only with --notify-format telegram";
const TOKEN_ONLY_JSON: &str = "--notify-token-file is used only with --notify-format json";
const TELEGRAM_NEEDS_CHAT_ID: &str = "--notify-format telegram needs --notify-telegram-chat-id";
const HOOK: &str = "https://hooks.example.com/services/T0/B0/fake";
const TELEGRAM_HOOK: &str = "https://api.telegram.org/bot000000:fake-token-for-tests/sendMessage";

fn needs_url_file(flag: &str) -> String {
    format!("{flag} needs --notify-url-file")
}

fn bad_chat_id(value: &str) -> String {
    format!(
        "--notify-telegram-chat-id takes an integer or @ and a channel's username, not '{value}'"
    )
}

fn bad_format(value: &str) -> String {
    format!("--notify-format takes json, slack or telegram, not '{value}'")
}

fn url_problem(path: &str, problem: &str) -> String {
    format!("the URL in --notify-url-file '{path}' {problem}")
}

const NOT_A_URL: &str = "is not a valid URL";
const NOT_HTTPS: &str = "must be https:// (or http:// with --notify-allow-http)";
const USER_INFO: &str = "must not carry a user name or password";
const TELEGRAM_PATH: &str = "must end in /bot<token>/sendMessage with --notify-format telegram";

/// A directory of the test's own under Cargo's temporary directory for integration tests.
fn notify_dir(test: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("notify-{test}"));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// Writes `content` to `name` in `dir` and returns the path as a string.
fn file(dir: &std::path::Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("a temporary file");
    path.to_str().expect("a UTF-8 path").to_owned()
}

/// `run --endpoint ENDPOINT --model m` followed by `rest`.
fn with_endpoint(rest: &[&str]) -> Vec<String> {
    ["run", "--endpoint", ENDPOINT, "--model", "m"]
        .iter()
        .chain(rest)
        .map(|s| (*s).to_owned())
        .collect()
}

/// A usage error whose message starts with `prefix` and goes on with an error of the system on
/// the same line (the text of an I/O error depends on the platform).
fn assert_usage_error_starting(output: &Output, prefix: &str, case: &str) {
    assert_eq!(output.status.code(), Some(2), "{case}: {}", stderr(output));
    assert_eq!(stdout(output), "", "{case}");
    let text = stderr(output);
    let message = text
        .strip_prefix("error: ")
        .and_then(|t| t.strip_suffix("\n\nRun 'kausa --help' for usage.\n"))
        .unwrap_or_else(|| panic!("{case}: not a usage error: {text}"));
    let rest = message
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{case}: expected '{prefix}...', got: {message}"));
    assert!(
        !rest.is_empty() && !rest.contains('\n'),
        "{case}: {message}"
    );
}

#[test]
fn notify_flags_need_the_url_file() {
    let dir = notify_dir("needs-url");
    let token = file(&dir, "token", "a-token\n");
    let cases: [(&[&str], &str); 6] = [
        (&["--notify-format", "slack"], "--notify-format"),
        (&["--notify-format", "json"], "--notify-format"),
        (&["--notify-format", "telegram"], "--notify-format"),
        (
            &["--notify-telegram-chat-id", "-1001234567890"],
            "--notify-telegram-chat-id",
        ),
        (&["--notify-token-file", &token], "--notify-token-file"),
        (&["--notify-allow-http"], "--notify-allow-http"),
    ];
    for (flags, flag) in cases {
        let message = needs_url_file(flag);
        let mut alone = vec!["run"];
        alone.extend_from_slice(flags);
        assert_usage_error(&kausa(&alone), &message, &format!("{alone:?}"));
        let args = with_endpoint(flags);
        assert_usage_error(&kausa_owned(&args), &message, &format!("{args:?}"));
        // Before the dry-run check.
        let mut dry = alone.clone();
        dry.push("--dry-run");
        assert_usage_error(&kausa(&dry), &message, &format!("{dry:?}"));
    }
}

#[test]
fn the_first_notify_flag_without_the_url_file_is_named_in_a_fixed_order() {
    let dir = notify_dir("needs-url-order");
    let token = file(&dir, "token", "a-token\n");
    // format, chat id, token file, allow-http, whatever the order of the arguments.
    let cases: [(&[&str], &str); 4] = [
        (
            &[
                "--notify-allow-http",
                "--notify-token-file",
                &token,
                "--notify-telegram-chat-id",
                "1",
                "--notify-format",
                "telegram",
            ],
            "--notify-format",
        ),
        (
            &[
                "--notify-allow-http",
                "--notify-token-file",
                &token,
                "--notify-telegram-chat-id",
                "1",
            ],
            "--notify-telegram-chat-id",
        ),
        (
            &["--notify-allow-http", "--notify-token-file", &token],
            "--notify-token-file",
        ),
        (&["--notify-allow-http", "--dry-run"], "--notify-allow-http"),
    ];
    for (flags, flag) in cases {
        let mut args = vec!["run"];
        args.extend_from_slice(flags);
        assert_usage_error(&kausa(&args), &needs_url_file(flag), &format!("{args:?}"));
    }
}

#[test]
fn the_url_file_is_not_used_with_dry_run() {
    let dir = notify_dir("dry-run");
    let url = file(&dir, "url", HOOK);
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    for args in [
        vec!["run", "--notify-url-file", &url, "--dry-run"],
        vec!["run", "--dry-run", "--notify-url-file", &url],
        // Before the endpoint check, and before the file is read.
        vec!["run", "--notify-url-file", &missing, "--dry-run"],
        vec![
            "run",
            "--notify-url-file",
            &url,
            "--notify-format",
            "telegram",
            "--dry-run",
        ],
        vec![
            "run",
            "--endpoint",
            ENDPOINT,
            "--model",
            "m",
            "--notify-url-file",
            &url,
            "--dry-run",
        ],
    ] {
        assert_usage_error(
            &kausa(&args),
            NOTIFY_URL_NOT_IN_DRY_RUN,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn the_url_file_needs_an_endpoint() {
    let dir = notify_dir("endpoint");
    let url = file(&dir, "url", HOOK);
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    for args in [
        vec!["run", "--notify-url-file", &url],
        vec!["run", "--notify-url-file", &url, "--model", "m"],
        // Before the file is read, and before the format checks.
        vec!["run", "--notify-url-file", &missing],
        vec![
            "run",
            "--notify-url-file",
            &url,
            "--notify-format",
            "telegram",
        ],
        vec![
            "run",
            "--notify-url-file",
            &url,
            "--notify-telegram-chat-id",
            "1",
        ],
    ] {
        assert_usage_error(
            &kausa(&args),
            NOTIFY_URL_NEEDS_ENDPOINT,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn the_chat_id_is_used_only_with_telegram() {
    let dir = notify_dir("chat-id");
    let url = file(&dir, "url", HOOK);
    for rest in [
        vec![
            "--notify-url-file",
            &url,
            "--notify-telegram-chat-id",
            "-100",
        ],
        vec![
            "--notify-url-file",
            &url,
            "--notify-format",
            "json",
            "--notify-telegram-chat-id",
            "@valid_name",
        ],
        vec![
            "--notify-url-file",
            &url,
            "--notify-format",
            "slack",
            "--notify-telegram-chat-id",
            "-100",
        ],
        // Before the chat id itself is checked.
        vec![
            "--notify-url-file",
            &url,
            "--notify-format",
            "slack",
            "--notify-telegram-chat-id",
            "not-an-id",
        ],
    ] {
        let args = with_endpoint(&rest);
        assert_usage_error(
            &kausa_owned(&args),
            CHAT_ID_ONLY_TELEGRAM,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn the_token_file_is_used_only_with_json() {
    let dir = notify_dir("token-format");
    let url = file(&dir, "url", HOOK);
    let telegram = file(&dir, "telegram-url", TELEGRAM_HOOK);
    let token = file(&dir, "token", "a-token\n");
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    for rest in [
        vec![
            "--notify-url-file",
            &url,
            "--notify-format",
            "slack",
            "--notify-token-file",
            &token,
        ],
        // Before the token file is read.
        vec![
            "--notify-url-file",
            &url,
            "--notify-format",
            "slack",
            "--notify-token-file",
            &missing,
        ],
        vec![
            "--notify-url-file",
            &telegram,
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            "-100",
            "--notify-token-file",
            &token,
        ],
        // Before the missing chat id.
        vec![
            "--notify-url-file",
            &telegram,
            "--notify-format",
            "telegram",
            "--notify-token-file",
            &token,
        ],
    ] {
        let args = with_endpoint(&rest);
        assert_usage_error(&kausa_owned(&args), TOKEN_ONLY_JSON, &format!("{args:?}"));
    }
}

#[test]
fn telegram_needs_a_chat_id() {
    let dir = notify_dir("telegram-chat-id");
    let url = file(&dir, "url", TELEGRAM_HOOK);
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    for rest in [
        vec!["--notify-url-file", &url, "--notify-format", "telegram"],
        // Before the file is read.
        vec!["--notify-url-file", &missing, "--notify-format", "telegram"],
    ] {
        let args = with_endpoint(&rest);
        assert_usage_error(
            &kausa_owned(&args),
            TELEGRAM_NEEDS_CHAT_ID,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn a_malformed_chat_id_is_refused_and_named() {
    let dir = notify_dir("bad-chat-id");
    let url = file(&dir, "url", TELEGRAM_HOOK);
    for value in [
        "0",
        "-0",
        "+1",
        "1.5",
        "@ab",
        "@abcd",
        "@1abcd",
        "@abc-de",
        "valid_name",
        "99999999999999999999",
        " 1",
        "@abcdefghijklmnopqrstuvwxyz0123456",
    ] {
        let args = with_endpoint(&[
            "--notify-url-file",
            &url,
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            value,
        ]);
        assert_usage_error(&kausa_owned(&args), &bad_chat_id(value), value);
    }
}

#[test]
fn notify_flags_need_their_values() {
    for (args, message) in [
        (
            &["run", "--notify-url-file"][..],
            "--notify-url-file needs a value",
        ),
        (
            &["run", "--notify-token-file"][..],
            "--notify-token-file needs a value",
        ),
        (
            &["run", "--notify-format"][..],
            "--notify-format needs a value",
        ),
        (
            &["run", "--notify-format", ""][..],
            "--notify-format needs a value",
        ),
        (
            &["run", "--notify-telegram-chat-id"][..],
            "--notify-telegram-chat-id needs a value",
        ),
        (
            &["run", "--notify-telegram-chat-id", ""][..],
            "--notify-telegram-chat-id needs a value",
        ),
    ] {
        assert_usage_error(&kausa(args), message, &format!("{args:?}"));
    }
}

#[test]
fn the_format_is_json_slack_or_telegram() {
    let dir = notify_dir("format");
    let url = file(&dir, "url", HOOK);
    for value in ["xml", "JSON", "Slack", "telegram2", "json ", "discord"] {
        let args = with_endpoint(&["--notify-url-file", &url, "--notify-format", value]);
        assert_usage_error(&kausa_owned(&args), &bad_format(value), value);
    }
}

#[test]
fn an_unreadable_or_empty_url_file_is_named_by_its_path() {
    let dir = notify_dir("url-file");
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    let directory = dir.to_str().expect("UTF-8").to_owned();
    for path in [&missing, &directory] {
        let args = with_endpoint(&["--notify-url-file", path]);
        assert_usage_error_starting(
            &kausa_owned(&args),
            &format!("--notify-url-file '{path}' cannot be read: "),
            path,
        );
    }
    for (name, content) in [("empty", ""), ("blank", "  \n\t\r\n")] {
        let path = file(&dir, name, content);
        let args = with_endpoint(&["--notify-url-file", &path]);
        assert_usage_error(
            &kausa_owned(&args),
            &format!("--notify-url-file '{path}' is empty"),
            name,
        );
    }
}

#[test]
fn the_url_must_parse_be_https_and_carry_no_user_information() {
    let dir = notify_dir("url");
    let cases = [
        ("not-a-url", "not a url", NOT_A_URL, false),
        ("relative", "hooks.example.com/services/x", NOT_A_URL, false),
        ("no-host", "https://", NOT_A_URL, false),
        (
            "plain-http",
            "http://receiver.kausa.svc:8080/hook",
            NOT_HTTPS,
            false,
        ),
        ("ftp", "ftp://hooks.example.com/x", NOT_HTTPS, true),
        ("file", "file:///etc/hostname", NOT_HTTPS, true),
        (
            "user-password",
            "https://user:pass@hooks.example.com/x",
            USER_INFO,
            false,
        ),
        ("user", "https://user@hooks.example.com/x", USER_INFO, false),
        (
            "password",
            "http://:pass@receiver.kausa.svc/x",
            USER_INFO,
            true,
        ),
    ];
    for (name, content, problem, allow_http) in cases {
        let path = file(&dir, name, &format!("{content}\n"));
        let mut rest = vec!["--notify-url-file", path.as_str()];
        if allow_http {
            rest.push("--notify-allow-http");
        }
        let args = with_endpoint(&rest);
        assert_usage_error(&kausa_owned(&args), &url_problem(&path, problem), name);
    }
}

#[test]
fn a_telegram_url_must_end_in_bot_token_send_message() {
    let dir = notify_dir("telegram-url");
    for (name, content) in [
        ("no-token", "https://api.telegram.org/bot/sendMessage"),
        (
            "get-me",
            "https://api.telegram.org/bot000000:fake-token-for-tests/getMe",
        ),
        (
            "trailing-slash",
            "https://api.telegram.org/bot000000:fake-token-for-tests/sendMessage/",
        ),
        ("no-path", "https://api.telegram.org"),
        ("slack", HOOK),
    ] {
        let path = file(&dir, name, content);
        let args = with_endpoint(&[
            "--notify-url-file",
            &path,
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            "-1001234567890",
        ]);
        assert_usage_error(
            &kausa_owned(&args),
            &url_problem(&path, TELEGRAM_PATH),
            name,
        );
    }
}

#[test]
fn an_unreadable_or_empty_token_file_is_named_by_its_path() {
    let dir = notify_dir("token-file");
    let url = file(&dir, "url", HOOK);
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    let args = with_endpoint(&["--notify-url-file", &url, "--notify-token-file", &missing]);
    assert_usage_error_starting(
        &kausa_owned(&args),
        &format!("--notify-token-file '{missing}' cannot be read: "),
        "missing",
    );
    let empty = file(&dir, "empty", " \n");
    let args = with_endpoint(&["--notify-url-file", &url, "--notify-token-file", &empty]);
    assert_usage_error(
        &kausa_owned(&args),
        &format!("--notify-token-file '{empty}' is empty"),
        "empty",
    );
}

#[test]
fn the_content_of_the_url_and_token_files_never_reaches_the_output() {
    let dir = notify_dir("sentinels");
    let sentinel = "kausa-sentinel-7Hq2Zp";
    let token = file(&dir, "token", &format!("{sentinel}-token\n"));
    let urls = [
        (format!("gopher://{sentinel}.example.com/hook"), false),
        (format!("{sentinel} not a url"), false),
        (
            format!("https://{sentinel}:pass@hooks.example.com/x"),
            false,
        ),
        (format!("http://hooks.example.com/{sentinel}"), false),
        (format!("https://hooks.example.com/{sentinel}"), true),
    ];
    for (i, (content, valid)) in urls.iter().enumerate() {
        let url = file(&dir, &format!("url-{i}"), content);
        let args = with_endpoint(&["--notify-url-file", &url, "--notify-token-file", &token]);
        let output = kausa_owned(&args);
        if *valid {
            assert_gets_past_the_flags(&output, content);
        } else {
            assert_eq!(output.status.code(), Some(2), "{content}");
        }
        assert!(!stderr(&output).contains(sentinel), "{}", stderr(&output));
        assert!(!stdout(&output).contains(sentinel), "{}", stdout(&output));
    }
    // A token read, then a later usage error.
    let url = file(&dir, "url-ok", HOOK);
    let output = kausa(&[
        "run",
        "--endpoint",
        ENDPOINT,
        "--notify-url-file",
        &url,
        "--notify-token-file",
        &token,
    ]);
    assert_usage_error(&output, "--endpoint needs --model", "a token, no --model");
    assert!(!stderr(&output).contains(sentinel));
}

#[test]
fn the_notify_checks_come_after_the_namespaces_and_the_history_and_before_the_endpoint() {
    let dir = notify_dir("order");
    let url = file(&dir, "url", HOOK);
    let plain = file(&dir, "plain-url", "http://receiver.kausa.svc:8080/hook");
    let missing = dir.join("missing").to_str().expect("UTF-8").to_owned();
    assert_usage_error(
        &kausa(&["run", "--namespace", "Shop", "--notify-format", "slack"]),
        &not_a_namespace_name("Shop"),
        "a bad namespace first",
    );
    assert_usage_error(
        &kausa(&["run", "--history-days", "7", "--notify-format", "slack"]),
        DAYS_NEED_HISTORY,
        "the history first",
    );
    assert_usage_error(
        &kausa(&["run", "--history", "--notify-url-file", &url]),
        HISTORY_NEEDS_ENDPOINT,
        "the history's endpoint first",
    );
    // Then the notify checks: the flags (a needs-url before a dry-run before an endpoint),
    assert_usage_error(
        &kausa(&["run", "--notify-format", "slack", "--dry-run"]),
        &needs_url_file("--notify-format"),
        "needs-url before dry-run",
    );
    assert_usage_error(
        &kausa(&["run", "--notify-url-file", &url, "--dry-run"]),
        NOTIFY_URL_NOT_IN_DRY_RUN,
        "dry-run before endpoint",
    );
    assert_usage_error(
        &kausa(&["run", "--endpoint", ENDPOINT, "--notify-format", "slack"]),
        &needs_url_file("--notify-format"),
        "before --endpoint needs --model",
    );
    // then the files,
    assert_usage_error_starting(
        &kausa(&["run", "--endpoint", ENDPOINT, "--notify-url-file", &missing]),
        &format!("--notify-url-file '{missing}' cannot be read: "),
        "the file before --endpoint needs --model",
    );
    // and the explanation settings last.
    assert_usage_error(
        &kausa(&["run", "--endpoint", ENDPOINT, "--notify-url-file", &url]),
        "--endpoint needs --model",
        "a valid notify setup without --model",
    );
    // --notify-allow-http allows only the webhook, and --allow-http only the endpoint.
    let output = kausa(&[
        "run",
        "--endpoint",
        "http://127.0.0.1:11434/v1",
        "--model",
        "m",
        "--notify-url-file",
        &plain,
        "--notify-allow-http",
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("plain HTTP"),
        "the endpoint check refuses it: {}",
        stderr(&output)
    );
    let args = [
        "run",
        "--endpoint",
        "http://127.0.0.1:11434/v1",
        "--allow-http",
        "--model",
        "m",
        "--notify-url-file",
        &plain,
    ];
    assert_usage_error(
        &kausa(&args),
        &url_problem(&plain, NOT_HTTPS),
        "--allow-http",
    );
}

#[test]
fn the_url_has_no_flag_or_other_form() {
    for (args, message) in [
        (
            &["run", "--notify-url", HOOK][..],
            "unexpected argument '--notify-url'",
        ),
        (
            &["run", "--notify-url-file=/x"][..],
            "unexpected argument '--notify-url-file=/x'",
        ),
        (
            &["--notify-url-file", "/x"][..],
            "unknown command or option '--notify-url-file'",
        ),
    ] {
        assert_usage_error(&kausa(args), message, &format!("{args:?}"));
    }
}

#[test]
fn run_accepts_each_notification_format_and_fails_only_on_the_missing_cluster() {
    let dir = notify_dir("valid");
    let url = file(&dir, "url", &format!("  {HOOK}\n"));
    let token = file(&dir, "token", "a-token\n");
    let telegram = file(&dir, "telegram", &format!("{TELEGRAM_HOOK}\n"));
    let self_hosted = file(
        &dir,
        "self-hosted",
        "https://tg.example.com:8443/bot1:fake/sendMessage?x=1",
    );
    let plain = file(&dir, "plain", "http://receiver.kausa.svc:8080/hook");
    let plain_telegram = file(
        &dir,
        "plain-telegram",
        "http://127.0.0.1:8081/bot1:fake/sendMessage",
    );
    let cases: Vec<Vec<&str>> = vec![
        vec!["--notify-url-file", &url],
        vec!["--notify-url-file", &url, "--notify-format", "json"],
        vec!["--notify-url-file", &url, "--notify-token-file", &token],
        vec!["--notify-url-file", &url, "--notify-format", "slack"],
        vec!["--notify-url-file", &url, "--notify-allow-http"],
        vec!["--notify-url-file", &plain, "--notify-allow-http"],
        vec![
            "--notify-url-file",
            &plain,
            "--notify-allow-http",
            "--notify-format",
            "slack",
            "--no-events",
        ],
        vec![
            "--notify-url-file",
            &telegram,
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            "-1001234567890",
        ],
        vec![
            "--notify-telegram-chat-id",
            "@valid_name",
            "--notify-format",
            "telegram",
            "--notify-url-file",
            &telegram,
        ],
        vec![
            "--notify-url-file",
            &self_hosted,
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            "12345",
        ],
        vec![
            "--notify-url-file",
            &plain_telegram,
            "--notify-allow-http",
            "--notify-format",
            "telegram",
            "--notify-telegram-chat-id",
            "-1",
        ],
        vec![
            "--notify-url-file",
            &url,
            "--notify-token-file",
            &token,
            "--history",
            "--namespace",
            "shop",
        ],
    ];
    for rest in cases {
        let args = with_endpoint(&rest);
        assert_gets_past_the_flags(&kausa_owned(&args), &format!("{args:?}"));
    }
}

#[test]
fn help_lists_the_notify_flags_and_where_data_goes() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let lines: Vec<&str> = help.lines().collect();
    let url_lines = [
        "  --notify-url-file <PATH>  Post each new explanation to the webhook whose URL is",
        "                            in this file (needs --endpoint)",
    ];
    assert!(
        lines.windows(2).any(|w| w == url_lines),
        "the --notify-url-file lines are missing: {help}"
    );
    for line in [
        "  --notify-format <FORMAT>  json (default), slack or telegram",
        "  --notify-telegram-chat-id <ID>  telegram's chat: an integer or @channel",
        "  --notify-token-file <PATH>  json only: a bearer token for the webhook",
        "  --notify-allow-http       Allow a plain http:// webhook (local or in-cluster)",
    ] {
        assert!(lines.contains(&line), "missing '{line}': {help}");
    }
    let options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    for flag in [
        "--notify-url-file",
        "--notify-format",
        "--notify-telegram-chat-id",
        "--notify-token-file",
        "--notify-allow-http",
    ] {
        assert!(
            options.iter().any(|l| l.starts_with(&format!("  {flag} "))),
            "{flag} is not under 'Options of kausa run:': {help}"
        );
    }
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
    // The "By design" paragraph: nothing unredacted to the model, and only the documented fields
    // of each new explanation to the webhook.
    let by_design: String = help
        .split("\n\n")
        .find(|p| p.starts_with("By design"))
        .expect("a 'By design' paragraph")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for words in [
        "nothing unredacted",
        "to the model",
        "--notify-url-file",
        "the webhook",
    ] {
        assert!(by_design.contains(words), "no '{words}' in: {by_design}");
    }
}

// `kausa run --no-rollouts` (docs/adr/0018-stuck-rollouts.md): the Deployment watch of `D014` is on
// by default, and the flag turns it off. It needs no other flag, so it gets past the checks and
// fails only on the missing cluster. Whether `started` says `"rollouts"`, `true` or `false`, is
// checked by the kind test: with no cluster, `kausa run` stops before it logs `started`.

const ROLLOUTS_HELP_LINE: &str =
    "  --no-rollouts          Do not watch Deployments for stalled rollouts (D014)";

#[test]
fn help_lists_the_rollouts_flag_after_the_namespace_flag() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    let at = |line: &str| options.iter().position(|l| *l == line);
    let namespace = at(NAMESPACE_HELP_LINE).expect("the --namespace help line");
    assert_eq!(
        at(ROLLOUTS_HELP_LINE),
        Some(namespace + 1),
        "the --no-rollouts help line is not right after --namespace: {help}"
    );
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
    // The paragraph on what `kausa run` watches names the Deployments.
    let lines: Vec<&str> = help.lines().collect();
    let paragraph = [
        "  current context of the kubeconfig otherwise. It watches Pods, Warning events",
        "  and Deployments, groups the failures of one workload into one incident once",
        "  they persist for the settle window, and writes one JSON object per line to",
        "  stdout. It stops on SIGTERM or Ctrl-C.",
    ];
    let start = lines
        .iter()
        .position(|l| *l == paragraph[0])
        .unwrap_or_else(|| panic!("missing '{}': {help}", paragraph[0]));
    assert_eq!(lines[start..start + paragraph.len()], paragraph, "{help}");
}

#[test]
fn run_accepts_no_rollouts_and_fails_only_on_the_missing_cluster() {
    for args in [
        &["run", "--no-rollouts"][..],
        &["run", "--namespace", "shop", "--no-rollouts", "--dry-run"][..],
        &[
            "run",
            "--no-rollouts",
            "--namespace",
            "a",
            "--namespace",
            "b",
        ][..],
        &["run", "--no-logs", "--no-rollouts", "--settle-seconds", "5"][..],
    ] {
        assert_gets_past_the_flags(&kausa(args), &format!("{args:?}"));
    }
}

#[test]
fn the_rollouts_flag_has_no_other_form() {
    for flag in ["--no-rollout", "--no-rollouts=true", "--rollouts"] {
        assert_usage_error(
            &kausa(&["run", flag]),
            &format!("unexpected argument '{flag}'"),
            flag,
        );
    }
    // It belongs to `kausa run` only.
    assert_usage_error(
        &kausa(&["--no-rollouts"]),
        "unknown command or option '--no-rollouts'",
        "kausa --no-rollouts",
    );
}
