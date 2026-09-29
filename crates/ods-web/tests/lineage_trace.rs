//! The explorer's column traces (`assets/lineage.js`), run under Node when it is
//! installed: at an opaque node the trail can't be followed, and must say so rather
//! than stop, so nothing past it reads as unaffected (AGENTS rule 3, #312).

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Loads the script with no page around it, then runs `call` on its tracer over the
/// document on stdin, printing the JSON it returns.
const HARNESS: &str = r#"
const fs = require("fs");
const src = fs.readFileSync(process.argv[1], "utf8");
const OdsLineage = new Function(src + "\nreturn OdsLineage;")();
const input = JSON.parse(fs.readFileSync(0, "utf8"));
const t = OdsLineage.tracer(input.doc);
const r = t.traceColumn(input.node, input.column, true);
const out = {};
for (const [k, v] of Object.entries(r)) out[k] = [...v].map(x => typeof x === "string" ? x.replace("\u0000", ".") : x).sort();
process.stdout.write(JSON.stringify(out));
"#;

fn node() -> Option<String> {
    let found = Command::new("node").arg("--version").output().ok()?;
    found.status.success().then(|| "node".to_owned())
}

fn col(node: &str, column: &str) -> Value {
    json!({ "node": node, "column": column })
}

fn edge(from: &Value, to: &Value) -> Value {
    json!({ "from": from, "to": to, "kind": { "type": "direct", "subtype": "identity" } })
}

fn model(id: &str, columns: &[&str], opaque: bool) -> Value {
    json!({ "id": id, "name": id, "kind": "model", "relation": id, "columns": columns,
            "confidence": null, "opaque": opaque, "diagnostics": [], "layer": 0 })
}

/// `raw.id → orders.id → customers.id`; `segments` (a Python model: opaque, only
/// declares `customers`) → `summary.segment`; `report` reads `summary`; `other` reads
/// `raw` and is untouched by `customers.id`.
fn document() -> Value {
    json!({
        "schema_version": 1,
        "nodes": [
            model("raw", &["id"], false),
            model("orders", &["id"], false),
            model("customers", &["id"], false),
            model("segments", &["segment"], true),
            model("summary", &["segment"], false),
            model("report", &["segment"], false),
            model("other", &["id"], false),
        ],
        "node_edges": [
            { "from": "raw", "to": "orders", "via": "sql" },
            { "from": "orders", "to": "customers", "via": "sql" },
            { "from": "customers", "to": "segments", "via": "declared" },
            { "from": "segments", "to": "summary", "via": "sql" },
            { "from": "summary", "to": "report", "via": "sql" },
            { "from": "raw", "to": "other", "via": "sql" },
        ],
        "column_edges": [
            edge(&col("raw", "id"), &col("orders", "id")),
            edge(&col("orders", "id"), &col("customers", "id")),
            edge(&col("segments", "segment"), &col("summary", "segment")),
            edge(&col("summary", "segment"), &col("report", "segment")),
            edge(&col("raw", "id"), &col("other", "id")),
        ],
    })
}

fn trace(node_bin: &str, node: &str, column: &str) -> Value {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/lineage.js");
    let mut child = Command::new(node_bin)
        .args(["-e", HARNESS, script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({ "doc": document(), "node": node, "column": column });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn a_column_trace_says_where_it_stops_at_an_opaque_node() {
    let Some(node_bin) = node() else {
        eprintln!("skipped: Node isn't installed, so the explorer's script can't run here");
        return;
    };
    let down = trace(&node_bin, "customers", "id");
    // Known lineage as far as it goes…
    assert_eq!(down["up"], json!(["orders.id", "raw.id"]), "{down}");
    // …then a named stop, and everything past it may change: nothing reads as safe.
    assert_eq!(down["stopsDown"], json!(["segments"]), "{down}");
    assert_eq!(
        down["unknownDown"],
        json!(["report", "segments", "summary"]),
        "{down}"
    );
    assert_eq!(down["down"], json!([]), "no column is claimed");
    assert!(
        !down["unknownDown"].to_string().contains("other"),
        "a node off the trail stays off it"
    );

    // Upstream: the column of a node past an opaque one can come from anything the
    // opaque node reads.
    let up = trace(&node_bin, "report", "segment");
    assert_eq!(
        up["up"],
        json!(["segments.segment", "summary.segment"]),
        "{up}"
    );
    assert_eq!(up["stopsUp"], json!(["segments"]), "{up}");
    assert_eq!(
        up["unknownUp"],
        json!(["customers", "orders", "raw"]),
        "{up}"
    );
}

#[test]
fn a_trace_with_no_opaque_node_has_no_stop() {
    let Some(node_bin) = node() else {
        eprintln!("skipped: Node isn't installed");
        return;
    };
    let t = trace(&node_bin, "raw", "id");
    assert_eq!(
        t["down"],
        json!(["customers.id", "orders.id", "other.id"]),
        "{t}"
    );
    assert_eq!(
        t["stopsDown"],
        json!(["segments"]),
        "customers is read by segments: {t}"
    );
    let t = trace(&node_bin, "other", "id");
    assert_eq!(t["stopsDown"], json!([]), "{t}");
    assert_eq!(t["stopsUp"], json!([]), "{t}");
    assert_eq!(t["unknownDown"], json!([]), "{t}");
}
