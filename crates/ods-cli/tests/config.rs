//! Configuration through the real `ods` binary: discovery, precedence, errors, explain.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("ods-cli-config-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("home")).unwrap();
        Self(path)
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

/// Replaces the temp directory with `[dir]` and uses `/` separators, so snapshots are
/// the same on every OS. The binary may report the canonical path (macOS resolves
/// `/var` to `/private/var`), so that form is replaced first.
fn redact_dir(text: &str, dir: &Dir) -> String {
    let mut text = text.to_owned();
    if let Ok(canonical) = dir.0.canonicalize() {
        text = text.replace(&canonical.display().to_string(), "[dir]");
    }
    text.replace(&dir.0.display().to_string(), "[dir]")
        .replace('\\', "/")
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs `ods` in `cwd` with a clean, isolated environment.
fn ods(dir: &Dir, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ods"));
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("XDG_CONFIG_HOME", dir.0.join("home"));
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("failed to spawn ods")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn project_config_sets_defaults_that_flags_override() {
    let dir = Dir::new();
    dir.write("proj/ods.toml", "[output]\nformat = \"json\"\n");
    fs::create_dir_all(dir.0.join("proj/models")).unwrap();
    let cwd = dir.0.join("proj/models");

    let out = ods(&dir, &cwd, &["version"], &[]);
    assert!(out.status.success());
    assert!(
        stdout(&out).trim_start().starts_with('{'),
        "config selects JSON: {}",
        stdout(&out)
    );

    let out = ods(&dir, &cwd, &["version", "-o", "plain"], &[]);
    assert!(stdout(&out).starts_with("ods: "), "flag beats config");

    let out = ods(
        &dir,
        &cwd,
        &["version"],
        &[("ODS__OUTPUT__FORMAT", "plain")],
    );
    assert!(
        stdout(&out).starts_with("ods: "),
        "environment beats project file"
    );
}

#[test]
fn invalid_config_exits_4_in_the_active_output_mode() {
    let dir = Dir::new();
    dir.write("ods.toml", "[output]\nfromat = \"json\"\n");
    let out = ods(&dir, &dir.0, &["version"], &[]);
    assert_eq!(out.status.code(), Some(4));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[ODS-E0102]") && stderr.contains("fromat"),
        "{stderr}"
    );

    let out = ods(&dir, &dir.0, &["--json", "version"], &[]);
    assert_eq!(out.status.code(), Some(4));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(value["diagnostics"][0]["code"], "ODS-E0102");
}

#[test]
fn profiles_are_selected_by_flag_or_environment() {
    let dir = Dir::new();
    dir.write(
        "ods.toml",
        "[profiles.ci.output]\nformat = \"json\"\n[profiles.local.output]\nformat = \"plain\"\n",
    );
    let out = ods(&dir, &dir.0, &["version"], &[("ODS_PROFILE", "ci")]);
    assert!(stdout(&out).trim_start().starts_with('{'));
    let out = ods(
        &dir,
        &dir.0,
        &["--profile", "local", "version"],
        &[("ODS_PROFILE", "ci")],
    );
    assert!(
        stdout(&out).starts_with("ods: "),
        "--profile beats ODS_PROFILE"
    );
    let out = ods(&dir, &dir.0, &["version", "--profile", "nope"], &[]);
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ODS-E0104"));
}

#[test]
fn explain_shows_values_sources_and_overrides() {
    let dir = Dir::new();
    dir.write("home/ods/config.toml", "[output]\ncolor = \"never\"\n");
    dir.write(
        "proj/ods.toml",
        "default_profile = \"dev\"\n[output]\nwidth = 100\n\
         [providers.warehouse]\nkind = \"example\"\n\
         settings = { host = \"prod.example\", token = { secret = \"env:WH_TOKEN\" } }\n\
         [profiles.dev.providers.warehouse.settings]\nhost = \"dev.example\"\n",
    );
    dir.write("proj/.ods/local.toml", "[output]\nwidth = 120\n");
    let cwd = dir.0.join("proj");
    let out = ods(
        &dir,
        &cwd,
        &["config", "explain", "-o", "plain"],
        &[("WH_TOKEN", "s3cret-value")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = redact_dir(&stdout(&out), &dir);
    assert!(
        !text.contains("s3cret-value"),
        "secret values must never be shown"
    );
    insta::assert_snapshot!(text);
}

#[test]
fn explain_filters_by_key_prefix() {
    let dir = Dir::new();
    dir.write(
        "ods.toml",
        "[output]\nwidth = 90\n[project]\nname = \"p\"\n",
    );
    let out = ods(
        &dir,
        &dir.0,
        &["config", "explain", "output", "--json"],
        &[],
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let keys: Vec<&str> = value["result"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["output.format", "output.width"],
        "the --json flag is a layer too"
    );

    let out = ods(
        &dir,
        &dir.0,
        &["config", "explain", "log", "-o", "plain"],
        &[],
    );
    assert!(stdout(&out).contains("no configuration value is set at or under `log`"));
}

#[test]
fn explain_reports_a_table_replaced_by_a_scalar() {
    let dir = Dir::new();
    dir.write("ods.toml", "[policy.rules]\nmax = { warn = 1 }\n");
    dir.write(".ods/local.toml", "[policy.rules]\nmax = 5\n");
    let out = ods(
        &dir,
        &dir.0,
        &["config", "explain", "policy", "-o", "plain"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("policy.rules.max\t5\t"), "{text}");
    assert!(
        !text.contains("policy.rules.max.warn\t"),
        "replaced key is not listed as effective: {text}"
    );
    assert!(
        text.contains("replaces policy.rules.max.warn = 1"),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_ods_variables_are_config_errors() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = Dir::new();
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .arg("version")
        .current_dir(&dir.0)
        .env_clear()
        .env("ODS_PROFILE", OsStr::from_bytes(b"dev\xff"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ODS_PROFILE is not valid UTF-8"));
}
