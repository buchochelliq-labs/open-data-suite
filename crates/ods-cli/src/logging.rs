//! Logging to stderr via `tracing` (ADR-0004 §5).
//!
//! Logs never go to stdout, so they cannot corrupt command results or the JSON envelope.

use clap::{ArgAction, Args};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt as _;

/// Verbosity flags accepted by every command.
#[derive(Debug, Clone, Args)]
pub struct LogArgs {
    /// More log output on stderr (-v info, -vv debug, -vvv trace)
    #[arg(short = 'v', long, global = true, action = ArgAction::Count, conflicts_with = "quiet")]
    verbose: u8,

    /// Only log errors
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
}

impl LogArgs {
    /// Resolves the log level: `ODS_LOG` (`env`) beats `-v`/`-q`, which beat the
    /// configured `log.level` (`config`), which beats the `warn` default (ADR-0005 §2).
    ///
    /// # Errors
    /// Returns the offending value if `ODS_LOG` is not a known level.
    pub fn level(
        &self,
        env: Option<&str>,
        config: Option<LevelFilter>,
    ) -> Result<LevelFilter, String> {
        if let Some(value) = env.map(str::trim).filter(|v| !v.is_empty()) {
            return match value.to_ascii_lowercase().as_str() {
                "off" => Ok(LevelFilter::OFF),
                "error" => Ok(LevelFilter::ERROR),
                "warn" => Ok(LevelFilter::WARN),
                "info" => Ok(LevelFilter::INFO),
                "debug" => Ok(LevelFilter::DEBUG),
                "trace" => Ok(LevelFilter::TRACE),
                _ => Err(value.to_owned()),
            };
        }
        Ok(match (self.quiet, self.verbose) {
            (true, _) => LevelFilter::ERROR,
            (false, 0) => config.unwrap_or(LevelFilter::WARN),
            (false, 1) => LevelFilter::INFO,
            (false, 2) => LevelFilter::DEBUG,
            (false, _) => LevelFilter::TRACE,
        })
    }
}

/// Installs the global stderr subscriber. A second call (e.g. in tests) is a no-op.
///
/// Libraries' own logs (e.g. every SQL statement the state store runs) only show at
/// `trace`: at `debug`, ODS's own decisions should be readable.
pub fn init(level: LevelFilter, ansi: bool) {
    let libraries = if level == LevelFilter::TRACE {
        LevelFilter::TRACE
    } else {
        level.min(LevelFilter::WARN)
    };
    let targets = Targets::new()
        .with_default(libraries)
        .with_target("ods", level);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_ansi(ansi)
        .with_target(false)
        .without_time()
        .finish()
        .with(targets);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Converts a configured level to a filter.
pub fn from_config(level: ods_config::LogLevel) -> LevelFilter {
    use ods_config::LogLevel;
    match level {
        LogLevel::Off => LevelFilter::OFF,
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        log: LogArgs,
    }

    fn level(args: &[&str], env: Option<&str>) -> Result<LevelFilter, String> {
        let cli =
            TestCli::try_parse_from(std::iter::once("ods").chain(args.iter().copied())).unwrap();
        cli.log.level(env, None)
    }

    #[test]
    fn flags_map_to_levels() {
        assert_eq!(level(&[], None), Ok(LevelFilter::WARN));
        assert_eq!(level(&["-v"], None), Ok(LevelFilter::INFO));
        assert_eq!(level(&["-vv"], None), Ok(LevelFilter::DEBUG));
        assert_eq!(level(&["-vvvv"], None), Ok(LevelFilter::TRACE));
        assert_eq!(level(&["-q"], None), Ok(LevelFilter::ERROR));
    }

    #[test]
    fn ods_log_overrides_flags() {
        assert_eq!(level(&["-q"], Some("DEBUG")), Ok(LevelFilter::DEBUG));
        assert_eq!(
            level(&["-v"], Some("")),
            Ok(LevelFilter::INFO),
            "empty value is ignored"
        );
        assert_eq!(level(&[], Some("loud")), Err("loud".to_owned()));
    }

    #[test]
    fn configured_level_applies_only_without_flags_or_env() {
        let cli = |args: &[&str]| {
            TestCli::try_parse_from(std::iter::once("ods").chain(args.iter().copied())).unwrap()
        };
        let debug = Some(LevelFilter::DEBUG);
        assert_eq!(cli(&[]).log.level(None, debug), Ok(LevelFilter::DEBUG));
        assert_eq!(cli(&["-q"]).log.level(None, debug), Ok(LevelFilter::ERROR));
        assert_eq!(
            cli(&[]).log.level(Some("info"), debug),
            Ok(LevelFilter::INFO)
        );
    }

    #[test]
    fn verbose_and_quiet_conflict() {
        assert!(TestCli::try_parse_from(["ods", "-v", "-q"]).is_err());
    }
}
