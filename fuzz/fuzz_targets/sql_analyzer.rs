//! Any text is analyzed without panicking, in every dialect: SQL that can't be read is
//! opaque, and an opaque result claims nothing (rule 3). The input is the SQL itself, so
//! a seed (real SQL) reaches the parser unchanged.
#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use ods_core::Confidence;
use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};
use ods_sdk::contracts::sql_lineage::{AnalyzeRequest, MapSchema, SqlLineageAnalyzer};

fuzz_target!(|sql: &str| {
    for dialect in SqlDialect::ALL {
        let analyzer = SqlparserAnalyzer::new(dialect);
        let schema = MapSchema(
            analyzer
                .relation_name("db.orders")
                .map(|r| BTreeMap::from([(r, vec![analyzer.column_name("id")])]))
                .unwrap_or_default(),
        );
        if let Ok(lineage) = analyzer.analyze(&AnalyzeRequest {
            sql,
            schema: &schema,
        }) {
            if lineage.opaque {
                assert_eq!(lineage.confidence, Confidence::Unknown);
                assert!(lineage.outputs.is_empty());
            }
        }
        let _ = analyzer.relation_name(sql);
        let _ = analyzer.column_name(sql);
    }
});
