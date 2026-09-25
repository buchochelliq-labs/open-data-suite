//! `ods mcp` end to end: a real stdio session against the fixtures.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Sends `requests` (ids 1..) after an initialize handshake and returns the responses
/// by id.
fn session(args: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ods"))
        .arg("mcp")
        .args(args)
        .env_clear()
        .env("XDG_CONFIG_HOME", std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = String::new();
    input.push_str(
        &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "test", "version": "1"}}})
        .to_string(),
    );
    input.push('\n');
    input.push_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    input.push('\n');
    for (i, request) in requests.iter().enumerate() {
        let mut message = request.clone();
        message["jsonrpc"] = json!("2.0");
        message["id"] = json!(i + 1);
        input.push_str(&message.to_string());
        input.push('\n');
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut responses: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdout carries only JSON-RPC"))
        .collect();
    responses.sort_by_key(|r| r["id"].as_u64());
    assert_eq!(responses.len(), requests.len() + 1);
    responses.remove(0);
    responses
}

fn tool(name: &str, arguments: &Value) -> Value {
    json!({"method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

fn structured(response: &Value) -> &Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    &response["result"]["structuredContent"]
}

fn text(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}

fn target() -> String {
    fixtures()
        .join("dbt/jaffle-ods/artifacts/dbt-1.10")
        .display()
        .to_string()
}

#[test]
fn every_tool_answers_from_the_fixture() {
    let target = target();
    let observed = fixtures()
        .join("databricks/uc-lineage/column_lineage.csv")
        .display()
        .to_string();
    let r = session(
        &["--target-dir", &target],
        &[
            json!({"method": "tools/list"}),
            tool("ods_project_summary", &json!({})),
            tool("ods_search", &json!({"query": "lifetime"})),
            tool("ods_get_node", &json!({"node": "customers"})),
            tool(
                "ods_lineage",
                &json!({"focus": ["customers.lifetime_value"], "upstream": 1}),
            ),
            tool(
                "ods_lineage",
                &json!({"focus": ["orders"], "format": "mermaid"}),
            ),
            tool(
                "ods_impact",
                &json!({"columns": ["stg_payments.payment_method"]}),
            ),
            tool("ods_erd", &json!({"select": ["orders"]})),
            tool("ods_erd", &json!({"format": "json", "infer": true})),
            tool("ods_test_gaps", &json!({})),
            tool("ods_list_opaque", &json!({})),
            tool("ods_compare_observed", &json!({"observed_file": observed})),
        ],
    );
    let tools = r[0]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13);
    assert!(
        tools
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true
                && t["annotations"]["openWorldHint"] == false
                && t["inputSchema"]["type"] == "object")
    );

    assert_eq!(structured(&r[1])["counts"]["data_tests"], 12);
    assert_eq!(
        structured(&r[2])["hits"][0]["label"],
        "customers.lifetime_value"
    );
    assert_eq!(
        structured(&r[3])["models"][0]["unique_id"],
        "model.jaffle_ods.customers"
    );
    assert!(structured(&r[4])["column_edges"].as_array().unwrap().len() > 1);
    assert!(text(&r[5]).starts_with("flowchart"), "{}", text(&r[5]));
    assert_eq!(structured(&r[6])["run"], json!(["model.jaffle_ods.orders"]));
    assert!(text(&r[7]).starts_with("erDiagram"));
    assert_eq!(structured(&r[7])["tested"], 2, "orders' relationships");
    assert!(structured(&r[8])["inferred"].as_u64().unwrap() > 0);
    let gaps = structured(&r[9])["suggestions"].as_array().unwrap();
    assert!(
        gaps.iter()
            .any(|g| g["model"] == "customer_order_rank" && g["test"] == "not_null")
    );
    assert!(
        gaps.iter()
            .all(|g| g["yaml"].as_str().unwrap().starts_with("- name: "))
    );
    assert_eq!(structured(&r[10])["opaque"], json!([]));
    assert_eq!(structured(&r[11])["comparison"]["missing"], 1);
}

#[test]
fn state_policies_come_from_dbt_state_configs() {
    let target = fixtures()
        .join("dbt/jaffle-ods-state/artifacts/dbt-1.10")
        .display()
        .to_string();
    let r = session(
        &["--target-dir", &target],
        &[tool(
            "ods_state_policies",
            &json!({"model": "customer_order_rank"}),
        )],
    );
    assert_eq!(structured(&r[0])["models"][0]["lag_tolerance"], "4h");
}

#[test]
fn bad_requests_are_tool_errors_and_arguments_are_never_flags() {
    let target = target();
    let escape = std::env::temp_dir().join(format!("ods-mcp-escape-{}", std::process::id()));
    let r = session(
        &["--target-dir", &target],
        &[
            tool(
                "ods_get_node",
                &json!({"node": format!("--output-file={}", escape.display())}),
            ),
            tool("ods_impact", &json!({"columns": ["orders.nope"]})),
            tool("ods_impact", &json!({})),
            tool("ods_lineage", &json!({})),
            tool("ods_erd", &json!({"select": ["nope"]})),
        ],
    );
    for response in &r {
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    assert!(text(&r[1]).contains("ODS-E0203"), "{}", text(&r[1]));
    assert!(!escape.exists(), "an argument must never become a flag");
}

#[test]
fn a_missing_target_is_reported_by_tools_not_fatal() {
    let r = session(
        &["--target-dir", "/nonexistent/ods"],
        &[
            json!({"method": "ping"}),
            tool("ods_project_summary", &json!({})),
        ],
    );
    assert_eq!(r[0]["result"], json!({}));
    assert_eq!(r[1]["result"]["isError"], true);
    assert!(text(&r[1]).contains("dbt compile"));
}

#[test]
fn resources_and_prompts() {
    let target = target();
    let r = session(
        &["--target-dir", &target],
        &[
            json!({"method": "resources/list"}),
            json!({"method": "resources/read", "params": {"uri": "ods://erd"}}),
            json!({"method": "resources/read", "params": {"uri": "ods://node/orders"}}),
            json!({"method": "resources/read", "params": {"uri": "ods://nope"}}),
            json!({"method": "prompts/get",
                   "params": {"name": "assess_change_impact",
                              "arguments": {"change": "drop orders.status"}}}),
            json!({"method": "prompts/get",
                   "params": {"name": "answer_data_question",
                              "arguments": {"question": "who are our best customers?"}}}),
        ],
    );
    assert_eq!(r[0]["result"]["resources"].as_array().unwrap().len(), 3);
    assert_eq!(
        r[5]["result"]["messages"][0]["content"]["text"]
            .as_str()
            .map(|t| t.contains("ods_plan_query")),
        Some(true)
    );
    let erd = r[1]["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(erd.starts_with("erDiagram"));
    let node: Value =
        serde_json::from_str(r[2]["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(node["models"][0]["name"], "orders");
    assert_eq!(r[3]["error"]["code"], -32_002);
    let prompt = r[4]["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("drop orders.status") && prompt.contains("ods_impact"));
}

#[test]
fn data_users_find_tables_by_meaning_and_get_sql() {
    let target = target();
    let r = session(
        &["--target-dir", &target],
        &[
            tool("ods_find_data", &json!({"query": "order statistics"})),
            tool("ods_describe_entity", &json!({"entity": "orders"})),
            tool(
                "ods_plan_query",
                &json!({"entities": ["customers", "orders"],
                        "columns": ["customers.first_name", "orders.amount"]}),
            ),
            tool(
                "ods_plan_query",
                &json!({"entities": ["orders"], "columns": ["orders.nope"]}),
            ),
            tool(
                "ods_plan_query",
                &json!({"entities": ["orders", "raw_customers"]}),
            ),
        ],
    );
    let found = structured(&r[0]);
    assert_eq!(
        found["results"][0]["entity"], "customers",
        "matched by its description: {found}"
    );
    assert_eq!(
        found["results"][0]["grain"]["columns"],
        json!(["customer_id"])
    );

    let orders = structured(&r[1]);
    assert_eq!(orders["grain"]["statement"], "one row per order_id");
    let references = orders["references"].as_array().unwrap();
    assert!(references.iter().any(|r| r["entity"] == "customers"
        && r["cardinality"] == "many-to-one"
        && r["basis"] == "tested"));
    assert!(
        references
            .iter()
            .any(|r| r["entity"] == "stg_customers" && r["basis"] == "joined in the project's SQL"),
        "joins the project makes are relationships too: {references:?}"
    );

    let plan = structured(&r[2]);
    let sql = plan["sql"].as_str().unwrap();
    assert!(
        sql.contains("from \"jaffle_ods\".\"main\".\"customers\" as c"),
        "{sql}"
    );
    assert!(sql.contains("on o.customer_id = c.customer_id"), "{sql}");
    assert!(
        sql.contains("c.first_name") && sql.contains("o.amount"),
        "{sql}"
    );
    assert_eq!(plan["joins"][0]["repeats_rows"], true);
    assert!(
        plan["warnings"][0].as_str().unwrap().contains("Aggregate"),
        "joining orders onto customers repeats customers: {plan}"
    );

    assert_eq!(r[3]["result"]["isError"], true);
    assert!(text(&r[3]).contains("no column `nope`"), "{}", text(&r[3]));
    assert_eq!(r[4]["result"]["isError"], true);
    assert!(
        text(&r[4]).contains("no known relationship"),
        "{}",
        text(&r[4])
    );
}
