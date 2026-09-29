//! Which dbt is installed, from `dbt --version`, and whether ODS supports it (#181).
//!
//! dbt prints its version in a few shapes:
//!
//! ```text
//! Core:
//!   - installed: 1.10.2
//!   - latest:    1.10.2 - Up to date!
//!
//! Plugins:
//!   - databricks: 1.10.1 - Up to date!
//! ```
//!
//! and other builds print one line such as `dbt-fusion 2.0.0-preview.1`. Only the
//! major and minor numbers decide support. They are read by hand rather than with a
//! version crate because dbt's versions follow Python's PEP 440 (`1.9.0b1`,
//! `1.10.0rc2`), which semantic-version parsers reject, and two integers are all that
//! is needed.

use serde::Serialize;

/// The oldest dbt whose artifacts ODS reads: dbt 1.7 writes manifest v11.
pub const MIN_SUPPORTED: (u32, u32) = (1, 7);

/// The first major version ODS isn't tested with.
pub const UNTESTED_MAJOR: u32 = 2;

/// What `dbt --version` said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DbtVersion {
    /// The version as printed, e.g. `1.10.2`.
    pub raw: String,
    /// Its major number.
    pub major: u32,
    /// Its minor number.
    pub minor: u32,
    /// Installed adapter plugins and their versions, sorted by name.
    pub plugins: Vec<(String, String)>,
}

/// Whether ODS supports a dbt version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Support {
    /// Supported and tested.
    Supported,
    /// Older than [`MIN_SUPPORTED`]: its artifacts can't be read.
    TooOld,
    /// A major version ODS isn't tested with.
    Untested,
}

impl DbtVersion {
    /// Reads `dbt --version`'s output. `None` if no version can be found in it.
    pub fn parse(output: &str) -> Option<Self> {
        let raw = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("- installed:"))
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
            .or_else(|| {
                output
                    .split_whitespace()
                    .find(|word| numbers(word).is_some())
                    .map(str::to_owned)
            })?;
        let (major, minor) = numbers(&raw)?;
        Some(Self {
            raw,
            major,
            minor,
            plugins: plugins(output),
        })
    }

    /// Whether ODS supports this version.
    pub fn support(&self) -> Support {
        if (self.major, self.minor) < MIN_SUPPORTED {
            Support::TooOld
        } else if self.major >= UNTESTED_MAJOR {
            Support::Untested
        } else {
            Support::Supported
        }
    }

    /// Whether dbt listed a plugin named `adapter` (as the manifest's `adapter_type`
    /// names it). `None` when dbt listed no plugins at all, so nothing can be said.
    pub fn has_plugin(&self, adapter: &str) -> Option<bool> {
        if self.plugins.is_empty() {
            return None;
        }
        Some(self.plugins.iter().any(|(name, _)| name == adapter))
    }
}

/// `major.minor` at the start of `word` (after an optional `v`), if it has them.
fn numbers(word: &str) -> Option<(u32, u32)> {
    let word = word.strip_prefix('v').unwrap_or(word);
    let mut parts = word.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor.parse().ok()?))
}

/// The `- name: version …` lines under `Plugins:`, sorted by name.
fn plugins(output: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = output
        .lines()
        .skip_while(|line| line.trim() != "Plugins:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (name, rest) = line.trim().strip_prefix("- ")?.split_once(':')?;
            let version = rest.split_whitespace().next()?;
            Some((name.trim().to_owned(), version.to_owned()))
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dbt_core_output_with_plugins() {
        let out = "Core:\n  - installed: 1.10.2\n  - latest:    1.10.2 - Up to date!\n\nPlugins:\n  - postgres:   1.9.0 - Up to date!\n  - databricks: 1.10.1 - Up to date!\n";
        let v = DbtVersion::parse(out).unwrap();
        assert_eq!((v.raw.as_str(), v.major, v.minor), ("1.10.2", 1, 10));
        assert_eq!(
            v.plugins,
            [
                ("databricks".to_owned(), "1.10.1".to_owned()),
                ("postgres".to_owned(), "1.9.0".to_owned())
            ]
        );
        assert_eq!(v.support(), Support::Supported);
        assert_eq!(v.has_plugin("databricks"), Some(true));
        assert_eq!(v.has_plugin("duckdb"), Some(false));
    }

    #[test]
    fn reads_pre_releases_and_one_line_versions() {
        let v = DbtVersion::parse("Core:\n  - installed: 1.9.0b1\n").unwrap();
        assert_eq!((v.major, v.minor), (1, 9));
        assert_eq!(v.has_plugin("duckdb"), None);
        let v = DbtVersion::parse("dbt-fusion 2.0.0-preview.12\n").unwrap();
        assert_eq!((v.major, v.minor, v.support()), (2, 0, Support::Untested));
        assert!(DbtVersion::parse("command not found").is_none());
        assert!(DbtVersion::parse("").is_none());
    }

    #[test]
    fn support_follows_the_oldest_readable_manifest() {
        let at = |raw: &str| DbtVersion::parse(&format!("Core:\n  - installed: {raw}\n")).unwrap();
        assert_eq!(at("1.6.9").support(), Support::TooOld);
        assert_eq!(at("1.7.0").support(), Support::Supported);
        assert_eq!(at("1.11.3").support(), Support::Supported);
        assert_eq!(at("2.0.0").support(), Support::Untested);
    }
}
