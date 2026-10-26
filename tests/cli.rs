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
