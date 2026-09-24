//! Black-box tests for the `ods` binary.

use std::process::{Command, Output};

fn ods(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("ODS_LOG")
        .output()
        .expect("failed to spawn ods")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

#[test]
fn version_defaults_to_plain_when_not_a_terminal() {
    let out = ods(&["version"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.starts_with("ods: "), "{text}");
    assert!(
        !text.contains('\x1b'),
        "plain output must not contain ANSI escapes: {text:?}"
    );
}

#[test]
fn version_json_envelope() {
    let out = ods(&["version", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(value["command"], "version");
    assert_eq!(value["schema_version"]["major"], 0);
    assert_eq!(value["result"]["ods_version"], env!("CARGO_PKG_VERSION"));
    assert!(value["diagnostics"].as_array().is_some_and(Vec::is_empty));
}

#[test]
fn global_flags_work_before_the_subcommand() {
    let out = ods(&["-o", "json", "version"]);
    assert!(out.status.success());
    assert!(stdout(&out).trim_start().starts_with('{'));
}

#[test]
fn human_output_honours_color_flag() {
    let colored = stdout(&ods(&["version", "-o", "human", "--color", "always"]));
    assert!(
        colored.contains('\x1b'),
        "expected ANSI escapes: {colored:?}"
    );
    let uncolored = stdout(&ods(&["version", "-o", "human", "--color", "never"]));
    assert!(
        !uncolored.contains('\x1b'),
        "unexpected ANSI escapes: {uncolored:?}"
    );
}

#[test]
fn color_always_overrides_no_color() {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["version", "-o", "human", "--color", "always"])
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to spawn ods");
    assert!(
        stdout(&out).contains('\x1b'),
        "--color always must win over NO_COLOR"
    );
}

#[test]
fn no_color_env_disables_auto_colour() {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["version", "-o", "human"])
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to spawn ods");
    assert!(!stdout(&out).contains('\x1b'));
}

#[test]
fn json_and_output_flags_conflict() {
    let out = ods(&["version", "--json", "-o", "plain"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unimplemented_module_exits_with_code_3() {
    let out = ods(&["state"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty(), "diagnostics belong on stderr");
}

#[test]
fn logs_go_to_stderr_and_never_corrupt_json() {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["-vv", "--json", "version"])
        .env_remove("ODS_LOG")
        .output()
        .expect("failed to spawn ods");
    assert!(out.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(value["command"], "version");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dispatching"),
        "expected a debug log on stderr: {stderr}"
    );
}

#[test]
fn ods_log_env_overrides_verbosity_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["-q", "version"])
        .env("ODS_LOG", "debug")
        .output()
        .expect("failed to spawn ods");
    assert!(String::from_utf8_lossy(&out.stderr).contains("dispatching"));
}

#[test]
fn not_implemented_in_json_mode_is_one_document_on_stdout() {
    let out = ods(&["--json", "erd", "generate"]);
    assert_eq!(out.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert!(value["result"].is_null());
    assert_eq!(value["diagnostics"][0]["code"], "ODS-E0003");
    assert!(out.stderr.is_empty());
}

#[test]
fn completions_script_is_printed() {
    let out = ods(&["completions", "zsh"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("#compdef ods"));
}
