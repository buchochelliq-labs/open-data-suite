//! Benchmark: column lineage for a synthetic project.
//!
//! ```text
//! cargo run --release -p ods-cli --example lineage_bench -- 2000
//! ```
//!
//! Builds a layered DAG of `N` models. Each model has a CTE, a join of two upstream
//! models, aggregation, a CASE and a window function, and 12 output columns. Reports
//! a cold build, a warm (fully cached) rebuild, a rebuild after one model changes,
//! and an impact query.

use std::time::Instant;

use ods_core::{ColumnRef, RelationName};
use ods_lineage::{
    Change, ColumnChangeKind, LineageNode, LineageProject, MemoryCache, NodeKind, build,
};
use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};

const WIDTH: usize = 50;

fn rel(i: usize) -> RelationName {
    RelationName::new(["cat", "sch", &format!("m{i}")]).expect("valid")
}

fn model_sql(a: usize, b: usize, variant: usize) -> String {
    format!(
        "with base as (
            select x.k, x.c0 + {variant} as c0, x.c1, x.c2, y.c3, y.c4, y.c5
            from cat.sch.m{a} x join cat.sch.m{b} y on x.k = y.k
            where x.c6 > 0 and y.c7 is not null
        )
        select k,
               sum(c0) as c0, max(c1) as c1, min(c2) as c2, avg(c3) as c3,
               count(c4) as c4, sum(c5) as c5,
               case when sum(c0) > 100 then 'hi' else 'lo' end as c6,
               max(c1) - min(c2) as c7,
               row_number() over (partition by k order by sum(c0)) as c8,
               coalesce(max(c3), 0) as c9,
               upper(max(cast(c4 as string))) as c10
        from base
        group by k"
    )
}

fn project(n: usize, changed: Option<usize>) -> LineageProject {
    let columns: Vec<String> = std::iter::once("k".to_owned())
        .chain((0..=10).map(|c| format!("c{c}")))
        .collect();
    let mut nodes = Vec::with_capacity(n + WIDTH);
    for s in 0..WIDTH {
        nodes.push(
            LineageNode::new(
                format!("seed{s}"),
                RelationName::new(["cat", "sch", &format!("s{s}")]).expect("valid"),
                NodeKind::Seed,
            )
            .with_columns(columns.iter().cloned()),
        );
    }
    for i in 0..n {
        // Each model reads two models from the previous layer (or seeds for layer 0).
        let (a, b, deps) = if i < WIDTH {
            (
                i,
                (i + 1) % WIDTH,
                vec![format!("seed{i}"), format!("seed{}", (i + 1) % WIDTH)],
            )
        } else {
            let a = i - WIDTH;
            let b = i - WIDTH + usize::from(i % WIDTH != WIDTH - 1);
            (a, b, vec![format!("m{a}"), format!("m{b}")])
        };
        let (sa, sb) = if i < WIDTH {
            (format!("s{a}"), format!("s{b}"))
        } else {
            (format!("m{a}"), format!("m{b}"))
        };
        let variant = usize::from(changed == Some(i));
        let sql = model_sql(0, 0, variant)
            .replace("cat.sch.m0 x", &format!("cat.sch.{sa} x"))
            .replace("cat.sch.m0 y", &format!("cat.sch.{sb} y"));
        nodes.push(
            LineageNode::new(format!("m{i}"), rel(i), NodeKind::Model)
                .with_sql(sql)
                .with_depends_on(deps),
        );
    }
    LineageProject::new(nodes)
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let analyzer = SqlparserAnalyzer::new(SqlDialect::Databricks);
    let cache = MemoryCache::default();

    let base = project(n, None);
    let t = Instant::now();
    let (graph, stats) = build(&base, &analyzer, &cache).expect("builds");
    let cold = t.elapsed();
    assert_eq!(stats.opaque, 0, "every model should be analyzable");

    let t = Instant::now();
    let (_, warm_stats) = build(&base, &analyzer, &cache).expect("builds");
    let warm = t.elapsed();

    let changed = project(n, Some(n / 2));
    let t = Instant::now();
    let (_, inc_stats) = build(&changed, &analyzer, &cache).expect("builds");
    let incremental = t.elapsed();

    let t = Instant::now();
    let impact = graph.impact(&[Change::Column {
        column: ColumnRef::new(rel(0), "c1"),
        kind: ColumnChangeKind::Modified,
    }]);
    let impact_time = t.elapsed();

    let edges: usize = graph
        .nodes()
        .filter_map(|n| n.lineage.as_ref())
        .map(|l| l.outputs.iter().map(|o| o.inputs.len()).sum::<usize>() + l.row_inputs.len())
        .sum();
    println!("models: {n} ({} waves), column edges: {edges}", stats.waves);
    println!(
        "cold build:        {:>8.1} ms ({} analyzed, {:.0} µs/model wall)",
        ms(cold),
        stats.analyzed,
        cold.as_secs_f64() * 1e6 / f64::from(u32::try_from(n).unwrap_or(u32::MAX))
    );
    println!(
        "warm rebuild:      {:>8.1} ms ({} cached, {} analyzed)",
        ms(warm),
        warm_stats.cached,
        warm_stats.analyzed
    );
    println!(
        "one model changed: {:>8.1} ms ({} re-analyzed)",
        ms(incremental),
        inc_stats.analyzed
    );
    println!(
        "impact of m0.c1:   {:>8.1} ms ({} impacted, {} pruned readers)",
        ms(impact_time),
        impact.nodes.len(),
        impact.pruned.len()
    );
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
