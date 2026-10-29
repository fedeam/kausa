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
