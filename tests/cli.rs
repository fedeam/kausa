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

// `kausa run --leader-lease <NAMESPACE>/<NAME>` (docs/adr/0019-high-availability.md): act only
// while holding that Lease, as POD_NAME. Usage errors exit 2 with the exact message before
// anything connects; a valid flag with POD_NAME gets past the checks and fails only on the
// missing cluster. Whether `started` names the Lease is checked by the kind test.

const LEASE: &str = "kausa/kausa-leader";
const POD: &str = "kausa-7d9c8b6f4d-2xkqp";
const LEASE_NEEDS_VALUE: &str = "--leader-lease needs <NAMESPACE>/<NAME>";
const LEASE_TWICE: &str = "--leader-lease is given twice";
const LEASE_NEEDS_POD_NAME: &str = "--leader-lease needs POD_NAME, the name this replica holds \
                                    the Lease with (the chart sets it from the Pod's name)";
const POD_NAME_TOO_LONG: &str = "POD_NAME is longer than 253 bytes";

fn lease_shape(value: &str) -> String {
    format!("--leader-lease takes <NAMESPACE>/<NAME>, not '{value}'")
}

fn lease_namespace(part: &str) -> String {
    format!("--leader-lease: '{part}' is not a namespace name")
}

fn lease_name(part: &str) -> String {
    format!("--leader-lease: '{part}' is not a Lease name")
}

/// Runs the binary with `POD_NAME` set to `pod`, or unset with `None`.
fn kausa_as(pod: Option<&str>, args: &[&str]) -> Output {
    let mut command = command();
    match pod {
        Some(pod) => command.env("POD_NAME", pod),
        None => command.env_remove("POD_NAME"),
    };
    command
        .args(args)
        .output()
        .expect("the kausa binary should start")
}

#[test]
fn run_accepts_a_leader_lease_and_fails_only_on_the_missing_cluster() {
    let long_name = "a".repeat(253);
    let long_namespace = "n".repeat(63);
    let long_pod = "p".repeat(253);
    let with_long_name = format!("kausa/{long_name}");
    let with_long_namespace = format!("{long_namespace}/kausa-leader");
    for (pod, args) in [
        (POD, vec!["run", "--leader-lease", LEASE]),
        (
            POD,
            vec!["run", "--leader-lease", "kausa-system/tenant-kausa-leader"],
        ),
        (POD, vec!["run", "--leader-lease", "0/a"]),
        (POD, vec!["run", "--leader-lease", "kausa/kausa.leader"]),
        (POD, vec!["run", "--leader-lease", &with_long_name]),
        (POD, vec!["run", "--leader-lease", &with_long_namespace]),
        (long_pod.as_str(), vec!["run", "--leader-lease", LEASE]),
        // POD_NAME is compared byte for byte, never checked as a name.
        ("Any Name_at_all", vec!["run", "--leader-lease", LEASE]),
        // With the other flags, in any order, as the chart passes them.
        (
            POD,
            vec![
                "run",
                "--namespace",
                "shop",
                "--leader-lease",
                LEASE,
                "--settle-seconds",
                "60",
                "--dry-run",
            ],
        ),
        (
            POD,
            vec![
                "run",
                "--dry-run",
                "--leader-lease",
                LEASE,
                "--namespace",
                "kausa",
            ],
        ),
    ] {
        let case = format!("{args:?} with a POD_NAME of {} bytes", pod.len());
        assert_gets_past_the_flags(&kausa_as(Some(pod), &args), &case);
    }
}

#[test]
fn a_leader_lease_needs_a_value() {
    for args in [
        &["run", "--leader-lease"][..],
        &["run", "--leader-lease", ""][..],
        &["run", "--dry-run", "--leader-lease"][..],
    ] {
        assert_usage_error(
            &kausa_as(Some(POD), args),
            LEASE_NEEDS_VALUE,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn a_leader_lease_is_given_once() {
    for args in [
        &["run", "--leader-lease", LEASE, "--leader-lease", LEASE][..],
        &[
            "run",
            "--leader-lease",
            LEASE,
            "--leader-lease",
            "other/lease",
        ][..],
    ] {
        assert_usage_error(
            &kausa_as(Some(POD), args),
            LEASE_TWICE,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn a_leader_lease_is_one_namespace_and_one_name() {
    for value in [
        "kausa",
        "kausa-leader",
        "a/b/c",
        "/kausa-leader",
        "kausa/",
        "/",
        "//",
    ] {
        assert_usage_error(
            &kausa_as(Some(POD), &["run", "--leader-lease", value]),
            &lease_shape(value),
            value,
        );
    }
}

#[test]
fn a_leader_lease_namespace_is_a_namespace_name() {
    let too_long = "n".repeat(64);
    for part in [
        "Kausa",
        "kausa_system",
        "-kausa",
        "kausa-",
        "kausa.system",
        too_long.as_str(),
    ] {
        let value = format!("{part}/kausa-leader");
        assert_usage_error(
            &kausa_as(Some(POD), &["run", "--leader-lease", &value]),
            &lease_namespace(part),
            part,
        );
    }
}

#[test]
fn a_leader_lease_name_is_a_dns_subdomain() {
    let too_long = "a".repeat(254);
    for part in [
        "Kausa-Leader",
        "kausa_leader",
        "-kausa",
        "kausa-",
        ".kausa",
        "kausa.",
        "kausa leader",
        too_long.as_str(),
    ] {
        let value = format!("kausa/{part}");
        let shown = if part.len() > 64 { "254 bytes" } else { part };
        assert_usage_error(
            &kausa_as(Some(POD), &["run", "--leader-lease", &value]),
            &lease_name(part),
            shown,
        );
    }
}

#[test]
fn a_leader_lease_needs_a_pod_name() {
    for pod in [None, Some("")] {
        assert_usage_error(
            &kausa_as(pod, &["run", "--leader-lease", LEASE]),
            LEASE_NEEDS_POD_NAME,
            &format!("POD_NAME {:?}", pod.map(str::len)),
        );
    }
    let too_long = "p".repeat(254);
    assert_usage_error(
        &kausa_as(Some(&too_long), &["run", "--leader-lease", LEASE]),
        POD_NAME_TOO_LONG,
        "POD_NAME of 254 bytes",
    );
    // A malformed value is refused in the loop, before POD_NAME is looked at.
    assert_usage_error(
        &kausa_as(None, &["run", "--leader-lease", "kausa"]),
        &lease_shape("kausa"),
        "no POD_NAME and no '/'",
    );
    // Without the flag, POD_NAME is not needed (the Events' instance falls back to kausa).
    assert_gets_past_the_flags(&kausa_as(None, &["run"]), "run without POD_NAME");
}

#[test]
fn the_leader_lease_flag_has_no_other_form() {
    for flag in [
        "--leader-lease=kausa/kausa-leader",
        "--leader-election",
        "--lease",
    ] {
        assert_usage_error(
            &kausa_as(Some(POD), &["run", flag]),
            &format!("unexpected argument '{flag}'"),
            flag,
        );
    }
    assert_usage_error(
        &kausa_as(Some(POD), &["--leader-lease", LEASE]),
        "unknown command or option '--leader-lease'",
        "kausa --leader-lease",
    );
}

#[test]
fn help_lists_the_leader_lease_flag_and_what_kausa_writes() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    let lines = [
        "  --leader-lease <NS>/<NAME>  Act only while holding this Lease; wait as a",
        "                              standby otherwise (for two replicas)",
    ];
    assert!(
        options.windows(2).any(|w| w == lines),
        "the --leader-lease lines are not under 'Options of kausa run:': {help}"
    );
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
    let by_design: String = help
        .split("\n\n")
        .find(|p| p.starts_with("By design"))
        .expect("a 'By design' paragraph")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for words in [
        "writes nothing but Events and, with --history, its own Explanation objects, and, with \
         --leader-lease, the spec of its one Lease,",
        "nothing unredacted",
    ] {
        assert!(by_design.contains(words), "no '{words}' in: {by_design}");
    }
    // A lost Lease is not an exit: the exit statuses do not change.
    let collapsed = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains(
            "Exit status: 0 on success or a clean stop, 1 when it cannot start or a watch ends \
             (watch errors are logged and retried), 2 on a usage error or when stdout cannot be \
             written."
        ),
        "{help}"
    );
}

// The call budget refills at a steady rate from a start burst of three
// (docs/adr/0021-refill-the-call-budget.md). The flags, their defaults and their ranges do not
// change; only their help lines do.

const BUDGET_HELP_LINES: [&str; 4] = [
    "  --max-calls-per-hour <N>  Model calls allowed in any hour (default 30); each",
    "                            start allows 3 at once, then one every 3600/N seconds",
    "  --max-calls-per-day <N>   Model calls allowed in any day, per process",
    "                            (default 200)",
];

#[test]
fn help_explains_the_refilling_budget() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(
        options.windows(4).any(|w| w == BUDGET_HELP_LINES),
        "the budget lines are not under 'Options of kausa run:': {help}"
    );
    // The lines of v0.4 are gone.
    for old in [
        "Model calls allowed per hour",
        "Model calls allowed per day",
    ] {
        assert!(!help.contains(old), "'{old}' is still in: {help}");
    }
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
}

// `kausa check-endpoint` and `--model-timeout` (docs/adr/0020-check-the-endpoint.md), written
// before the code. Usage errors exit 2 before anything connects; against a fake endpoint on
// localhost, the check sends one request, prints one `endpoint check` line and exits 0 or 1.
// Every run has a nonexistent kubeconfig: the check reads nothing from a cluster. Whether
// `started` shows `model_timeout_seconds` is checked by the kind test: with no cluster, `kausa run`
// stops before it logs `started`.

const PLAIN_HTTP: &str = "the endpoint uses plain HTTP: pass --allow-http for an endpoint inside \
                          the cluster or the same private network";
const NEEDS_ENDPOINT_OR_DRY_RUN: &str = "--model, --api-key-file, --allow-http, --no-json-schema \
                                         and --model-timeout need --endpoint or --dry-run";
const RUN_ONLY: [&str; 19] = [
    "--settle-seconds",
    "--namespace",
    "--no-rollouts",
    "--dry-run",
    "--no-logs",
    "--no-events",
    "--history",
    "--history-days",
    "--notify-url-file",
    "--notify-format",
    "--notify-telegram-chat-id",
    "--notify-token-file",
    "--notify-allow-http",
    "--max-calls-per-hour",
    "--max-calls-per-day",
    "--max-input-tokens",
    "--dedup-hours",
    "--metrics-address",
    "--leader-lease",
];

fn bad_timeout(value: &str) -> String {
    format!("--model-timeout takes a whole number of seconds from 10 to 600, not '{value}'")
}

fn run_only(flag: &str) -> String {
    format!("{flag} is a flag of kausa run, not of kausa check-endpoint")
}

/// `check-endpoint --endpoint ENDPOINT --model m` followed by `rest`.
fn check_with(rest: &[&str]) -> Vec<String> {
    ["check-endpoint", "--endpoint", ENDPOINT, "--model", "m"]
        .iter()
        .chain(rest)
        .map(|s| (*s).to_owned())
        .collect()
}

#[test]
fn check_endpoint_needs_an_endpoint_and_a_model() {
    for args in [
        &["check-endpoint"][..],
        &["check-endpoint", "--model", "m"][..],
        &["check-endpoint", "--model", "m", "--allow-http"][..],
        // No endpoint comes before the key file is read.
        &[
            "check-endpoint",
            "--model",
            "m",
            "--api-key-file",
            "/nonexistent/kausa-tests/key",
        ][..],
    ] {
        assert_usage_error(
            &kausa(args),
            "kausa check-endpoint needs --endpoint",
            &format!("{args:?}"),
        );
    }
    assert_usage_error(
        &kausa(&["check-endpoint", "--endpoint", ENDPOINT]),
        "--endpoint needs --model",
        "no model",
    );
}

#[test]
fn check_endpoint_flags_need_their_values() {
    for flag in ["--endpoint", "--model", "--model-timeout", "--api-key-file"] {
        assert_usage_error(
            &kausa(&["check-endpoint", flag]),
            &format!("{flag} needs a value"),
            flag,
        );
    }
    for flag in ["--endpoint", "--model", "--model-timeout"] {
        assert_usage_error(
            &kausa(&["check-endpoint", flag, ""]),
            &format!("{flag} needs a value"),
            &format!("{flag} ''"),
        );
    }
}

#[test]
fn check_endpoint_checks_the_endpoint_as_run_does() {
    assert_usage_error(
        &kausa(&[
            "check-endpoint",
            "--endpoint",
            "http://127.0.0.1:11434/v1",
            "--model",
            "m",
        ]),
        PLAIN_HTTP,
        "plain http",
    );
    assert_usage_error(
        &kausa(&[
            "check-endpoint",
            "--endpoint",
            "ftp://example.com",
            "--model",
            "m",
            "--allow-http",
        ]),
        "the endpoint 'ftp://example.com' is not an http(s) URL",
        "ftp",
    );
    // The scheme is checked before the model: plain http without a model names the scheme.
    assert_usage_error(
        &kausa(&["check-endpoint", "--endpoint", "http://127.0.0.1:11434/v1"]),
        PLAIN_HTTP,
        "plain http, no model",
    );
}

#[test]
fn check_endpoint_names_an_unreadable_key_file() {
    let prefix = "cannot read the API key file '/nonexistent/kausa-tests/key': ";
    assert_usage_error_starting(
        &kausa_owned(&check_with(&[
            "--api-key-file",
            "/nonexistent/kausa-tests/key",
        ])),
        prefix,
        "unreadable",
    );
    // The key file is read before the scheme and the model are checked.
    assert_usage_error_starting(
        &kausa(&[
            "check-endpoint",
            "--endpoint",
            "ftp://example.com",
            "--api-key-file",
            "/nonexistent/kausa-tests/key",
        ]),
        prefix,
        "unreadable before the scheme",
    );
}

#[test]
fn check_endpoint_refuses_the_flags_of_run() {
    for flag in RUN_ONLY {
        // Refused at the flag itself, with or without a value after it.
        assert_usage_error(&kausa_owned(&check_with(&[flag])), &run_only(flag), flag);
        assert_usage_error(
            &kausa(&["check-endpoint", flag, "1"]),
            &run_only(flag),
            &format!("{flag} first"),
        );
    }
}

#[test]
fn check_endpoint_refuses_a_flag_given_twice() {
    let dir = notify_dir("check-twice");
    let key = file(&dir, "key", "a-key\n");
    for (flag, value) in [
        ("--endpoint", Some(ENDPOINT)),
        ("--model", Some("m")),
        ("--api-key-file", Some(key.as_str())),
        ("--model-timeout", Some("30")),
        ("--allow-http", None),
        ("--no-json-schema", None),
    ] {
        let mut args = vec!["check-endpoint"];
        for _ in 0..2 {
            args.push(flag);
            args.extend(value);
        }
        assert_usage_error(&kausa(&args), &format!("{flag} is given twice"), flag);
    }
}

#[test]
fn check_endpoint_refuses_anything_else() {
    for arg in ["--bogus", "extra", "-h", "--version", "run"] {
        assert_usage_error(
            &kausa_owned(&check_with(&[arg])),
            &format!("unexpected argument '{arg}'"),
            arg,
        );
    }
}

#[test]
fn check_endpoint_reports_the_first_bad_argument_in_order() {
    let cases: [(&[&str], String); 5] = [
        (
            &["check-endpoint", "--bogus", "--dry-run"],
            "unexpected argument '--bogus'".to_owned(),
        ),
        (
            &["check-endpoint", "--dry-run", "--bogus"],
            run_only("--dry-run"),
        ),
        (
            &["check-endpoint", "--model-timeout", "9", "--namespace", "a"],
            bad_timeout("9"),
        ),
        (
            &[
                "check-endpoint",
                "--allow-http",
                "--allow-http",
                "--dry-run",
            ],
            "--allow-http is given twice".to_owned(),
        ),
        // Every argument is read before the endpoint is missed.
        (
            &["check-endpoint", "--model", "m", "--history"],
            run_only("--history"),
        ),
    ];
    for (args, message) in cases {
        assert_usage_error(&kausa(args), &message, &format!("{args:?}"));
    }
}

#[test]
fn the_model_timeout_takes_10_to_600_seconds_on_both_commands() {
    for value in [
        "9",
        "601",
        "abc",
        "0",
        "-1",
        "10.5",
        "1e2",
        " 30",
        "99999999999",
    ] {
        assert_usage_error(
            &kausa_owned(&check_with(&["--model-timeout", value])),
            &bad_timeout(value),
            &format!("check {value}"),
        );
        assert_usage_error(
            &kausa_owned(&with_endpoint(&["--model-timeout", value])),
            &bad_timeout(value),
            &format!("run {value}"),
        );
    }
    // Accepted on run: it gets past the flags and fails only on the missing cluster.
    for value in ["10", "120", "600"] {
        assert_gets_past_the_flags(
            &kausa_owned(&with_endpoint(&["--model-timeout", value])),
            value,
        );
        assert_gets_past_the_flags(
            &kausa(&["run", "--dry-run", "--model-timeout", value]),
            &format!("dry-run {value}"),
        );
    }
}

#[test]
fn run_takes_the_model_flags_only_with_an_endpoint_or_dry_run() {
    for args in [
        &["run", "--model-timeout", "30"][..],
        &["run", "--model", "m"][..],
        &["run", "--allow-http"][..],
        &["run", "--no-json-schema"][..],
        &["run", "--api-key-file", "/nonexistent/kausa-tests/key"][..],
    ] {
        assert_usage_error(
            &kausa(args),
            NEEDS_ENDPOINT_OR_DRY_RUN,
            &format!("{args:?}"),
        );
    }
}

#[test]
fn help_lists_check_endpoint_and_the_model_timeout() {
    let output = kausa(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    let usage: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Usage:")
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(
        usage
            .iter()
            .any(|l| l.starts_with("  kausa check-endpoint [OPTIONS]")
                && l.contains("Send one synthetic request to the model endpoint")),
        "no check-endpoint line under 'Usage:': {help}"
    );
    let run_options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa run:")
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(
        run_options
            .iter()
            .any(|l| l.starts_with("  --model-timeout <N>")
                && l.contains("A call's total timeout in seconds (10-600, default 120)")),
        "no --model-timeout line under 'Options of kausa run:': {help}"
    );
    let check_options: Vec<&str> = help
        .lines()
        .skip_while(|l| *l != "Options of kausa check-endpoint:")
        .skip(1)
        .take_while(|l| !l.is_empty())
        .collect();
    for flag in [
        "--endpoint <URL>",
        "--model <NAME>",
        "--api-key-file <PATH>",
        "--allow-http",
        "--no-json-schema",
        "--model-timeout <N>",
    ] {
        assert!(
            check_options.iter().any(|l| l.contains(flag)),
            "{flag} is not under 'Options of kausa check-endpoint:': {help}"
        );
    }
    for flag in RUN_ONLY {
        assert!(
            !check_options.iter().any(|l| l.contains(flag)),
            "{flag} is under 'Options of kausa check-endpoint:': {help}"
        );
    }
    assert!(help.lines().all(|l| l.len() <= 84), "{help}");
}

/// A fake model endpoint on localhost: answers every request with `response` and keeps each
/// whole request.
struct FakeEndpoint {
    port: u16,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeEndpoint {
    fn start(response: String) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let port = listener.local_addr().expect("an address").port();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = std::sync::Arc::clone(&requests);
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut request = Vec::new();
                let mut buffer = vec![0; 65536];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                kept.lock()
                    .expect("the requests")
                    .push(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { port, requests }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("the requests").clone()
    }
}

fn http_reply(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A chat completion whose content is a valid explanation, with its token counts.
fn valid_completion() -> String {
    let content = serde_json::json!({
        "summary": "deployment-1 in namespace-1 crashes: namespace-1_URL is not set.",
        "cause": "container-1 exits with code 1.",
        "steps": ["Set the variable."],
        "confidence": "high",
    })
    .to_string();
    let body = serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 30},
    });
    http_reply("200 OK", "", &body.to_string())
}

/// Runs `kausa check-endpoint --endpoint <url> --model example-model` and `rest`, with `key` in
/// `KAUSA_API_KEY` when given.
fn check_against(url: &str, rest: &[&str], key: Option<&str>) -> Output {
    let mut command = command();
    command.args([
        "check-endpoint",
        "--endpoint",
        url,
        "--model",
        "example-model",
        "--allow-http",
    ]);
    command.args(rest);
    if let Some(key) = key {
        command.env("KAUSA_API_KEY", key);
    }
    command.output().expect("the kausa binary should start")
}

/// The one line the check printed, parsed.
fn check_line(output: &Output) -> serde_json::Value {
    let text = stdout(output);
    assert_eq!(text.lines().count(), 1, "not one line: {text}");
    assert!(text.ends_with('\n'), "{text}");
    let line: serde_json::Value = serde_json::from_str(text).expect("a JSON line");
    assert_eq!(line["msg"], "endpoint check", "{text}");
    line
}

/// The fields every `endpoint check` line has, beside `ts`, `level` and `msg`.
const CHECK_FIELDS: [&str; 22] = [
    "answer_valid",
    "api_key",
    "authenticated",
    "cause",
    "class",
    "connected",
    "detail",
    "endpoint",
    "hint",
    "json_schema",
    "latency_ms",
    "model",
    "model_found",
    "prompt_version",
    "reason",
    "request_accepted",
    "result",
    "retry_after_seconds",
    "status",
    "timeout_seconds",
    "tokens_in",
    "tokens_out",
];

fn assert_fields(line: &serde_json::Value, case: &str) {
    let object = line.as_object().expect("an object");
    let mut names: Vec<&str> = object.keys().map(String::as_str).collect();
    names.retain(|n| !["ts", "level", "msg"].contains(n));
    names.sort_unstable();
    assert_eq!(names, CHECK_FIELDS, "{case}: the fields of the line");
    assert!(line["latency_ms"].is_u64(), "{case}: {line}");
    assert_eq!(line["prompt_version"], "3", "{case}: {line}");
}

/// The steps of a line: connected, authenticated, model_found, request_accepted, answer_valid.
fn steps(line: &serde_json::Value) -> [serde_json::Value; 5] {
    [
        line["connected"].clone(),
        line["authenticated"].clone(),
        line["model_found"].clone(),
        line["request_accepted"].clone(),
        line["answer_valid"].clone(),
    ]
}

fn golden(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/tests/golden/check-endpoint-request-{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).expect("a golden file");
    serde_json::from_str(&text).expect("JSON")
}

/// The body of a request the fake endpoint kept.
fn body_of(request: &str) -> serde_json::Value {
    let body = request.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    serde_json::from_str(body).expect("a JSON body")
}

#[test]
fn a_passing_check_exits_0_with_every_step_proven() {
    use serde_json::{Value, json};
    let endpoint = FakeEndpoint::start(valid_completion());
    let output = check_against(&endpoint.url(), &[], None);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let line = check_line(&output);
    assert_fields(&line, "pass");
    assert_eq!(line["level"], "info");
    assert_eq!(line["result"], "ok");
    let t = Value::Bool(true);
    assert_eq!(
        steps(&line),
        [t.clone(), t.clone(), t.clone(), t.clone(), t]
    );
    for field in [
        "cause",
        "class",
        "reason",
        "hint",
        "detail",
        "retry_after_seconds",
    ] {
        assert_eq!(line[field], Value::Null, "{field}: {line}");
    }
    assert_eq!(line["status"], 200);
    assert_eq!(line["endpoint"], json!(endpoint.origin()));
    assert_eq!(line["model"], "example-model");
    assert_eq!(line["json_schema"], true);
    assert_eq!(line["timeout_seconds"], 120);
    assert_eq!(line["api_key"], false);
    assert_eq!(
        (line["tokens_in"].clone(), line["tokens_out"].clone()),
        (json!(120), json!(30))
    );
    // Never the answer's text.
    assert!(!stdout(&output).contains("Set the variable"), "{line}");
    // One request, to the chat completions path, no key, the golden body.
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].starts_with("POST /v1/chat/completions "),
        "{}",
        requests[0].lines().next().unwrap_or_default()
    );
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    assert_eq!(body_of(&requests[0]), golden("json-schema"));
}

#[test]
fn the_check_sends_the_golden_body_in_both_output_modes() {
    for (flags, name, json_schema) in [
        (&[][..], "json-schema", true),
        (&["--no-json-schema"][..], "instructions", false),
    ] {
        let endpoint = FakeEndpoint::start(valid_completion());
        let output = check_against(&endpoint.url(), flags, None);
        assert_eq!(output.status.code(), Some(0), "{name}");
        assert_eq!(check_line(&output)["json_schema"], json_schema, "{name}");
        let requests = endpoint.requests();
        assert_eq!(requests.len(), 1, "{name}");
        assert_eq!(body_of(&requests[0]), golden(name), "{name}");
    }
}

#[test]
fn a_failed_check_exits_1_and_names_the_step_that_failed() {
    use serde_json::{Value, json};
    let n = Value::Null;
    let (t, f) = (Value::Bool(true), Value::Bool(false));
    let refusal = json!({"choices": [{"message": {"content": null, "refusal": "REFUSAL-CANARY"},
                                      "finish_reason": "stop"}]})
    .to_string();
    let not_kausa = json!({"choices": [{"message": {"content": "hello"}}]}).to_string();
    // (case, response, cause, class, status, steps)
    type Case = (&'static str, String, &'static str, Value, Value, [Value; 5]);
    let cases: Vec<Case> = vec![
        (
            "404",
            http_reply(
                "404 Not Found",
                "",
                r#"{"error":{"message":"The model does not exist","code":"model_not_found"}}"#,
            ),
            "not_found",
            json!("configuration"),
            json!(404),
            [t.clone(), n.clone(), f.clone(), n.clone(), n.clone()],
        ),
        (
            "401",
            http_reply("401 Unauthorized", "", "{}"),
            "key",
            json!("configuration"),
            json!(401),
            [t.clone(), f.clone(), n.clone(), n.clone(), n.clone()],
        ),
        (
            "429 quota",
            http_reply(
                "429 Too Many Requests",
                "",
                r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}"#,
            ),
            "quota",
            json!("configuration"),
            json!(429),
            [t.clone(), n.clone(), n.clone(), n.clone(), n.clone()],
        ),
        (
            "400",
            http_reply("400 Bad Request", "", "{}"),
            "rejected",
            json!("rejected"),
            json!(400),
            [t.clone(), n.clone(), n.clone(), f.clone(), n.clone()],
        ),
        (
            "413",
            http_reply("413 Payload Too Large", "", ""),
            "too_large",
            json!("rejected"),
            json!(413),
            [t.clone(), n.clone(), n.clone(), f.clone(), n.clone()],
        ),
        (
            "503",
            http_reply("503 Service Unavailable", "", ""),
            "server_error",
            json!("transient"),
            json!(503),
            [t.clone(), n.clone(), n.clone(), n.clone(), n.clone()],
        ),
        (
            "200 OK",
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\
             connection: close\r\n\r\nOK"
                .to_owned(),
            "not_an_answer",
            json!("configuration"),
            json!(200),
            [t.clone(), t.clone(), t.clone(), t.clone(), f.clone()],
        ),
        (
            "refusal",
            http_reply("200 OK", "", &refusal),
            "no_content",
            json!("rejected"),
            json!(200),
            [t.clone(), t.clone(), t.clone(), t.clone(), f.clone()],
        ),
        (
            "invalid",
            http_reply("200 OK", "", &not_kausa),
            "invalid_answer",
            Value::Null,
            json!(200),
            [t.clone(), t.clone(), t.clone(), t.clone(), f.clone()],
        ),
    ];
    for (case, response, cause, class, status, expected) in cases {
        let endpoint = FakeEndpoint::start(response);
        let output = check_against(&endpoint.url(), &[], None);
        assert_eq!(output.status.code(), Some(1), "{case}: {}", stderr(&output));
        assert_eq!(stderr(&output), "", "{case}");
        let line = check_line(&output);
        assert_fields(&line, case);
        assert_eq!(line["level"], "error", "{case}");
        assert_eq!(line["result"], "failed", "{case}");
        assert_eq!(line["cause"], cause, "{case}");
        assert_eq!(line["class"], class, "{case}");
        assert_eq!(line["status"], status, "{case}");
        assert_eq!(steps(&line), expected, "{case}: {line}");
        assert!(line["reason"].is_string(), "{case}: {line}");
        assert!(line["hint"].is_string(), "{case}: {line}");
        assert_eq!(line["retry_after_seconds"], Value::Null, "{case}");
        assert_eq!(line["endpoint"], json!(endpoint.origin()), "{case}");
        assert!(
            !stdout(&output).contains("REFUSAL-CANARY"),
            "{case}: the model's text"
        );
        // One request: the check never retries.
        assert_eq!(endpoint.requests().len(), 1, "{case}");
    }
}

#[test]
fn a_failed_check_carries_the_reason_hint_and_detail_run_logs() {
    use serde_json::{Value, json};
    let body = r#"{"error":{"message":"The model does not exist","code":"model_not_found"}}"#;
    let endpoint = FakeEndpoint::start(http_reply("404 Not Found", "", body));
    let line = check_line(&check_against(&endpoint.url(), &[], None));
    assert_eq!(line["reason"], "the endpoint answered 404 Not Found");
    assert_eq!(
        line["hint"],
        "the model or the URL is wrong, or the model is no longer offered: check model.name \
         (--model) and that model.endpoint is the base URL"
    );
    assert_eq!(line["detail"], body);
    assert_eq!(
        (line["tokens_in"].clone(), line["tokens_out"].clone()),
        (Value::Null, Value::Null)
    );

    let not_kausa = json!({"choices": [{"message": {"content": "hello"}}]}).to_string();
    let endpoint = FakeEndpoint::start(http_reply("200 OK", "", &not_kausa));
    let line = check_line(&check_against(&endpoint.url(), &[], None));
    assert!(
        line["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("invalid answer: ")),
        "{line}"
    );
    assert_eq!(
        line["hint"],
        "the model answered, but not with kausa's JSON: try another model, or \
         model.jsonSchema=false (--no-json-schema) if the server ignores response_format"
    );
    assert_eq!(line["detail"], Value::Null);
    assert!(!line.to_string().contains("hello"), "the answer's text");

    let refusal = json!({"choices": [{"message": {"content": null},
                                      "finish_reason": "length"}]})
    .to_string();
    let endpoint = FakeEndpoint::start(http_reply("200 OK", "", &refusal));
    let line = check_line(&check_against(&endpoint.url(), &[], None));
    assert_eq!(
        line["reason"],
        "the model answered with no content: it ran out of output tokens"
    );
    assert_eq!(line["detail"], Value::Null);

    // Without the JSON schema, the hint of a refused request blames no response_format.
    let endpoint = FakeEndpoint::start(http_reply("400 Bad Request", "", "{}"));
    let line = check_line(&check_against(&endpoint.url(), &["--no-json-schema"], None));
    assert_eq!(line["hint"], "the endpoint refused the request: see detail");
}

#[test]
fn a_check_shows_the_clamped_retry_after_and_never_waits_for_it() {
    let endpoint = FakeEndpoint::start(http_reply(
        "429 Too Many Requests",
        "retry-after: 99999\r\n",
        "{}",
    ));
    let start = std::time::Instant::now();
    let output = check_against(&endpoint.url(), &[], None);
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(output.status.code(), Some(1));
    let line = check_line(&output);
    assert_eq!(line["cause"], "rate_limited");
    assert_eq!(line["class"], "transient");
    assert_eq!(line["retry_after_seconds"], 600);
    assert_eq!(endpoint.requests().len(), 1, "a retry was sent");
}

#[test]
fn a_closed_port_fails_to_connect() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let output = check_against(&format!("http://127.0.0.1:{port}/v1"), &[], None);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let line = check_line(&output);
    assert_fields(&line, "closed port");
    assert_eq!(line["cause"], "connection");
    assert_eq!(line["class"], "transient");
    assert_eq!(line["status"], serde_json::Value::Null);
    let n = serde_json::Value::Null;
    assert_eq!(
        steps(&line),
        [
            serde_json::Value::Bool(false),
            n.clone(),
            n.clone(),
            n.clone(),
            n
        ]
    );
    assert_eq!(line["endpoint"], format!("http://127.0.0.1:{port}"));
}

#[test]
fn the_model_timeout_is_the_checks_timeout() {
    let endpoint = FakeEndpoint::start(valid_completion());
    for value in ["10", "600"] {
        let output = check_against(&endpoint.url(), &["--model-timeout", value], None);
        assert_eq!(output.status.code(), Some(0), "{value}");
        assert_eq!(
            check_line(&output)["timeout_seconds"],
            value.parse::<u64>().expect("a number"),
            "{value}"
        );
    }
}

#[test]
fn the_key_and_the_urls_user_information_never_reach_the_output() {
    // Built with concat! so that a secret scanner does not take the fake key for a real one.
    let key = concat!("kausa-sentinel-", "key-4Rt8Wq");
    let password = "kausa-sentinel-password-9Kd2";
    let dir = notify_dir("check-key");
    let key_file = file(&dir, "key", &format!("  {key}\n"));
    let blank_file = file(&dir, "blank", " \n");
    // A pass, a refusal and a failed connection, with the key from the file or the environment.
    let endpoint = FakeEndpoint::start(valid_completion());
    let refusing = FakeEndpoint::start(http_reply(
        "401 Unauthorized",
        "",
        r#"{"error":{"message":"Incorrect API key provided"}}"#,
    ));
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let with_user = |port: u16| format!("http://user:{password}@127.0.0.1:{port}/v1");
    for (case, port, code) in [
        ("pass", endpoint.port, 0),
        ("refused", refusing.port, 1),
        ("closed", closed, 1),
    ] {
        for from_file in [true, false] {
            let output = if from_file {
                check_against(&with_user(port), &["--api-key-file", &key_file], None)
            } else {
                check_against(&with_user(port), &[], Some(key))
            };
            assert_eq!(output.status.code(), Some(code), "{case}");
            for (stream, text) in [("stdout", stdout(&output)), ("stderr", stderr(&output))] {
                assert!(!text.contains(key), "{case}: the API key is in {stream}");
                assert!(
                    !text.contains(password),
                    "{case}: the URL's password is in {stream}"
                );
                assert!(
                    !text.contains("user:"),
                    "{case}: the URL's user name is in {stream}"
                );
            }
            let line = check_line(&output);
            assert_eq!(line["api_key"], true, "{case}");
            assert_eq!(
                line["endpoint"],
                format!("http://127.0.0.1:{port}"),
                "{case}"
            );
        }
    }
    // The key went to the endpoint, as a bearer token, and nowhere else.
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            request.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                key.to_ascii_lowercase()
            )),
            "no bearer key"
        );
    }
    // A key file of white space is no key.
    let endpoint = FakeEndpoint::start(valid_completion());
    let output = check_against(&endpoint.url(), &["--api-key-file", &blank_file], None);
    assert_eq!(check_line(&output)["api_key"], false);
    assert!(
        !endpoint.requests()[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer")
    );
}

// A failed write exits with 2, as for the help (Linux only: /dev/full).
#[cfg(target_os = "linux")]
#[test]
fn a_check_that_cannot_write_its_line_exits_with_2() {
    let endpoint = FakeEndpoint::start(valid_completion());
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full should open for writing");
    let output = command()
        .args([
            "check-endpoint",
            "--endpoint",
            &endpoint.url(),
            "--model",
            "example-model",
            "--allow-http",
        ])
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
