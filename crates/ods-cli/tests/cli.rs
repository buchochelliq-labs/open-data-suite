//! Black-box tests for the `ods` binary.

use std::process::{Command, Output};

fn ods(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .env_remove("NO_COLOR")
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
