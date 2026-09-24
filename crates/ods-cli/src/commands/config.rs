//! `ods config explain [KEY]`: effective configuration and where each value came from
//! (ADR-0005 §5).

use clap::{Arg, ArgMatches, Command};
use ods_config::{FileStatus, Loaded, SecretRef, Source};
use serde::Serialize;

use crate::exit::CliError;
use crate::module::{Context, Module};
use crate::present::{Level, Present, Span, Tone, TreeItem, ViewNode};

/// `ods config`.
pub struct Config;

impl Module for Config {
    fn command(&self) -> Command {
        Command::new("config")
            .about("Inspect configuration and profiles")
            .subcommand_required(true)
            .subcommand(
                Command::new("explain")
                    .about("Show effective configuration and where each value came from")
                    .arg(Arg::new("key").help(
                        "Only keys at or under this dotted path, e.g. output or providers.wh",
                    )),
            )
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        match matches.subcommand() {
            Some(("explain", args)) => {
                let filter = args.get_one::<String>("key").map(String::as_str);
                let explanation = Explanation::build(ctx.config, filter);
                ctx.emit(&explanation)
            }
            // `subcommand_required` makes clap reject anything else first.
            _ => Ok(()),
        }
    }
}

/// Result model for `ods config explain`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Explanation {
    /// Active profile, if any.
    profile: Option<Profile>,
    /// Configuration files considered, lowest precedence first.
    files: Vec<FileStatus>,
    /// The key filter, if one was given.
    filter: Option<String>,
    /// Effective keys, sorted.
    keys: Vec<Entry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Profile {
    name: String,
    selected_by: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Entry {
    key: String,
    /// The effective value. Secrets appear only as references (`{"secret": "env:X"}`).
    value: toml::Value,
    source: Source,
    /// Values this one replaced, highest precedence first.
    overridden: Vec<Overridden>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Overridden {
    value: toml::Value,
    source: Source,
}

impl Explanation {
    fn build(loaded: &Loaded, filter: Option<&str>) -> Self {
        let prefix: Vec<&str> = filter.map(|f| f.split('.').collect()).unwrap_or_default();
        let keys = loaded
            .history
            .iter()
            .filter(|(key, _)| {
                key.len() >= prefix.len() && key.iter().zip(&prefix).all(|(a, b)| a == b)
            })
            .filter_map(|(key, settings)| {
                let (effective, earlier) = settings.split_last()?;
                Some(Entry {
                    key: key.join("."),
                    value: effective.value.clone(),
                    source: effective.source.clone(),
                    overridden: earlier
                        .iter()
                        .rev()
                        .map(|s| Overridden {
                            value: s.value.clone(),
                            source: s.source.clone(),
                        })
                        .collect(),
                })
            })
            .collect();
        Self {
            profile: loaded.profile.as_ref().map(|(name, selected_by)| Profile {
                name: name.clone(),
                selected_by: selected_by.clone(),
            }),
            files: loaded.files.clone(),
            filter: filter.map(str::to_owned),
            keys,
        }
    }
}

/// Human spelling of a value; secret references are shown as references, never values.
fn display(value: &toml::Value) -> String {
    value
        .clone()
        .try_into::<SecretRef>()
        .map_or_else(|_| value.to_string(), |secret| secret.to_string())
}

impl Present for Explanation {
    const COMMAND: &'static str = "config.explain";

    fn view(&self) -> ViewNode {
        let profile = match &self.profile {
            Some(p) => vec![
                Span::toned(p.name.as_str(), Tone::Code),
                Span::plain(format!(" (from {})", p.selected_by)),
            ],
            None => vec![Span::toned("none", Tone::Muted)],
        };
        let mut summary = vec![("profile".to_owned(), profile)];
        for file in &self.files {
            let status = if file.loaded {
                Span::plain("")
            } else {
                Span::toned(" (not found)", Tone::Muted)
            };
            summary.push((
                format!("{} file", file.kind.name()),
                vec![
                    Span::toned(file.path.display().to_string(), Tone::Code),
                    status,
                ],
            ));
        }

        let mut blocks = vec![
            ViewNode::Heading("Configuration".into()),
            ViewNode::KeyValue(summary),
        ];
        if self.keys.is_empty() {
            let message = match &self.filter {
                Some(filter) => format!(
                    "no configuration value is set at or under `{filter}`; built-in defaults apply"
                ),
                None => "no configuration values are set; built-in defaults apply".to_owned(),
            };
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(message)],
            });
            return ViewNode::Group(blocks);
        }

        blocks.push(ViewNode::Table {
            title: None,
            columns: vec!["key".into(), "value".into(), "source".into()],
            rows: self
                .keys
                .iter()
                .map(|k| {
                    vec![
                        vec![Span::toned(k.key.as_str(), Tone::Code)],
                        vec![Span::plain(display(&k.value))],
                        vec![Span::plain(k.source.to_string())],
                    ]
                })
                .collect(),
        });
        let overridden: Vec<TreeItem> = self
            .keys
            .iter()
            .filter(|k| !k.overridden.is_empty())
            .map(|k| TreeItem {
                label: vec![
                    Span::toned(k.key.as_str(), Tone::Code),
                    Span::plain(format!(" = {}", display(&k.value))),
                ],
                children: k
                    .overridden
                    .iter()
                    .map(|o| {
                        TreeItem::leaf(vec![
                            Span::toned(format!("overrides {}", display(&o.value)), Tone::Muted),
                            Span::plain(format!(" from {}", o.source)),
                        ])
                    })
                    .collect(),
            })
            .collect();
        if !overridden.is_empty() {
            blocks.push(ViewNode::Tree(TreeItem {
                label: vec![Span::toned("overrides", Tone::Emphasis)],
                children: overridden,
            }));
        }
        ViewNode::Group(blocks)
    }
}
