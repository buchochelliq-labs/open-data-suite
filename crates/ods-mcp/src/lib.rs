//! A Model Context Protocol server for OpenDataSuite (ADR-0010, #169).
//!
//! This crate is the protocol: JSON-RPC 2.0 over newline-delimited stdio, the MCP
//! lifecycle (`initialize`, `ping`), and the `tools`, `resources` and `prompts`
//! methods. It knows nothing about dbt or warehouses: the binary registers [`Tool`]s,
//! [`Resources`] and [`Prompts`] backed by the ODS engines (ADR-0001).
//!
//! Every tool ODS registers is read-only and local. The server declares that through
//! each tool's annotations (`readOnlyHint`, `openWorldHint: false`), so clients can
//! auto-approve them.

mod protocol;

use serde::Serialize;
use serde_json::{Value, json};

pub use protocol::{PROTOCOL_VERSIONS, Server, ServerInfo};

/// What a tool is and how to call it, as listed by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolDefinition {
    /// Stable name, e.g. `ods_impact`.
    pub name: String,
    /// Short human title.
    pub title: String,
    /// What it answers and when to use it; this is what the model reads.
    pub description: String,
    /// JSON Schema of the arguments (an `object` schema).
    pub input_schema: Value,
    /// Behaviour hints for clients.
    pub annotations: Value,
}

impl ToolDefinition {
    /// A read-only, closed-world (no network), idempotent tool.
    pub fn read_only(
        name: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        let title = title.into();
        Self {
            name: name.into(),
            annotations: json!({
                "title": title,
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }),
            title,
            description: description.into(),
            input_schema,
        }
    }
}

/// What a tool call produced.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ToolOutput {
    /// A JSON object: sent as structured content, and as pretty JSON text for clients
    /// that only read text.
    Json(Value),
    /// Text (e.g. a Mermaid diagram) with the structured result it was rendered from.
    Text {
        /// What to show.
        text: String,
        /// The data behind it, if any.
        structured: Option<Value>,
    },
    /// The tool ran but the request can't be answered (unknown model, missing
    /// artifacts, …). Reported to the model as a tool error it can act on, not a
    /// protocol error.
    Error(String),
}

/// A tool.
pub trait Tool: Send + Sync {
    /// Its definition.
    fn definition(&self) -> ToolDefinition;

    /// Runs it with the client's arguments (validated by the tool itself).
    fn call(&self, arguments: &Value) -> ToolOutput;
}

/// A resource, as listed by `resources/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ResourceDefinition {
    /// Its URI, e.g. `ods://lineage/graph`.
    pub uri: String,
    /// Short name.
    pub name: String,
    /// What it is.
    pub description: String,
    /// Media type of its contents.
    pub mime_type: String,
}

impl ResourceDefinition {
    /// A resource.
    pub fn new(
        uri: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        mime_type: impl Into<String>,
    ) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            description: description.into(),
            mime_type: mime_type.into(),
        }
    }
}

/// A family of resources, as listed by `resources/templates/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ResourceTemplate {
    /// RFC 6570 template, e.g. `ods://node/{id}`.
    pub uri_template: String,
    /// Short name.
    pub name: String,
    /// What it is.
    pub description: String,
    /// Media type of its contents.
    pub mime_type: String,
}

impl ResourceTemplate {
    /// A template.
    pub fn new(
        uri_template: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        mime_type: impl Into<String>,
    ) -> Self {
        Self {
            uri_template: uri_template.into(),
            name: name.into(),
            description: description.into(),
            mime_type: mime_type.into(),
        }
    }
}

/// Resource contents: text with its media type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceContent {
    /// Media type.
    pub mime_type: String,
    /// The text.
    pub text: String,
}

impl ResourceContent {
    /// Contents.
    pub fn new(mime_type: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            mime_type: mime_type.into(),
            text: text.into(),
        }
    }
}

/// Why a resource can't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadError {
    /// No such resource.
    NotFound,
    /// It exists but couldn't be produced (e.g. artifacts are missing).
    Failed(String),
}

/// The server's resources.
pub trait Resources: Send + Sync {
    /// Fixed resources.
    fn list(&self) -> Vec<ResourceDefinition>;
    /// Parameterised resources.
    fn templates(&self) -> Vec<ResourceTemplate>;
    /// Reads one.
    ///
    /// # Errors
    /// [`ReadError`] if it doesn't exist or can't be produced.
    fn read(&self, uri: &str) -> Result<ResourceContent, ReadError>;
}

/// A prompt argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PromptArgument {
    /// Name.
    pub name: String,
    /// What to pass.
    pub description: String,
    /// Whether it must be given.
    pub required: bool,
}

impl PromptArgument {
    /// An argument.
    pub fn new(name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required,
        }
    }
}

/// A prompt template, as listed by `prompts/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PromptDefinition {
    /// Name.
    pub name: String,
    /// Short human title.
    pub title: String,
    /// What it's for.
    pub description: String,
    /// Its arguments.
    pub arguments: Vec<PromptArgument>,
}

impl PromptDefinition {
    /// A prompt.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        arguments: Vec<PromptArgument>,
    ) -> Self {
        Self {
            name: name.into(),
            title: title.into(),
            description: description.into(),
            arguments,
        }
    }
}

/// The server's prompts.
pub trait Prompts: Send + Sync {
    /// Every prompt.
    fn list(&self) -> Vec<PromptDefinition>;
    /// A prompt's user message with `arguments` filled in, or `None` if there is no
    /// such prompt.
    ///
    /// # Errors
    /// A message naming a missing required argument.
    fn get(
        &self,
        name: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Option<Result<String, String>>;
}
