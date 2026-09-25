//! `ods serve`: host the lineage explorer over HTTP (ADR-0009).
//!
//! The composition root for `ods-web`: it builds [`Snapshot`]s from dbt artifacts with
//! the same providers as `ods lineage`, and reloads them when the artifacts change.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_lineage::{GraphFilter, MemoryCache};
use ods_provider_dbt::ArtifactPreference;
use ods_web::{Loader, ServeOptions, Snapshot, WebError};
use serde::Serialize;

use super::lineage::{Loaded, Summary, common_args, preference};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods serve`.
pub struct Serve;

/// How often the artifacts are checked for changes.
const WATCH_EVERY: Duration = Duration::from_secs(1);

impl Module for Serve {
    fn command(&self) -> Command {
        common_args(
            Command::new("serve")
                .about("Host the lineage explorer and its read-only JSON API over HTTP")
                .arg(
                    Arg::new("host")
                        .long("host")
                        .value_name("ADDR")
                        .value_parser(clap::value_parser!(IpAddr))
                        .default_value("127.0.0.1")
                        .help("Address to listen on; anything but loopback exposes the API to your network"),
                )
                .arg(
                    Arg::new("port")
                        .long("port")
                        .value_name("PORT")
                        .value_parser(clap::value_parser!(u16))
                        .default_value("8765")
                        .help("Port to listen on; 0 picks a free one"),
                )
                .arg(
                    Arg::new("base-path")
                        .long("base-path")
                        .value_name("PATH")
                        .default_value("")
                        .help("URL prefix when behind a reverse proxy, e.g. /lineage"),
                )
                .arg(
                    Arg::new("allow-host")
                        .long("allow-host")
                        .value_name("NAME")
                        .action(ArgAction::Append)
                        .help("Also accept this Host name, e.g. the one a reverse proxy forwards; repeatable"),
                )
                .arg(
                    Arg::new("no-watch")
                        .long("no-watch")
                        .action(ArgAction::SetTrue)
                        .help("Don't reload when the dbt artifacts change"),
                ),
        )
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let target_dir = PathBuf::from(
            matches
                .get_one::<String>("target-dir")
                .map_or("target", String::as_str),
        );
        let dialect = matches.get_one::<String>("dialect").cloned();
        let preference = preference(matches);
        // Shared across reloads so only changed models are re-analyzed.
        let cache = Arc::new(MemoryCache::default());

        // Load once up front so a bad target directory is a normal CLI error, not a
        // server that never starts. The server then takes this snapshot as its first.
        let first = Loaded::from_dir(&target_dir, dialect.as_deref(), preference, &cache)?;
        let summary = Summary::of(&first);
        let initial = Mutex::new(Some(snapshot(&first, &target_dir)));
        drop(first);

        let loader = loader(target_dir.clone(), dialect, preference, cache, initial);
        let host = matches
            .get_one::<IpAddr>("host")
            .copied()
            .unwrap_or(IpAddr::from([127, 0, 0, 1]));
        let port = matches.get_one::<u16>("port").copied().unwrap_or(0);
        let base_path = matches
            .get_one::<String>("base-path")
            .map_or("", String::as_str);
        let mut options = ServeOptions::new(SocketAddr::new(host, port))
            .with_base_path(base_path)
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::SERVE, e.to_string()))?
            .with_allowed_hosts(
                matches
                    .get_many::<String>("allow-host")
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        let watching = !matches.get_flag("no-watch");
        if watching {
            options = options.with_watch(watched(&target_dir), WATCH_EVERY);
        }
        let base_path = options.base_path().to_owned();

        let mut announced = Ok(());
        let served = ods_web::serve_blocking(options, loader, |addr| {
            let report = ServeReport {
                summary,
                url: format!("http://{addr}{base_path}/"),
                exposed: !addr.ip().is_loopback(),
                watching,
            };
            announced = ctx.emit(&report).and_then(|()| {
                ctx.raw_out().flush().map_err(|e| {
                    CliError::new(ExitStatus::Failure, codes::OUTPUT_WRITE, e.to_string())
                })
            });
        });
        announced?;
        served.map_err(|e| match e {
            WebError::Load(message) => {
                CliError::new(ExitStatus::Failure, codes::LINEAGE_ARTIFACTS, message)
            }
            e @ WebError::BasePath(_) => {
                CliError::new(ExitStatus::Usage, codes::SERVE, e.to_string())
            }
            other => CliError::new(ExitStatus::Failure, codes::SERVE, other.to_string())
                .with_hint("pick another --port, or 0 for any free port"),
        })
    }
}

/// The server's view of a loaded project.
fn snapshot(loaded: &Loaded, target_dir: &Path) -> Snapshot {
    let document = loaded
        .graph
        .document(&|id| loaded.node_name(id), &GraphFilter::default());
    Snapshot::new(
        document,
        loaded.graph.clone(),
        target_dir.display().to_string(),
    )
}

/// Hands out `initial` first, then builds a fresh snapshot on every call; errors are
/// reported by the server as text.
fn loader(
    target_dir: PathBuf,
    dialect: Option<String>,
    preference: ArtifactPreference,
    cache: Arc<MemoryCache>,
    initial: Mutex<Option<Snapshot>>,
) -> Loader {
    Arc::new(move || {
        if let Some(first) = initial
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return Ok(first);
        }
        let loaded = Loaded::from_dir(&target_dir, dialect.as_deref(), preference, &cache)
            .map_err(|e| e.to_string())?;
        Ok(snapshot(&loaded, &target_dir))
    })
}

/// The files `dbt compile`/`docs generate` rewrite, in either artifact format.
fn watched(target_dir: &Path) -> Vec<PathBuf> {
    let info_schema = target_dir.join("info_schema").join("v1");
    vec![
        target_dir.join("manifest.json"),
        target_dir.join("catalog.json"),
        info_schema.join("dbt.models.parquet"),
        info_schema.join("dbt.node_columns.parquet"),
    ]
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ServeReport {
    summary: Summary,
    url: String,
    exposed: bool,
    watching: bool,
}

impl Present for ServeReport {
    const COMMAND: &'static str = "serve";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("Lineage explorer".into()),
            self.summary.view(),
            ViewNode::KeyValue(vec![
                (
                    "url".into(),
                    vec![Span::toned(self.url.as_str(), Tone::Code)],
                ),
                (
                    "reload".into(),
                    vec![Span::plain(if self.watching {
                        "when the dbt artifacts change"
                    } else {
                        "off"
                    })],
                ),
            ]),
        ];
        if self.exposed {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(
                    "listening beyond loopback: anyone who can reach this address can read \
                     the project's lineage. There is no authentication; put it behind a proxy \
                     that has some, and name it with --allow-host.",
                )],
            });
        }
        blocks.push(ViewNode::Notice {
            level: Level::Info,
            message: vec![Span::plain("press Ctrl-C to stop")],
        });
        ViewNode::Group(blocks)
    }
}
