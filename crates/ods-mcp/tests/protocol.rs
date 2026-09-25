//! The MCP protocol surface, with fake tools, resources and prompts.

use ods_mcp::{
    PromptArgument, PromptDefinition, Prompts, ReadError, ResourceContent, ResourceDefinition,
    ResourceTemplate, Resources, Server, ServerInfo, Tool, ToolDefinition, ToolOutput,
};
use serde_json::{Map, Value, json};

struct Echo;

impl Tool for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::read_only(
            "echo",
            "Echo",
            "Returns its arguments, or fails on `fail`.",
            json!({"type": "object", "properties": {"fail": {"type": "boolean"}}}),
        )
    }

    fn call(&self, arguments: &Value) -> ToolOutput {
        assert!(arguments["panic"] != true, "a bug in the tool");
        if arguments["fail"] == true {
            ToolOutput::Error("asked to fail".into())
        } else {
            ToolOutput::Json(json!({ "echo": arguments }))
        }
    }
}

struct Docs;

impl Resources for Docs {
    fn list(&self) -> Vec<ResourceDefinition> {
        vec![ResourceDefinition::new("ods://a", "a", "A", "text/plain")]
    }
    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate::new(
            "ods://node/{id}",
            "node",
            "A node",
            "application/json",
        )]
    }
    fn read(&self, uri: &str) -> Result<ResourceContent, ReadError> {
        match uri {
            "ods://a" => Ok(ResourceContent::new("text/plain", "hello")),
            "ods://broken" => Err(ReadError::Failed("artifacts missing".into())),
            _ => Err(ReadError::NotFound),
        }
    }
}

struct Greet;

impl Prompts for Greet {
    fn list(&self) -> Vec<PromptDefinition> {
        vec![PromptDefinition::new(
            "greet",
            "Greet",
            "Says hello",
            vec![PromptArgument::new("name", "Who", true)],
        )]
    }
    fn get(&self, name: &str, arguments: &Map<String, Value>) -> Option<Result<String, String>> {
        (name == "greet").then(|| {
            arguments
                .get("name")
                .and_then(Value::as_str)
                .map(|n| format!("hello {n}"))
                .ok_or_else(|| "`name` is required".to_owned())
        })
    }
}

fn server() -> Server {
    Server::new(ServerInfo::new("ods", "0.0.0", "Use the tools."))
        .with_tool(Box::new(Echo))
        .with_resources(Box::new(Docs))
        .with_prompts(Box::new(Greet))
}

fn call(method: &str, params: &Value) -> Value {
    server()
        .handle(&json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}))
        .unwrap()
}

#[test]
fn initialize_negotiates_the_version_and_declares_capabilities() {
    let r = call(
        "initialize",
        &json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "ods");
    assert_eq!(r["result"]["instructions"], "Use the tools.");
    let caps = &r["result"]["capabilities"];
    assert!(
        caps["tools"].is_object() && caps["resources"].is_object() && caps["prompts"].is_object()
    );
    let unknown = call("initialize", &json!({"protocolVersion": "1999-01-01"}));
    assert_eq!(
        unknown["result"]["protocolVersion"],
        ods_mcp::PROTOCOL_VERSIONS[0]
    );
}

#[test]
fn tools_are_listed_read_only_and_called() {
    let list = call("tools/list", &json!({}));
    let tool = &list["result"]["tools"][0];
    assert_eq!(tool["name"], "echo");
    assert_eq!(tool["inputSchema"]["type"], "object");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);

    let ok = call(
        "tools/call",
        &json!({"name": "echo", "arguments": {"x": 1}}),
    );
    assert_eq!(ok["result"]["isError"], false);
    assert_eq!(ok["result"]["structuredContent"]["echo"]["x"], 1);
    let text: Value =
        serde_json::from_str(ok["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        text["echo"]["x"], 1,
        "text content mirrors the structured result"
    );

    let failed = call(
        "tools/call",
        &json!({"name": "echo", "arguments": {"fail": true}}),
    );
    assert_eq!(
        failed["result"]["isError"], true,
        "tool failures are results, not protocol errors"
    );
    assert_eq!(
        call("tools/call", &json!({"name": "nope"}))["error"]["code"],
        -32_602
    );
    assert_eq!(
        call("tools/call", &json!({"name": "echo", "arguments": [1]}))["error"]["code"],
        -32_602
    );
}

#[test]
fn resources_and_templates_are_listed_and_read() {
    assert_eq!(
        call("resources/list", &json!({}))["result"]["resources"][0]["mimeType"],
        "text/plain"
    );
    assert_eq!(
        call("resources/templates/list", &json!({}))["result"]["resourceTemplates"][0]["uriTemplate"],
        "ods://node/{id}"
    );
    let read = call("resources/read", &json!({"uri": "ods://a"}));
    assert_eq!(read["result"]["contents"][0]["text"], "hello");
    assert_eq!(
        call("resources/read", &json!({"uri": "ods://x"}))["error"]["code"],
        -32_002
    );
    assert!(
        call("resources/read", &json!({"uri": "ods://broken"}))["error"]["message"]
            .as_str()
            .unwrap()
            .contains("artifacts missing")
    );
}

#[test]
fn prompts_fill_their_arguments() {
    assert_eq!(
        call("prompts/list", &json!({}))["result"]["prompts"][0]["arguments"][0]["required"],
        true
    );
    let got = call(
        "prompts/get",
        &json!({"name": "greet", "arguments": {"name": "Ada"}}),
    );
    assert_eq!(got["result"]["messages"][0]["content"]["text"], "hello Ada");
    assert_eq!(
        call("prompts/get", &json!({"name": "greet"}))["error"]["code"],
        -32_602
    );
}

#[test]
fn notifications_get_no_reply_and_bad_input_gets_json_rpc_errors() {
    let s = server();
    assert!(
        s.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none()
    );
    assert_eq!(s.handle_line("{nope").unwrap()["error"]["code"], -32_700);
    assert_eq!(
        call("resources/subscribe", &json!({}))["error"]["code"],
        -32_601
    );
    assert_eq!(call("ping", &json!({}))["result"], json!({}));
    let batch = s
        .handle_line(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/x"}]"#)
        .unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 1);
}

#[test]
fn serve_answers_one_line_per_request() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n"
    );
    let mut output = Vec::new();
    server().serve(input.as_bytes(), &mut output).unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["id"], 2);
}

#[test]
fn a_panicking_tool_is_an_internal_error_not_a_crash() {
    let r = call(
        "tools/call",
        &json!({"name": "echo", "arguments": {"panic": true}}),
    );
    assert_eq!(r["error"]["code"], -32_603);
    assert_eq!(r["id"], 7);
    // The server still answers afterwards.
    let ok = call("tools/call", &json!({"name": "echo", "arguments": {}}));
    assert_eq!(ok["result"]["isError"], false);
}

#[test]
fn responses_are_ignored_and_bad_ids_rejected() {
    let s = server();
    assert!(
        s.handle(&json!({"jsonrpc": "2.0", "id": 3, "result": {}}))
            .is_none(),
        "a response is never answered"
    );
    let no_method = s.handle(&json!({"jsonrpc": "2.0", "id": 4})).unwrap();
    assert_eq!(no_method["error"]["code"], -32_600);
    assert_eq!(no_method["id"], 4);
    let bad_id = s
        .handle(&json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "ping"}))
        .unwrap();
    assert_eq!(bad_id["error"]["code"], -32_600);
    assert_eq!(bad_id["id"], Value::Null);
}

#[test]
fn a_failed_resource_read_is_an_internal_error() {
    assert_eq!(
        call("resources/read", &json!({"uri": "ods://broken"}))["error"]["code"],
        -32_603
    );
}

#[test]
fn invalid_utf8_is_one_parse_error_and_serving_continues() {
    let mut input = b"\xff\xfe\n".to_vec();
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
    input.push(b'\n');
    let mut output = Vec::new();
    server().serve(input.as_slice(), &mut output).unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["code"], -32_700);
    assert_eq!(lines[1]["id"], 2);
}
