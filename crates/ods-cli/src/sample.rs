//! A stand-in execution plan used to exercise the presentation pipeline (ADR-0003 proof
//! of concept).
//!
//! The real `ExecutionPlan` arrives with #11/#20; until then this fixture has the same
//! shape of information (actions, reasons, evidence) so every backend is covered. It is
//! compiled only for tests. Its view is derived entirely from the model, which is the
//! pattern real presenters must follow: every reason in the JSON is visible to people.

use serde::Serialize;

use crate::present::{Level, Present, Span, Tone, TreeItem, ViewNode};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Build,
    Reuse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Decision {
    node: &'static str,
    action: Action,
    reasons: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SamplePlan {
    project: &'static str,
    decisions: Vec<Decision>,
}

impl SamplePlan {
    pub fn fixture() -> Self {
        Self {
            project: "jaffle_shop",
            decisions: vec![
                Decision {
                    node: "model.jaffle_shop.stg_orders",
                    action: Action::Build,
                    reasons: vec![
                        "code fingerprint changed: rendered SQL differs",
                        "config unchanged",
                    ],
                },
                Decision {
                    node: "model.jaffle_shop.orders",
                    action: Action::Build,
                    reasons: vec!["upstream model.jaffle_shop.stg_orders will be rebuilt"],
                },
                Decision {
                    node: "model.jaffle_shop.customers",
                    action: Action::Reuse,
                    reasons: vec!["code and inputs unchanged since last successful run"],
                },
            ],
        }
    }
}

fn action_span(action: Action) -> Span {
    match action {
        Action::Build => Span::toned("BUILD", Tone::Added),
        Action::Reuse => Span::toned("REUSE", Tone::Muted),
    }
}

impl Present for SamplePlan {
    const COMMAND: &'static str = "state.plan";

    fn view(&self) -> ViewNode {
        let rows = self
            .decisions
            .iter()
            .map(|d| {
                vec![
                    vec![Span::toned(d.node, Tone::Code)],
                    vec![action_span(d.action)],
                    vec![Span::plain(d.reasons.len().to_string())],
                ]
            })
            .collect();
        let reasons = TreeItem {
            label: vec![Span::toned("reasons", Tone::Emphasis)],
            children: self
                .decisions
                .iter()
                .map(|d| TreeItem {
                    label: vec![
                        Span::toned(d.node, Tone::Code),
                        Span::plain(" "),
                        action_span(d.action),
                    ],
                    children: d
                        .reasons
                        .iter()
                        .map(|r| TreeItem::leaf(vec![Span::plain(*r)]))
                        .collect(),
                })
                .collect(),
        };
        let builds = self
            .decisions
            .iter()
            .filter(|d| matches!(d.action, Action::Build))
            .count();
        ViewNode::Group(vec![
            ViewNode::Heading(format!("Execution plan for {}", self.project)),
            ViewNode::Table {
                title: None,
                columns: vec!["node".into(), "action".into(), "reasons".into()],
                rows,
            },
            ViewNode::Tree(reasons),
            ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "{builds} of {} nodes will run; state is committed only if dbt succeeds",
                    self.decisions.len()
                ))],
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{ColorChoice, Mode, OutputSettings};
    use crate::present::emit;
    use crate::present::view::plain_text;

    fn render(mode: Mode, color: ColorChoice) -> String {
        let settings = OutputSettings {
            mode,
            color,
            width: Some(100),
        };
        let mut out = Vec::new();
        emit(&SamplePlan::fixture(), &settings, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "[ods-version]")
    }

    fn collect_text(node: &ViewNode, out: &mut String) {
        fn tree(item: &TreeItem, out: &mut String) {
            out.push_str(&plain_text(&item.label));
            item.children.iter().for_each(|c| tree(c, out));
        }
        match node {
            ViewNode::Group(children) => children.iter().for_each(|c| collect_text(c, out)),
            ViewNode::Tree(root) => tree(root, out),
            _ => {}
        }
    }

    #[test]
    fn every_reason_is_visible_in_the_view() {
        let plan = SamplePlan::fixture();
        let mut text = String::new();
        collect_text(&plan.view(), &mut text);
        for reason in plan.decisions.iter().flat_map(|d| &d.reasons) {
            assert!(text.contains(reason), "reason missing from view: {reason}");
        }
    }

    /// Contract snapshots (ADR-0003 §5): JSON and plain output are public interfaces.
    mod contract {
        use super::*;

        #[test]
        fn plan_json() {
            insta::assert_snapshot!(render(Mode::Json, ColorChoice::Never));
        }

        #[test]
        fn plan_plain() {
            insta::assert_snapshot!(render(Mode::Plain, ColorChoice::Never));
        }
    }

    /// Presentation snapshots: may change legitimately with rs-rich upgrades, so they
    /// live apart from the contract snapshots.
    mod rich {
        use super::*;

        fn snapshot(name: &str, output: &str) {
            insta::with_settings!({ snapshot_path => "snapshots/rich", prepend_module_to_snapshot => false }, {
                insta::assert_snapshot!(name, output);
            });
        }

        #[test]
        fn plan_human_no_color() {
            snapshot(
                "plan_human_no_color",
                &render(Mode::Human, ColorChoice::Never),
            );
        }

        #[test]
        fn plan_human_color() {
            snapshot(
                "plan_human_color",
                &render(Mode::Human, ColorChoice::Always),
            );
        }
    }
}
