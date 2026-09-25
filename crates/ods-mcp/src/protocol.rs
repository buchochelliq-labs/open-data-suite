//! JSON-RPC 2.0 over newline-delimited stdio, and the MCP methods ODS supports.

use std::io::{self, BufRead, Write};

use serde_json::{Map, Value, json};

use crate::{Prompts, ReadError, Resources, Tool, ToolOutput};

/// MCP protocol revisions this server speaks, newest first. A client asking for one
/// of these gets it; otherwise the newest is offered and the client decides.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32_700;
const INVALID_REQUEST: i64 = -32_600;
const METHOD_NOT_FOUND: i64 = -32_601;
const INVALID_PARAMS: i64 = -32_602;
/// MCP's code for an unknown resource.
const RESOURCE_NOT_FOUND: i64 = -32_002;
const INTERNAL_ERROR: i64 = -32_603;

/// Who the server is, sent on `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerInfo {
    /// Implementation name, e.g. `ods`.
    pub name: String,
    /// Implementation version.
    pub version: String,
    /// Guidance for the model on how to use the server.
    pub instructions: String,
}

impl ServerInfo {
    /// Server identity and instructions.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            instructions: instructions.into(),
        }
    }
}

/// An MCP server: a set of tools, resources and prompts.
pub struct Server {
    info: ServerInfo,
    tools: Vec<Box<dyn Tool>>,
    resources: Option<Box<dyn Resources>>,
    prompts: Option<Box<dyn Prompts>>,
}

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Server {
    /// A server with no tools yet.
    pub fn new(info: ServerInfo) -> Self {
        Self {
            info,
            tools: Vec::new(),
            resources: None,
            prompts: None,
        }
    }

    /// Adds a tool.
    #[must_use]
    pub fn with_tool(mut self, tool: Box<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Sets the resources.
    #[must_use]
    pub fn with_resources(mut self, resources: Box<dyn Resources>) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Sets the prompts.
    #[must_use]
    pub fn with_prompts(mut self, prompts: Box<dyn Prompts>) -> Self {
        self.prompts = Some(prompts);
        self
    }

    /// Reads one JSON-RPC message per line from `input` and writes each response as one
    /// line to `output`, until `input` ends. Blank lines are ignored.
    ///
    /// # Errors
    /// Returns an I/O error from reading or writing.
    pub fn serve(&self, input: impl BufRead, output: &mut dyn Write) -> io::Result<()> {
        for line in input.split(b'\n') {
            let line = line?;
            // Invalid UTF-8 is one bad message, not the end of the session.
            let response = match std::str::from_utf8(&line) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => self.handle_line(line),
                Err(_) => Some(error_response(
                    &Value::Null,
                    &RpcError::new(PARSE_ERROR, "not UTF-8"),
                )),
            };
            if let Some(response) = response {
                // Serializing a `Value` can't fail.
                let text = serde_json::to_string(&response).unwrap_or_default();
                writeln!(output, "{text}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handles one line of input: a request, notification, or batch. Returns what to
    /// send back, if anything (notifications get no response).
    pub fn handle_line(&self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(e) => {
                return Some(error_response(
                    &Value::Null,
                    &RpcError::new(PARSE_ERROR, format!("not JSON: {e}")),
                ));
            }
        };
        match message {
            // Batches were dropped from the protocol in 2025-06-18; older clients may
            // still send them.
            Value::Array(batch) if !batch.is_empty() => {
                let responses: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                (!responses.is_empty()).then_some(Value::Array(responses))
            }
            message => self.handle(&message),
        }
    }

    /// Handles one message.
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_response(
                &Value::Null,
                &RpcError::new(INVALID_REQUEST, "expected a JSON-RPC object"),
            ));
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // A response (to a request we never send) is not answered; anything else
            // without a method is an invalid request.
            if object.contains_key("result") || object.contains_key("error") {
                return None;
            }
            return Some(error_response(
                &id.unwrap_or(Value::Null),
                &RpcError::new(INVALID_REQUEST, "missing `method`"),
            ));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a response, even if they fail.
        let id = id?;
        if !(id.is_string() || id.is_number()) {
            return Some(error_response(
                &Value::Null,
                &RpcError::new(INVALID_REQUEST, "`id` must be a string or a number"),
            ));
        }
        let result = self.dispatch(method, &params);
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => error_response(&id, &error),
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({
                "tools": self.tools.iter().map(|t| t.definition()).collect::<Vec<_>>()
            })),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({
                "resources": self.resources.as_ref().map(|r| r.list()).unwrap_or_default()
            })),
            "resources/templates/list" => Ok(json!({
                "resourceTemplates":
                    self.resources.as_ref().map(|r| r.templates()).unwrap_or_default()
            })),
            "resources/read" => self.read_resource(params),
            "prompts/list" => Ok(json!({
                "prompts": self.prompts.as_ref().map(|p| p.list()).unwrap_or_default()
            })),
            "prompts/get" => self.get_prompt(params),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("method `{other}` is not supported"),
            )),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params["protocolVersion"].as_str().unwrap_or_default();
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == requested)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        let mut capabilities = Map::new();
        capabilities.insert("tools".into(), json!({ "listChanged": false }));
        if self.resources.is_some() {
            capabilities.insert(
                "resources".into(),
                json!({ "subscribe": false, "listChanged": false }),
            );
        }
        if self.prompts.is_some() {
            capabilities.insert("prompts".into(), json!({ "listChanged": false }));
        }
        json!({
            "protocolVersion": version,
            "capabilities": capabilities,
            "serverInfo": { "name": self.info.name, "version": self.info.version },
            "instructions": self.info.instructions,
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "`name` is required"))?;
        let tool = self
            .tools
            .iter()
            .find(|t| t.definition().name == name)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("no tool `{name}`")))?;
        let arguments = match &params["arguments"] {
            Value::Null => Value::Object(Map::new()),
            Value::Object(_) => params["arguments"].clone(),
            _ => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "`arguments` must be an object",
                ));
            }
        };
        // A bug in one tool must not take the whole server down with it.
        let output =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tool.call(&arguments)))
                .map_err(|_| {
                    RpcError::new(INTERNAL_ERROR, format!("tool `{name}` failed unexpectedly"))
                })?;
        Ok(match output {
            ToolOutput::Json(value) => {
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                let mut result = json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                });
                // Structured content must be an object.
                if value.is_object() {
                    result["structuredContent"] = value;
                }
                result
            }
            ToolOutput::Text { text, structured } => {
                let mut result = json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                });
                if let Some(value) = structured.filter(Value::is_object) {
                    result["structuredContent"] = value;
                }
                result
            }
            ToolOutput::Error(message) => json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        })
    }

    fn read_resource(&self, params: &Value) -> Result<Value, RpcError> {
        let uri = params["uri"]
            .as_str()
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "`uri` is required"))?;
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| RpcError::new(RESOURCE_NOT_FOUND, format!("no resource `{uri}`")))?;
        match resources.read(uri) {
            Ok(content) => Ok(json!({
                "contents": [{ "uri": uri, "mimeType": content.mime_type, "text": content.text }]
            })),
            Err(ReadError::NotFound) => Err(RpcError::new(
                RESOURCE_NOT_FOUND,
                format!("no resource `{uri}`"),
            )),
            Err(ReadError::Failed(message)) => Err(RpcError::new(INTERNAL_ERROR, message)),
        }
    }

    fn get_prompt(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "`name` is required"))?;
        let empty = Map::new();
        let arguments = params["arguments"].as_object().unwrap_or(&empty);
        let prompts = self
            .prompts
            .as_ref()
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("no prompt `{name}`")))?;
        let definition = prompts.list().into_iter().find(|p| p.name == name);
        match prompts.get(name, arguments) {
            Some(Ok(text)) => Ok(json!({
                "description": definition.map(|d| d.description).unwrap_or_default(),
                "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
            })),
            Some(Err(message)) => Err(RpcError::new(INVALID_PARAMS, message)),
            None => Err(RpcError::new(INVALID_PARAMS, format!("no prompt `{name}`"))),
        }
    }
}

fn error_response(id: &Value, error: &RpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": error.code, "message": error.message },
    })
}
