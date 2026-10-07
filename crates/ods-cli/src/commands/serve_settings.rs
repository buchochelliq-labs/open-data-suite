//! The dashboard's Settings page (#351), in `ods-web`'s neutral terms: the effective
//! configuration as `ods config explain` lists it, shown as `ods doctor` shows it (no
//! secret resolved, no credential a value carries), what a run resolves it to, the
//! configured providers, and the doctor checks that only read.

use ods_config::{Loaded, Source, display_key};
use ods_web::settings::{
    FileFacts, ProfileFacts, ProviderFacts, Resolved, SettingEntry, SettingsInput,
};

use super::doctor_checks::{dbt_capabilities, offline_checks, shown_value};
use super::state_settings::{Setting, StateSettings};

/// The Settings page's facts for `config`, resolved as a run resolves it (`settings`).
pub(super) fn settings_input(config: &Loaded, settings: &StateSettings) -> SettingsInput {
    let mut input = SettingsInput::default();
    input.profile = config
        .profile
        .as_ref()
        .map(|(name, by)| ProfileFacts::new(name, by));
    input.files = config
        .files
        .iter()
        .map(|f| FileFacts::new(f.kind.name(), f.path.display().to_string(), f.loaded))
        .collect();
    input.entries = config
        .effective_settings()
        .map(|(key, setting)| {
            let shown = shown_value(key, &setting.value);
            let mut entry =
                SettingEntry::new(display_key(key), shown.text, setting.source.to_string());
            entry.secret = shown.secret;
            entry.source_short = short(&setting.source);
            entry.overrides = config.replaced(key).len();
            entry
        })
        .collect();
    let dbt = config
        .config
        .providers
        .iter()
        .find(|(_, p)| p.kind == "dbt")
        .map_or("<dbt>", |(name, _)| name.as_str());
    let key = |k: &str| format!("providers.{dbt}.settings.{k}");
    let row = |label: &str, key: String, setting: Option<&Setting>| {
        Resolved::new(
            label,
            key,
            setting.map(|s| s.value.clone()),
            setting.map(|s| s.origin.label()),
        )
    };
    input.project = vec![
        row(
            "Project dir",
            key("project_dir"),
            settings.project_dir.as_ref(),
        )
        .path(),
        row("Profile", key("profile"), settings.profile.as_ref()),
        row("Target", key("target"), settings.target.as_ref()),
        row(
            "Profiles dir",
            key("profiles_dir"),
            settings.profiles_dir.as_ref(),
        )
        .path(),
        row("Artifacts", key("target_dir"), Some(&settings.target_dir)).path(),
        row("dbt program", key("program"), Some(&settings.program)).path(),
    ];
    input.state = vec![
        row("Database", "state.db".to_owned(), Some(&settings.state_db)).path(),
        row(
            "Environment",
            "state.environment".to_owned(),
            Some(&settings.environment),
        ),
    ];
    input.providers = config
        .config
        .providers
        .iter()
        .map(|(name, p)| {
            let mut provider = ProviderFacts::new(name, &p.kind);
            if p.kind == "dbt" {
                provider.capabilities = dbt_capabilities(settings);
            }
            provider
        })
        .collect();
    input.checks = offline_checks(config, settings);
    input
}

/// Where a value came from, without a path: `project file`, `profile dev (user file)`.
fn short(source: &Source) -> String {
    match source {
        Source::File { kind, .. } => format!("{} file", kind.name()),
        Source::Profile { name, kind, .. } => format!("profile {name} ({} file)", kind.name()),
        other => other.to_string(),
    }
}
