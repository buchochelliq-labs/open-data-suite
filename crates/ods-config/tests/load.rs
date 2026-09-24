//! Loading and layering tests against real files in a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use ods_config::{ConfigError, FileKind, FlagValue, Inputs, OutputFormat, SecretRef, Source, load};

/// A fresh directory per test (no extra dev-dependency needed).
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("ods-config-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(dotted: &str) -> Vec<String> {
    dotted.split('.').map(str::to_owned).collect()
}

fn inputs(user: Option<PathBuf>, project: Option<PathBuf>, local: Option<PathBuf>) -> Inputs {
    Inputs {
        user_file: user,
        project_file: project,
        local_file: local,
        ..Inputs::default()
    }
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn precedence_is_user_project_local_profile_env_flag() {
    let dir = Dir::new();
    let user = dir.write(
        "home/ods/config.toml",
        "[output]\nwidth = 60\ncolor = \"never\"\n",
    );
    let project = dir.write(
        "proj/ods.toml",
        "default_profile = \"dev\"\n[output]\nwidth = 70\nformat = \"human\"\n\
         [profiles.dev.output]\nwidth = 90\n",
    );
    let local = dir.write("proj/.ods/local.toml", "[output]\nwidth = 80\n");
    let mut inputs = inputs(
        Some(user.clone()),
        Some(project.clone()),
        Some(local.clone()),
    );
    inputs.env = env(&[("ODS__OUTPUT__FORMAT", "plain")]);
    inputs.flags = vec![FlagValue {
        key: key("output.format"),
        value: toml::Value::String("json".into()),
        flag: "--json".into(),
    }];

    let loaded = load(&inputs).unwrap();
    assert_eq!(loaded.config.output.width, Some(90), "profile beats local");
    assert_eq!(
        loaded.config.output.format,
        Some(OutputFormat::Json),
        "flag beats env"
    );
    assert_eq!(
        loaded.profile,
        Some(("dev".into(), "default_profile".into()))
    );

    let width: Vec<i64> = loaded.history[&key("output.width")]
        .iter()
        .map(|s| s.value.as_integer().unwrap())
        .collect();
    assert_eq!(
        width,
        [60, 70, 80, 90],
        "history is lowest precedence first"
    );
    let format_sources: Vec<&Source> = loaded.history[&key("output.format")]
        .iter()
        .map(|s| &s.source)
        .collect();
    assert_eq!(
        format_sources,
        [
            &Source::File {
                kind: FileKind::Project,
                path: project
            },
            &Source::Env {
                var: "ODS__OUTPUT__FORMAT".into()
            },
            &Source::Flag {
                flag: "--json".into()
            },
        ]
    );
    assert_eq!(
        loaded.effective(&key("output.color")).unwrap().source,
        Source::File {
            kind: FileKind::User,
            path: user
        }
    );
}

#[test]
fn loading_is_deterministic_regardless_of_env_order() {
    let dir = Dir::new();
    let project = dir.write("ods.toml", "[project]\nname = \"p\"\n");
    let mut a = inputs(None, Some(project.clone()), None);
    a.env = env(&[
        ("ODS__OUTPUT__WIDTH", "80"),
        ("ODS__OUTPUT__COLOR", "never"),
    ]);
    let mut b = inputs(None, Some(project), None);
    b.env = env(&[
        ("ODS__OUTPUT__COLOR", "never"),
        ("ODS__OUTPUT__WIDTH", "80"),
    ]);
    let (a, b) = (load(&a).unwrap(), load(&b).unwrap());
    assert_eq!(a.config, b.config);
    assert_eq!(a.history, b.history);
    assert_eq!(
        a.config.output.width,
        Some(80),
        "env values are parsed as TOML values"
    );
}

#[test]
fn profile_selection_flag_beats_env_beats_default() {
    let dir = Dir::new();
    let project = dir.write(
        "ods.toml",
        "default_profile = \"dev\"\n[profiles.dev.project]\nname = \"d\"\n\
         [profiles.ci.project]\nname = \"c\"\n[profiles.prod.project]\nname = \"p\"\n",
    );
    let mut i = inputs(None, Some(project), None);
    assert_eq!(load(&i).unwrap().config.project.name.as_deref(), Some("d"));
    i.env = env(&[("ODS_PROFILE", "ci")]);
    assert_eq!(load(&i).unwrap().config.project.name.as_deref(), Some("c"));
    i.profile_flag = Some("prod".into());
    let loaded = load(&i).unwrap();
    assert_eq!(loaded.config.project.name.as_deref(), Some("p"));
    assert_eq!(loaded.profile, Some(("prod".into(), "--profile".into())));
}

#[test]
fn profiles_merge_across_files() {
    let dir = Dir::new();
    let user = dir.write(
        "user.toml",
        "[profiles.dev.providers.wh.settings]\ntoken = { secret = \"env:WH_TOKEN\" }\n",
    );
    let project = dir.write(
        "ods.toml",
        "[providers.wh]\nkind = \"example\"\n[profiles.dev.providers.wh.settings]\nhost = \"dev.example\"\n",
    );
    let mut i = inputs(Some(user), Some(project), None);
    i.profile_flag = Some("dev".into());
    let config = load(&i).unwrap().config;
    let settings = &config.providers["wh"].settings;
    assert_eq!(settings["host"].as_str(), Some("dev.example"));
    let token: SecretRef = settings["token"].clone().try_into().unwrap();
    assert_eq!(token.to_string(), "secret(env:WH_TOKEN)");
}

#[test]
fn unknown_profile_lists_defined_ones() {
    let dir = Dir::new();
    let project = dir.write("ods.toml", "[profiles.dev]\n[profiles.ci]\n");
    let mut i = inputs(None, Some(project), None);
    i.env = env(&[("ODS_PROFILE", "prod")]);
    let err = load(&i).unwrap_err();
    assert_eq!(err.code(), "ODS-E0104");
    assert_eq!(
        err.to_string(),
        "profile `prod` (selected by ODS_PROFILE) is not defined; defined profiles: ci, dev"
    );
}

#[test]
fn unknown_keys_and_wrong_types_name_the_key_and_source() {
    let dir = Dir::new();
    let project = dir.write("ods.toml", "[output]\nfromat = \"json\"\n");
    let err = load(&inputs(None, Some(project.clone()), None)).unwrap_err();
    assert_eq!(err.code(), "ODS-E0102");
    let text = err.to_string();
    assert!(
        text.contains("`output`") || text.contains("output.fromat"),
        "{text}"
    );
    assert!(text.contains("unknown field `fromat`"), "{text}");
    assert!(text.contains(&project.display().to_string()), "{text}");

    let mut i = inputs(None, None, None);
    i.env = env(&[("ODS__OUTPUT__WIDTH", "wide")]);
    let text = load(&i).unwrap_err().to_string();
    assert!(
        text.contains("output.width") && text.contains("ODS__OUTPUT__WIDTH"),
        "{text}"
    );
}

#[test]
fn value_rules_are_enforced() {
    let dir = Dir::new();
    let v2 = dir.write("a/ods.toml", "version = 2\n");
    assert!(
        load(&inputs(None, Some(v2), None))
            .unwrap_err()
            .to_string()
            .contains("version 2")
    );
    let narrow = dir.write("b/ods.toml", "[output]\nwidth = 10\n");
    assert!(
        load(&inputs(None, Some(narrow), None))
            .unwrap_err()
            .to_string()
            .contains("minimum of 20")
    );
    let bad_format = dir.write("c/ods.toml", "[output]\nformat = \"yaml\"\n");
    assert_eq!(
        load(&inputs(None, Some(bad_format), None))
            .unwrap_err()
            .code(),
        "ODS-E0102"
    );
}

#[test]
fn plaintext_credentials_are_rejected_wherever_they_come_from() {
    let dir = Dir::new();
    let project = dir.write(
        "ods.toml",
        "[providers.wh]\nkind = \"x\"\nsettings = { access_token = \"abc123\" }\n",
    );
    let err = load(&inputs(None, Some(project), None)).unwrap_err();
    assert_eq!(err.code(), "ODS-E0103");
    assert!(
        err.to_string()
            .contains("providers.wh.settings.access_token"),
        "{err}"
    );
    assert!(
        !err.to_string().contains("abc123"),
        "the secret itself must never be echoed"
    );

    let mut i = inputs(None, None, None);
    i.env = env(&[
        ("ODS__PROVIDERS__WH__KIND", "x"),
        ("ODS__PROVIDERS__WH__SETTINGS__PASSWORD", "hunter2"),
    ]);
    let err = load(&i).unwrap_err();
    assert_eq!(err.code(), "ODS-E0103");
    assert!(!err.to_string().contains("hunter2"));
}

#[test]
fn effective_config_never_contains_secret_values() {
    let dir = Dir::new();
    let project = dir.write(
        "ods.toml",
        "[providers.wh]\nkind = \"x\"\nsettings = { token = { secret = \"env:T\" }, host = \"h\" }\n",
    );
    let loaded = load(&inputs(None, Some(project), None)).unwrap();
    let json = serde_json::to_string(&loaded.config).unwrap();
    assert!(json.contains(r#""token":{"secret":"env:T"}"#), "{json}");
}

#[test]
fn missing_files_are_reported_but_not_errors() {
    let dir = Dir::new();
    let loaded = load(&inputs(
        Some(dir.0.join("nope.toml")),
        None,
        Some(dir.0.join("x.toml")),
    ))
    .unwrap();
    assert!(loaded.files.iter().all(|f| !f.loaded));
    assert_eq!(loaded.files.len(), 2);
    assert_eq!(loaded.config, ods_config::Config::default());
}

#[test]
fn malformed_toml_names_the_file() {
    let dir = Dir::new();
    let project = dir.write("ods.toml", "[output\n");
    let err = load(&inputs(None, Some(project.clone()), None)).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }));
    assert_eq!(err.code(), "ODS-E0101");
    assert!(err.to_string().contains(&project.display().to_string()));
}

#[test]
fn discovery_finds_the_nearest_project_file_and_user_dir() {
    let dir = Dir::new();
    let project = dir.write("repo/ods.toml", "");
    let nested = dir.0.join("repo/models/staging");
    fs::create_dir_all(&nested).unwrap();
    let found = Inputs::discover(
        &nested,
        &env(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/home/u")]),
    );
    assert_eq!(found.project_file.as_deref(), Some(project.as_path()));
    assert_eq!(found.local_file, Some(dir.0.join("repo/.ods/local.toml")));
    assert_eq!(
        found.user_file,
        Some(Path::new("/cfg/ods/config.toml").to_path_buf())
    );

    let home_only = Inputs::discover(&nested, &env(&[("HOME", "/home/u")]));
    assert_eq!(
        home_only.user_file,
        Some(Path::new("/home/u/.config/ods/config.toml").to_path_buf())
    );
}
