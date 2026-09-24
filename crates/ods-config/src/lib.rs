//! Layered, validated configuration and profiles for OpenDataSuite (ADR-0005).
//!
//! A foundation crate (ADR-0001): it depends on no other ODS crate and knows nothing
//! about specific providers. Provider and policy settings are carried as opaque tables
//! for the components that own them to validate.
//!
//! Precedence, lowest to highest: built-in defaults, user file, project file
//! (`ods.toml`), local file (`.ods/local.toml`), the active profile, `ODS__…`
//! environment variables, command-line flags.

mod error;
mod load;
mod model;
mod secret;
mod source;

pub use error::ConfigError;
pub use load::{
    ENV_PREFIX, FileStatus, FlagValue, Inputs, LOCAL_FILE, Loaded, PROFILE_ENV, PROJECT_FILE,
    Setting, load,
};
pub use model::{
    CONFIG_VERSION, ColorPreference, Config, LogConfig, LogLevel, OutputConfig, OutputFormat,
    PolicyConfig, ProjectConfig, ProviderConfig,
};
pub use secret::{SecretRef, is_secret_key};
pub use source::{FileKind, Source};
