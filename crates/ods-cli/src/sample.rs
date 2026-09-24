//! A stand-in execution plan used to exercise the presentation pipeline (ADR-0003 proof of concept).
//!
//! The real `ExecutionPlan` arrives with #11/#20; until then this fixture has the same
//! shape of information (actions, reasons, evidence) so every backend is covered. It is
//! compiled only for tests.

use serde::Serialize;

use crate::present::{Level, Present, Span, Tone, TreeItem, ViewNode};

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Build,
    Reuse,
}

#[derive(Debug, Serialize)]
struct Decision {
    node: &'static str,
    action: Action,
    reasons: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
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

impl Present for SamplePlan {
    const COMMAND: &'static str = "state.plan";

    fn view(&self) -> ViewNode {
        let action = |a: &Action| match a {
            Action::Build => Span::toned("BUILD", Tone::Added),
            Action::Reuse => Span::toned("REUSE", Tone::Muted),
        };
        let rows = self
            .decisions
            .iter()
            .map(|d| {
                vec![
                    vec![Span::toned(d.node, Tone::Code)],
                    vec![action(&d.action)],
                    vec![Span::plain(d.reasons[0])],
                ]
            })
            .collect();
        let why = TreeItem {
            label: vec![Span::plain("why model.jaffle_shop.orders builds")],
            children: vec![TreeItem {
                label: vec![Span::plain(
                    "upstream model.jaffle_shop.stg_orders will be rebuilt",
                )],
                children: vec![TreeItem::leaf(vec![Span::plain(
                    "code fingerprint changed: rendered SQL differs",
                )])],
            }],
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
                columns: vec!["node".into(), "action".into(), "reason".into()],
                rows,
            },
            ViewNode::Tree(why),
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

    fn render(mode: Mode, color: ColorChoice) -> String {
        let settings = OutputSettings {
            mode,
            color,
            width: Some(100),
        };
        let mut out = Vec::new();
        emit(&SamplePlan::fixture(), &settings, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plan_json() {
        insta::assert_snapshot!(render(Mode::Json, ColorChoice::Never));
    }

    #[test]
    fn plan_plain() {
        insta::assert_snapshot!(render(Mode::Plain, ColorChoice::Never));
    }

    #[test]
    fn plan_human_no_color() {
        insta::assert_snapshot!(render(Mode::Human, ColorChoice::Never));
    }

    #[test]
    fn plan_human_color() {
        insta::assert_snapshot!(render(Mode::Human, ColorChoice::Always));
    }
}
