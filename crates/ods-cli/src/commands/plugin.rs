//! `ods plugin list` and `ods plugin show` (ADR-0031 §3c): the plugins this `ods` runs
//! with and what each offers, as detected by asking it, never as it declares.

use clap::{Arg, ArgMatches, Command};
use serde::Serialize;

use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::plugins::{Detected, PluginKind, Plugins, Warehouses};
use crate::present::{Present, Span, Tone, ViewNode};

/// Lists the plugins this `ods` runs with.
pub struct PluginCommand;

impl Module for PluginCommand {
    fn command(&self) -> Command {
        Command::new("plugin")
            .about("List the plugins this ods runs with, and what each offers")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("Every plugin, one line each"))
            .subcommand(
                Command::new("show")
                    .about("One plugin: each thing it offers, and what a warehouse inherits")
                    .arg(
                        Arg::new("name")
                            .required(true)
                            .value_name("NAME")
                            .help("A health check's id, or a warehouse kind (e.g. `databricks`)"),
                    )
                    .arg(
                        Arg::new("kind")
                            .long("kind")
                            .value_name("KIND")
                            .value_parser(["warehouse", "health_check"])
                            .help("Which plugin, when a health check and a warehouse share NAME"),
                    ),
            )
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let plugins = crate::plugins::installed();
        let config = &ctx.config.config;
        match matches.subcommand() {
            Some(("list", _)) => ctx.emit(&PluginList {
                plugins: views(plugins, Some(config)),
            }),
            Some(("show", args)) => {
                let name = args
                    .get_one::<String>("name")
                    .map(String::as_str)
                    .unwrap_or_default();
                let kind = args.get_one::<String>("kind").map(String::as_str);
                let report = show(plugins, Some(config), name, kind)?;
                ctx.emit(&report)
            }
            _ => unreachable!("clap requires a known subcommand"),
        }
    }
}

/// One plugin and what it offers, as `ods plugin`, `ods_list_plugins` and the
/// dashboard show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PluginView {
    /// The check's id, or the warehouse kind.
    pub(crate) name: String,
    /// What kind of plugin it is.
    pub(crate) kind: PluginKind,
    /// Where it comes from: a crate and its version.
    pub(crate) from: String,
    /// Whether the released `ods` has it.
    pub(crate) builtin: bool,
    /// The warehouses a warehouse is built on, nearest first (ADR-0031 §3b).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) parents: Vec<String>,
    /// Who names them: the plugin, or `[warehouses.<kind>] extends`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parents_from: Option<&'static str>,
    /// What it offers itself.
    pub(crate) features: Vec<FeatureView>,
    /// What a warehouse takes from those it is built on: error patterns and the
    /// dialect, the capabilities that inherit (ADR-0031 §3b).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) inherited: Vec<Inherited>,
}

/// One thing a plugin offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct FeatureView {
    /// The feature, e.g. `source_versions`.
    pub(crate) name: &'static str,
    /// The SDK contract it implements, if any (a dialect implements none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) contract: Option<&'static str>,
    /// That contract's version, e.g. `0.3`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) contract_version: Option<String>,
    /// What a person needs to know: what it reads, its catalogue, its dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    /// Why it is offered but can't be used as configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable: Option<String>,
}

/// A capability a warehouse takes from one it is built on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct Inherited {
    /// `errors` or `dialect`.
    pub(crate) feature: &'static str,
    /// The warehouse it comes from.
    pub(crate) from: String,
    /// Its catalogue or dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Every plugin, as detected: health checks by id, then each warehouse. With
/// `config`, whether a feature can be used is judged with its settings and
/// `[warehouses.<kind>]`, as a run would use them (e.g. Databricks' links with its
/// configured `host`); without, as detection does, with none.
pub(crate) fn views(plugins: &Plugins, config: Option<&ods_config::Config>) -> Vec<PluginView> {
    plugins
        .detected()
        .into_iter()
        .map(|d| view(plugins, config, d))
        .collect()
}

fn view(plugins: &Plugins, config: Option<&ods_config::Config>, detected: Detected) -> PluginView {
    let none = Warehouses::new();
    let configured = config.map_or(&none, |c| &c.warehouses);
    let features = detected
        .features
        .iter()
        .map(|f| FeatureView {
            name: f.name,
            contract: f.contract.map(|c| c.name),
            contract_version: f.contract.map(|c| crate::version::dotted(c.version)),
            detail: f.detail.clone(),
            unavailable: match (config, f.name) {
                // Offered: whether it can be built is a matter of this configuration.
                (Some(config), "links") if detected.kind == PluginKind::Warehouse => plugins
                    .links(
                        Some(&detected.name),
                        &crate::plugins::WarehouseSettings::from_config(config, &detected.name),
                    )
                    .err()
                    .map(|why| why.to_string()),
                _ => f.unavailable.clone(),
            },
        })
        .collect();
    let (parents, parents_from, inherited) = if detected.kind == PluginKind::Warehouse {
        let parents = plugins.parents_of(&detected.name, configured);
        let from = (!parents.is_empty()).then_some(
            if configured
                .get(&detected.name)
                .is_some_and(|w| w.extends.is_some())
            {
                "configuration"
            } else {
                "plugin"
            },
        );
        (
            parents,
            from,
            inherited(plugins, configured, &detected.name),
        )
    } else {
        (Vec::new(), None, Vec::new())
    };
    PluginView {
        name: detected.name,
        kind: detected.kind,
        from: detected.from,
        builtin: detected.builtin,
        parents,
        parents_from,
        features,
        inherited,
    }
}

/// What `warehouse` takes from those it is built on: every parent's error patterns,
/// asked after its own, and a parent's dialect when it names none (ADR-0031 §3b).
fn inherited(plugins: &Plugins, configured: &Warehouses, warehouse: &str) -> Vec<Inherited> {
    let chain = plugins.chain(warehouse, configured);
    let mut out: Vec<Inherited> = chain
        .iter()
        .skip(1)
        .filter_map(|kind| {
            let info = plugins.warehouse(Some(kind))?.errors()?.catalogue();
            Some(Inherited {
                feature: "errors",
                from: kind.clone(),
                detail: Some(format!("{} catalogue {}", info.name, info.version)),
            })
        })
        .collect();
    // The dialect is the first a plugin on the chain names, else the first kind on it
    // the parser knows: inherited when that is a parent's.
    let named = chain.iter().find_map(|kind| {
        let dialect = plugins.warehouse(Some(kind))?.dialect()?;
        Some((kind.clone(), dialect.to_owned()))
    });
    let dialect = named.or_else(|| {
        chain
            .iter()
            .find(|kind| ods_provider_sqlparser::SqlDialect::from_name(kind).is_some())
            .map(|kind| (kind.clone(), kind.clone()))
    });
    if let Some((from, dialect)) = dialect.filter(|(from, _)| from != warehouse) {
        out.push(Inherited {
            feature: "dialect",
            from,
            detail: Some(dialect),
        });
    }
    out
}

fn show(
    plugins: &Plugins,
    config: Option<&ods_config::Config>,
    name: &str,
    kind: Option<&str>,
) -> Result<PluginShow, CliError> {
    let all = views(plugins, config);
    let names: Vec<String> = all.iter().map(|p| p.name.clone()).collect();
    let mut found: Vec<PluginView> = all
        .into_iter()
        .filter(|p| p.name == name && kind.is_none_or(|k| p.kind.name() == k))
        .collect();
    // A health check and a warehouse may share a name: say which.
    if found.len() > 1 {
        return Err(CliError::new(
            ExitStatus::Usage,
            codes::PLUGIN_UNKNOWN,
            format!("`{name}` names more than one plugin"),
        )
        .with_hint(format!(
            "say which with --kind: {}",
            found
                .iter()
                .map(|p| format!("`{}`", p.kind.name()))
                .collect::<Vec<_>>()
                .join(" or ")
        )));
    }
    found
        .pop()
        .map(|plugin| PluginShow { plugin })
        .ok_or_else(|| {
            CliError::new(
                ExitStatus::Usage,
                codes::PLUGIN_UNKNOWN,
                match kind {
                    Some(kind) => format!("no {kind} plugin `{name}`"),
                    None => format!("no plugin `{name}`"),
                },
            )
            .with_hint(if names.is_empty() {
                "this ods runs with no plugins".to_owned()
            } else {
                format!(
                    "the plugins are: {}; `ods plugin list` shows each",
                    names
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
        })
}

/// `ods plugin list`'s result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct PluginList {
    plugins: Vec<PluginView>,
}

impl Present for PluginList {
    const COMMAND: &'static str = "plugin.list";

    fn view(&self) -> ViewNode {
        if self.plugins.is_empty() {
            return ViewNode::Paragraph(vec![Span::plain("This ods runs with no plugins.")]);
        }
        ViewNode::Table {
            title: Some("Plugins".to_owned()),
            columns: vec![
                "plugin".into(),
                "kind".into(),
                "from".into(),
                "offers".into(),
            ],
            rows: self
                .plugins
                .iter()
                .map(|p| {
                    let mut offers: Vec<String> =
                        p.features.iter().map(|f| f.name.to_owned()).collect();
                    if !p.parents.is_empty() {
                        offers.push(format!("built on {}", p.parents.join(", ")));
                    }
                    vec![
                        vec![Span::toned(p.name.as_str(), Tone::Code)],
                        vec![Span::plain(p.kind.name())],
                        vec![Span::plain(if p.builtin {
                            format!("{}, built in", p.from)
                        } else {
                            p.from.clone()
                        })],
                        vec![Span::plain(offers.join(", "))],
                    ]
                })
                .collect(),
            breaks: Vec::new(),
            footer: None,
        }
    }
}

/// `ods plugin show`'s result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct PluginShow {
    plugin: PluginView,
}

impl Present for PluginShow {
    const COMMAND: &'static str = "plugin.show";

    fn view(&self) -> ViewNode {
        let p = &self.plugin;
        let mut about = vec![
            (
                "plugin".into(),
                vec![Span::toned(p.name.as_str(), Tone::Code)],
            ),
            ("kind".into(), vec![Span::plain(p.kind.name())]),
            ("from".into(), vec![Span::plain(p.from.as_str())]),
            (
                "built in".into(),
                vec![Span::plain(if p.builtin { "yes" } else { "no" })],
            ),
        ];
        if let Some(from) = p.parents_from {
            about.push((
                "built on".into(),
                vec![Span::plain(format!(
                    "{} (from the {from})",
                    p.parents.join(", ")
                ))],
            ));
        }
        let mut blocks = vec![ViewNode::KeyValue(about)];
        blocks.push(ViewNode::Table {
            title: Some("Offers".to_owned()),
            columns: vec!["feature".into(), "contract".into(), "detail".into()],
            rows: p
                .features
                .iter()
                .map(|f| {
                    let contract = match (f.contract, &f.contract_version) {
                        (Some(c), Some(v)) => format!("{c} {v}"),
                        _ => "-".to_owned(),
                    };
                    let detail = match (&f.detail, &f.unavailable) {
                        (_, Some(why)) => {
                            vec![Span::toned(format!("not usable: {why}"), Tone::Warning)]
                        }
                        (Some(d), None) => vec![Span::plain(d.as_str())],
                        (None, None) => vec![Span::plain("")],
                    };
                    vec![
                        vec![Span::toned(f.name, Tone::Code)],
                        vec![Span::plain(contract)],
                        detail,
                    ]
                })
                .collect(),
            breaks: Vec::new(),
            footer: None,
        });
        if !p.inherited.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("Inherited".to_owned()),
                columns: vec!["feature".into(), "from".into(), "detail".into()],
                rows: p
                    .inherited
                    .iter()
                    .map(|i| {
                        vec![
                            vec![Span::toned(i.feature, Tone::Code)],
                            vec![Span::plain(i.from.as_str())],
                            vec![Span::plain(i.detail.clone().unwrap_or_default())],
                        ]
                    })
                    .collect(),
                breaks: Vec::new(),
                footer: None,
            });
        }
        ViewNode::Group(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_released_ods_lists_databricks_and_what_it_offers() {
        let views = views(&Plugins::builtin(), None);
        let databricks = views.iter().find(|p| p.name == "databricks").unwrap();
        assert_eq!(databricks.kind, PluginKind::Warehouse);
        assert!(databricks.builtin);
        let features: Vec<&str> = databricks.features.iter().map(|f| f.name).collect();
        assert_eq!(
            features,
            [
                "source_versions",
                "login_check",
                "links",
                "observed_lineage",
                "errors",
                "dialect"
            ]
        );
        assert_eq!(databricks.parents, ["spark"]);
        assert_eq!(databricks.parents_from, Some("plugin"));
        // Spark has no plugin, so nothing is inherited from it but nothing breaks.
        assert!(
            databricks.inherited.iter().all(|i| i.feature != "errors"),
            "{:?}",
            databricks.inherited
        );
        let links = databricks
            .features
            .iter()
            .find(|f| f.name == "links")
            .unwrap();
        assert_eq!(links.contract, Some("relation_linker"));
        assert!(
            links.unavailable.is_some(),
            "no host configured: offered, not usable"
        );
    }

    #[test]
    fn a_warehouse_built_on_databricks_inherits_its_errors_and_dialect() {
        let mut configured = Warehouses::new();
        configured.insert(
            "acmebricks".to_owned(),
            toml::from_str("extends = [\"databricks\"]").unwrap(),
        );
        let plugins = Plugins::builtin();
        let inherited = inherited(&plugins, &configured, "acmebricks");
        assert_eq!(
            inherited
                .iter()
                .map(|i| (i.feature, i.from.as_str()))
                .collect::<Vec<_>>(),
            [("errors", "databricks"), ("dialect", "databricks")]
        );
        // A kind the parser knows keeps its own dialect.
        assert!(inherited_dialect(&plugins, "duckdb").is_none());
    }

    fn inherited_dialect(plugins: &Plugins, warehouse: &str) -> Option<Inherited> {
        inherited(plugins, &Warehouses::new(), warehouse)
            .into_iter()
            .find(|i| i.feature == "dialect")
    }

    use std::fmt::Write as _;

    /// `docs/plugins.md`'s table of the built-in warehouses, as detected (ADR-0031 §3c).
    fn builtin_warehouses_table() -> String {
        let cell = |p: &PluginView, name: &str| {
            p.features.iter().find(|f| f.name == name).map_or_else(
                || "—".to_owned(),
                |f| f.detail.clone().unwrap_or_else(|| "yes".to_owned()),
            )
        };
        let mut table = String::from(
            "| Warehouse | Source versions | Login check | Links | Observed lineage | Error patterns | Dialect | Built on |\n|---|---|---|---|---|---|---|---|\n",
        );
        for p in views(&Plugins::builtin(), None)
            .iter()
            .filter(|p| p.kind == PluginKind::Warehouse)
        {
            let row = [
                format!("`{}`", p.name),
                cell(p, "source_versions"),
                cell(p, "login_check"),
                cell(p, "links"),
                cell(p, "observed_lineage"),
                cell(p, "errors"),
                cell(p, "dialect"),
                if p.parents.is_empty() {
                    "—".to_owned()
                } else {
                    p.parents
                        .iter()
                        .map(|w| format!("`{w}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ];
            let _ = writeln!(table, "| {} |", row.join(" | "));
        }
        table
    }

    #[test]
    fn the_docs_table_of_built_in_warehouses_is_what_is_detected() {
        const START: &str =
            "<!-- built-in-warehouses:start (generated; see commands/plugin.rs) -->\n";
        const END: &str = "<!-- built-in-warehouses:end -->";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plugins.md");
        // A Windows checkout may give the page CRLF line endings.
        let docs = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let start = docs
            .find(START)
            .expect("docs/plugins.md has the table's start marker")
            + START.len();
        let end = docs[start..].find(END).expect("and its end marker") + start;
        let expected = builtin_warehouses_table();
        assert_eq!(
            &docs[start..end],
            expected,
            "docs/plugins.md's table of built-in warehouses differs from what is detected; replace it with:\n{expected}"
        );
    }

    #[test]
    fn a_health_check_and_a_warehouse_that_share_a_name_are_told_apart_by_kind() {
        let mut plugins = Plugins::builtin();
        plugins
            .add_health_check(
                crate::origin!(),
                std::sync::Arc::new(ods_provider_fake::FakeHealthCheck::new(
                    "databricks",
                    ods_sdk::contracts::health_check::Severity::Warn,
                )),
            )
            .unwrap();
        let error = show(&plugins, None, "databricks", None).unwrap_err();
        assert_eq!(error.status, ExitStatus::Usage);
        assert_eq!(error.message, "`databricks` names more than one plugin");
        assert!(
            error.hint.as_deref().is_some_and(|h| h.contains("--kind")),
            "{error:?}"
        );
        let warehouse = show(&plugins, None, "databricks", Some("warehouse")).unwrap();
        assert_eq!(warehouse.plugin.kind, PluginKind::Warehouse);
        let check = show(&plugins, None, "databricks", Some("health_check")).unwrap();
        assert_eq!(check.plugin.kind, PluginKind::HealthCheck);
        let none = show(
            &Plugins::builtin(),
            None,
            "databricks",
            Some("health_check"),
        )
        .unwrap_err();
        assert_eq!(none.message, "no health_check plugin `databricks`");
    }

    #[test]
    fn links_are_usable_when_the_configuration_has_what_they_need() {
        let config: ods_config::Config = toml::from_str(
            "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = \"https://dbc-0123.cloud.databricks.com/\"\n",
        )
        .unwrap();
        let links = |config: Option<&ods_config::Config>| {
            views(&Plugins::builtin(), config)
                .into_iter()
                .find(|p| p.name == "databricks")
                .unwrap()
                .features
                .into_iter()
                .find(|f| f.name == "links")
                .unwrap()
        };
        assert_eq!(links(Some(&config)).unavailable, None);
        // Without configuration, as detection sees it: offered, but no host.
        assert!(links(None).unavailable.is_some());
    }

    #[test]
    fn an_unknown_plugin_is_a_usage_error_naming_the_plugins() {
        let error = show(&Plugins::builtin(), None, "snowflake", None).unwrap_err();
        assert_eq!(error.status, ExitStatus::Usage);
        assert_eq!(error.code, codes::PLUGIN_UNKNOWN);
        assert_eq!(error.message, "no plugin `snowflake`");
        assert!(
            error
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("`databricks`")),
            "{error:?}"
        );
        let none = show(&Plugins::none(), None, "x", None).unwrap_err();
        assert_eq!(none.hint.as_deref(), Some("this ods runs with no plugins"));
    }
}
