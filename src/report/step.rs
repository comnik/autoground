use crate::navigator::judge::{DocumentVerdict, JudgeExchange};
use crate::navigator::trace::{format_atom, GroundStep};

use super::{format_duration, format_usd};

/// Render one grounding step as plain text (also the TUI body).
pub fn format_step(step: &GroundStep, index: usize, total: usize, repo_name: &str) -> String {
    match step {
        GroundStep::Listing {
            pred,
            args,
            dir,
            children,
            choices,
            effects,
            exchanges: _,
        } => {
            let mut out = header(pred, args, "listing", index, total);
            let where_ = if dir.is_empty() {
                format!("{repo_name}/")
            } else {
                format!("{repo_name}/{dir}/")
            };
            out.push_str(&where_);
            out.push('\n');
            let n = children.len();
            for (i, child) in children.iter().enumerate() {
                let choice = choices.iter().find(|c| c.rel_path == child.rel_path);
                let (mark, label, conf) = match choice {
                    Some(c) => (
                        c.transition.mark(),
                        c.transition.label(),
                        format!("{:.2}", c.confidence),
                    ),
                    None => ("?", "?", "—".into()),
                };
                let branch = if i + 1 == n { "└──" } else { "├──" };
                let kind = if child.is_dir { "/" } else { "" };
                let effect = effects
                    .iter()
                    .find(|e| e.starts_with(&format!("{}:", child.rel_path)))
                    .map(|e| {
                        e.split_once(':')
                            .map(|(_, rest)| rest.trim())
                            .unwrap_or(e.as_str())
                    })
                    .unwrap_or("");
                out.push_str(&format!(
                    "{branch} {name:<16} {mark} {label:<12} {conf:<5}  {effect}\n",
                    name = format!("{}{kind}", child.rel_path),
                ));
            }
            if children.is_empty() {
                out.push_str("    (empty)\n");
            }
            out
        }
        GroundStep::Document {
            pred,
            args,
            path,
            judgement,
            preview,
            note,
            exchanges: _,
        } => {
            let mut out = header(pred, args, "document", index, total);
            out.push_str(path);
            out.push('\n');
            let v = match judgement.verdict {
                DocumentVerdict::True => "true",
                DocumentVerdict::False => "false",
                DocumentVerdict::Unknown => "unknown",
            };
            out.push_str(&format!(
                "    verdict     {v}\n    confidence  {:.2}\n    {}\n",
                judgement.confidence, note
            ));
            if !preview.is_empty() {
                out.push('\n');
                for line in preview.lines() {
                    out.push_str("    │ ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out
        }
        GroundStep::Result {
            pred,
            args,
            status,
            bindings,
        } => {
            let mut out = header(pred, args, "result", index, total);
            out.push_str(status);
            out.push('\n');
            if bindings.is_empty() {
                out.push_str("    (no bindings)\n");
            } else {
                for (c, e) in bindings {
                    out.push_str(&format!("    \"{c}\"  ←  {e}\n"));
                }
            }
            out
        }
    }
}

fn header(pred: &str, args: &[String], kind: &str, index: usize, total: usize) -> String {
    format!(
        "  {}   ·  {kind}  ·  {}/{}\n\n",
        format_atom(pred, args),
        index + 1,
        total
    )
}

pub fn format_trace(steps: &[GroundStep], repo_name: &str) -> String {
    format_trace_with_jev(steps, repo_name, false)
}

/// Full walk, including Jev request/response payloads for each step.
pub fn format_trace_verbose(steps: &[GroundStep], repo_name: &str) -> String {
    format_trace_with_jev(steps, repo_name, true)
}

fn format_trace_with_jev(steps: &[GroundStep], repo_name: &str, verbose: bool) -> String {
    if steps.is_empty() {
        return "no grounding steps\n".into();
    }
    let n = steps.len();
    let mut out = String::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            out.push_str("\n────────────────────────────────\n\n");
        }
        out.push_str(&format_step(step, i, n, repo_name));
        if verbose && !step.exchanges().is_empty() {
            out.push_str(&format_jev(step));
        }
    }
    out
}

/// Pretty-printed Jev request/response for one step, or a note when none.
pub fn format_jev(step: &GroundStep) -> String {
    format_exchanges(step.exchanges())
}

fn format_exchanges(exchanges: &[JudgeExchange]) -> String {
    if exchanges.is_empty() {
        return "\n  (no Jev request)\n".into();
    }
    let mut out = String::from("\n");
    let n = exchanges.len();
    for (i, ex) in exchanges.iter().enumerate() {
        let meta = exchange_meta(ex);
        let req_label = if n == 1 {
            format!("  Jev request{meta}\n")
        } else {
            format!("  Jev request {}/{n}{meta}\n", i + 1)
        };
        let resp_label = if n == 1 {
            "  Jev response\n".to_string()
        } else {
            format!("  Jev response {}/{n}\n", i + 1)
        };
        out.push_str(&req_label);
        out.push_str(&indent_json(&ex.request));
        out.push_str(&resp_label);
        out.push_str(&indent_json(&ex.response));
    }
    out
}

fn exchange_meta(ex: &JudgeExchange) -> String {
    let mut bits = Vec::new();
    if let (Some(start), Some(end)) = (ex.started_at, ex.ended_at) {
        if let Ok(d) = end.duration_since(start) {
            bits.push(format_duration(d));
        }
    }
    if let Some(cost) = ex.cost_usd {
        bits.push(format_usd(cost));
    }
    if bits.is_empty() {
        String::new()
    } else {
        format!("  ({})", bits.join(", "))
    }
}

fn indent_json(value: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let mut out = String::new();
    for line in pretty.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{Child, ChildChoice, Transition};
    use serde_json::json;

    fn listing_with(exchanges: Vec<JudgeExchange>) -> GroundStep {
        GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![Child {
                name: "ui".into(),
                rel_path: "ui".into(),
                is_dir: true,
                sample: vec![],
            }],
            choices: vec![ChildChoice {
                rel_path: "ui".into(),
                transition: Transition::Investigate,
                confidence: 0.9,
            }],
            effects: vec!["ui: investigate → list".into()],
            exchanges,
        }
    }

    #[test]
    fn verbose_listing_prints_jev_json() {
        let step = listing_with(vec![JudgeExchange::new(
            json!({"model": "jev-latest", "questions": {"c0": {"type": "choice"}}}),
            json!({"answers": {"c0": {"choice": "investigate", "confidence": 0.9}}}),
        )]);
        let quiet = format_step(&step, 0, 1, "toy");
        assert!(!quiet.contains("Jev request"));
        assert!(quiet.contains("ui"));
        let payload = format_jev(&step);
        assert!(payload.contains("Jev request"));
        assert!(payload.contains("Jev response"));
        assert!(payload.contains("jev-latest"));
        assert!(payload.contains("\"choice\": \"investigate\""));
    }

    #[test]
    fn verbose_without_exchange_notes_absence() {
        let step = listing_with(Vec::new());
        assert!(format_jev(&step).contains("no Jev request"));
    }

    #[test]
    fn verbose_trace_includes_jev_payloads() {
        let step = listing_with(vec![JudgeExchange::new(
            json!({"model": "jev-latest"}),
            json!({"answers": {"c0": {"choice": "investigate"}}}),
        )]);
        let quiet = format_trace(std::slice::from_ref(&step), "toy");
        let verbose = format_trace_verbose(std::slice::from_ref(&step), "toy");
        assert!(!quiet.contains("Jev request"));
        assert!(verbose.contains("Jev request"));
        assert!(verbose.contains("jev-latest"));
    }

    #[test]
    fn jev_label_includes_duration_and_cost() {
        use std::time::{Duration, UNIX_EPOCH};
        let t0 = UNIX_EPOCH + Duration::from_millis(1_000);
        let step = listing_with(vec![JudgeExchange {
            request: json!({"model": "jev-latest"}),
            response: json!({"answers": {}}),
            started_at: Some(t0),
            ended_at: Some(t0 + Duration::from_millis(340)),
            cost_usd: Some(0.000042),
            ..JudgeExchange::default()
        }]);
        let payload = format_jev(&step);
        assert!(
            payload.contains("Jev request  (340ms, $0.000042)"),
            "{payload}"
        );
    }
}
