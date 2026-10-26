//! Runs the built binary the way a user would and checks what it prints today.

use std::process::{Command, Output};

fn kausa(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kausa"))
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
fn help_shows_usage_and_says_what_is_not_implemented() {
    for flag in ["--help", "-h"] {
        let output = kausa(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let help = stdout(&output);
        assert!(help.starts_with(concat!("kausa ", env!("CARGO_PKG_VERSION"), "\n")));
        assert!(help.contains("Usage:"), "{flag}: {help}");
        assert!(help.contains("NOT IMPLEMENTED YET"), "{flag}: {help}");
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
    let output = Command::new(env!("CARGO_BIN_EXE_kausa"))
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
fn run_rejects_unknown_arguments_and_bad_settle_values() {
    for args in [
        &["run", "--dry-run"][..],
        &["run", "--settle-seconds"][..],
        &["run", "--settle-seconds", "soon"][..],
        &["run", "--settle-seconds", "3601"][..],
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
