//! Global output flags and how they resolve to concrete settings (ADR-0003 §2).

use clap::{Args, ValueEnum};
use ods_config::{ColorPreference, FlagValue, OutputConfig, OutputFormat};

/// How results are written to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Styled terminal output.
    Human,
    /// Stable, uncoloured text for pipes and logs.
    Plain,
    /// Versioned JSON envelope.
    Json,
}

/// Whether to emit ANSI colour in human mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ColorChoice {
    /// Colour when writing to a terminal, honouring `NO_COLOR` and `TERM=dumb`.
    #[default]
    Auto,
    /// Always colour, even when redirected.
    Always,
    /// Never colour.
    Never,
}

/// Output flags accepted by every command.
#[derive(Debug, Clone, Args)]
pub struct OutputArgs {
    /// Output format [default: human on a terminal, plain otherwise]
    #[arg(long, short = 'o', global = true, value_enum)]
    output: Option<Mode>,

    /// Shorthand for `--output json`
    #[arg(long, global = true, conflicts_with = "output")]
    json: bool,

    /// When to use colour in human output [default: auto]
    #[arg(long, global = true, value_enum)]
    color: Option<ColorChoice>,

    /// Render width in columns [default: terminal width, or 100 when not a terminal]
    #[arg(long, global = true, value_parser = clap::value_parser!(u16).range(20..))]
    width: Option<u16>,
}

/// Whether `TERM` names a terminal that cannot render styles (`dumb` or `unknown`).
///
/// rs-rich 0.0.7 applies the same rule to its own colour detection. ODS checks it too, so
/// the ADR-0003 §2 contract holds independently of upstream and also covers log colour.
pub fn term_is_dumb() -> bool {
    std::env::var_os("TERM").is_some_and(|term| {
        term.eq_ignore_ascii_case("dumb") || term.eq_ignore_ascii_case("unknown")
    })
}

/// Width used for human output when stdout is not a terminal.
const NON_TERMINAL_WIDTH: usize = 100;

/// Fully resolved output settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSettings {
    /// Selected format.
    pub mode: Mode,
    /// Colour preference for human output.
    pub color: ColorChoice,
    /// Fixed render width, or `None` to detect the terminal width.
    pub width: Option<usize>,
}

impl OutputArgs {
    /// The explicitly given flags as configuration values, the highest layer (ADR-0005).
    pub fn flag_values(&self) -> Vec<FlagValue> {
        let flag = |key: &str, value: toml::Value, flag: &str| FlagValue {
            key: vec!["output".to_owned(), key.to_owned()],
            value,
            flag: flag.to_owned(),
        };
        let mut values = Vec::new();
        if self.json {
            values.push(flag("format", "json".into(), "--json"));
        } else if let Some(mode) = self.output {
            values.push(flag("format", mode.name().into(), "--output"));
        }
        if let Some(color) = self.color {
            values.push(flag("color", color.name().into(), "--color"));
        }
        if let Some(width) = self.width {
            values.push(flag("width", i64::from(width).into(), "--width"));
        }
        values
    }

    /// Resolves the flags alone, for reporting errors raised before configuration loads.
    pub fn resolve(&self, stdout_is_terminal: bool) -> OutputSettings {
        let mut config = OutputConfig::default();
        config.format = if self.json {
            Some(OutputFormat::Json)
        } else {
            self.output.map(Mode::to_config)
        };
        config.color = self.color.map(ColorChoice::to_config);
        config.width = self.width;
        OutputSettings::resolve(&config, stdout_is_terminal)
    }
}

impl OutputSettings {
    /// Resolves effective output configuration (flags already merged in) against
    /// whether stdout is a terminal (ADR-0003 §2).
    pub fn resolve(config: &OutputConfig, stdout_is_terminal: bool) -> Self {
        let mode = match config.format {
            Some(format) => Mode::from_config(format),
            None if stdout_is_terminal => Mode::Human,
            None => Mode::Plain,
        };
        let width = config
            .width
            .map(usize::from)
            .or((!stdout_is_terminal).then_some(NON_TERMINAL_WIDTH));
        OutputSettings {
            mode,
            color: config
                .color
                .map_or(ColorChoice::Auto, ColorChoice::from_config),
            width,
        }
    }
}

impl Mode {
    /// The flag/config spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Human => "human",
            Mode::Plain => "plain",
            Mode::Json => "json",
        }
    }

    const fn to_config(self) -> OutputFormat {
        match self {
            Mode::Human => OutputFormat::Human,
            Mode::Plain => OutputFormat::Plain,
            Mode::Json => OutputFormat::Json,
        }
    }

    const fn from_config(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Human => Mode::Human,
            OutputFormat::Plain => Mode::Plain,
            OutputFormat::Json => Mode::Json,
        }
    }
}

impl ColorChoice {
    /// The flag/config spelling.
    pub const fn name(self) -> &'static str {
        match self {
            ColorChoice::Auto => "auto",
            ColorChoice::Always => "always",
            ColorChoice::Never => "never",
        }
    }

    const fn to_config(self) -> ColorPreference {
        match self {
            ColorChoice::Auto => ColorPreference::Auto,
            ColorChoice::Always => ColorPreference::Always,
            ColorChoice::Never => ColorPreference::Never,
        }
    }

    const fn from_config(color: ColorPreference) -> Self {
        match color {
            ColorPreference::Auto => ColorChoice::Auto,
            ColorPreference::Always => ColorChoice::Always,
            ColorPreference::Never => ColorChoice::Never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        output: OutputArgs,
    }

    fn resolve(args: &[&str], tty: bool) -> OutputSettings {
        let cli =
            TestCli::try_parse_from(std::iter::once("ods").chain(args.iter().copied())).unwrap();
        cli.output.resolve(tty)
    }

    #[test]
    fn defaults_follow_the_terminal() {
        assert_eq!(resolve(&[], true).mode, Mode::Human);
        assert_eq!(resolve(&[], true).width, None);
        assert_eq!(resolve(&[], false).mode, Mode::Plain);
        assert_eq!(resolve(&[], false).width, Some(100));
    }

    #[test]
    fn explicit_flags_win() {
        assert_eq!(resolve(&["--json"], true).mode, Mode::Json);
        assert_eq!(resolve(&["-o", "human"], false).mode, Mode::Human);
        assert_eq!(resolve(&["--width", "60"], true).width, Some(60));
    }

    #[test]
    fn json_conflicts_with_output() {
        assert!(TestCli::try_parse_from(["ods", "--json", "-o", "plain"]).is_err());
    }
}
