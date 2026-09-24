//! Black-box tests for the `ods` binary.

use std::process::Command;

fn ods() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ods"))
}

#[test]
fn version_subcommand_succeeds() {
    let out = ods().arg("version").output().expect("failed to spawn ods");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ods "));
}

#[test]
fn unimplemented_module_exits_with_code_3() {
    let out = ods().arg("state").output().expect("failed to spawn ods");
    assert_eq!(out.status.code(), Some(3));
}
