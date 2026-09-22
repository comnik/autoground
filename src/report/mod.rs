pub mod step;
pub mod tui;

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::core::Compiled;
use crate::eval::{EdbAtom, Outcome, RunResult, Value};
use crate::navigator::{GroundStep, RequireTrace};
use crate::syntax::{print_ground_atom, print_require};

pub use crate::log::{allocate_unique_file, DEFAULT_TRACE_DIR};

pub fn jev_request_count(steps: &[GroundStep]) -> usize {
    steps.iter().map(|s| s.exchanges().len()).sum()
}

/// Wall-clock span of one `autoground run`.
#[derive(Debug, Clone, Copy)]
pub struct RunTiming {
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}

impl RunTiming {
    pub fn from_millis(started_at_ms: u64, ended_at_ms: u64) -> Self {
        Self {
            started_at: crate::navigator::judge::from_epoch_ms(started_at_ms),
            ended_at: crate::navigator::judge::from_epoch_ms(ended_at_ms),
        }
    }

    pub fn elapsed(self) -> Duration {
        self.ended_at
            .duration_since(self.started_at)
            .unwrap_or(Duration::ZERO)
    }
}

/// Start/end of each Jev round-trip that recorded wall-clock times.
pub fn jev_request_intervals(steps: &[GroundStep]) -> Vec<(SystemTime, SystemTime)> {
    steps
        .iter()
        .flat_map(|s| s.exchanges())
        .filter_map(|ex| Some((ex.started_at?, ex.ended_at?)))
        .collect()
}

/// Sum of recorded Jev costs, if any exchange reported one.
pub fn jev_request_cost_usd(steps: &[GroundStep]) -> Option<f64> {
    let mut any = false;
    let mut sum = 0.0;
    for ex in steps.iter().flat_map(|s| s.exchanges()) {
        if let Some(c) = ex.cost_usd {
            any = true;
            sum += c;
        }
    }
    any.then_some(sum)
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else {
        let secs = d.as_secs();
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

pub fn format_usd(cost: f64) -> String {
    if cost >= 0.01 {
        format!("${cost:.2}")
    } else if cost >= 0.0001 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.6}")
    }
}

fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Supported => "SUPPORTED",
        Outcome::Unsupported => "UNSUPPORTED",
        Outcome::Contradicted => "CONTRADICTED",
    }
}

pub fn format_run_header(outcome: &Outcome, trace_path: &Path, jev_requests: usize) -> String {
    let mut out = String::new();
    out.push_str(outcome_label(outcome));
    out.push('\n');
    out.push_str(&format!("trace: {}\n", trace_path.display()));
    out.push_str(&format!("jev requests: {jev_requests}\n"));
    out
}

pub fn require_traces(compiled: &Compiled, result: &RunResult) -> Vec<RequireTrace> {
    result
        .requires
        .iter()
        .enumerate()
        .map(|(i, req)| RequireTrace {
            formula: compiled
                .requires
                .get(i)
                .map(print_require)
                .unwrap_or_else(|| format!("(require #{i})")),
            value: req.value,
            witness: req.witness.clone(),
            edb: req.edb.clone(),
        })
        .collect()
}

pub fn format_run_body(compiled: &Compiled, result: &RunResult) -> String {
    format_require_traces(&require_traces(compiled, result))
}

pub fn format_require_traces(requires: &[RequireTrace]) -> String {
    let mut out = String::new();
    for req in requires {
        out.push('\n');
        out.push_str(&req.formula);
        out.push('\n');
        out.push_str(&format!("  {}\n", req.value));
        if let Some(w) = &req.witness {
            let bits: Vec<String> = w
                .assignment
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let label = if req.value == Value::False {
                "counterexample"
            } else {
                "witness"
            };
            out.push_str(&format!("  {label}: {}\n", bits.join(", ")));
        }
        out.push_str(&format_edb_leaves(&req.edb));
    }
    out
}

/// Human-readable summary of a run: header, then each `require` and the EDB it bottomed out on.
pub fn format_run(
    compiled: &Compiled,
    result: &RunResult,
    trace_path: &Path,
    jev_requests: usize,
) -> String {
    let mut out = format_run_header(&result.outcome, trace_path, jev_requests);
    out.push_str(&format_run_body(compiled, result));
    out
}

fn format_edb_leaves(atoms: &[EdbAtom]) -> String {
    if atoms.is_empty() {
        return String::new();
    }
    let rows: Vec<(String, String)> = atoms
        .iter()
        .map(|a| (print_ground_atom(&a.pred, &a.args), edb_verdict(a.value)))
        .collect();
    let width = rows.iter().map(|(atom, _)| atom.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (atom, verdict) in rows {
        out.push_str("  ");
        out.push_str(&atom);
        if width > atom.len() {
            out.push_str(&" ".repeat(width - atom.len()));
        }
        out.push_str("  ");
        out.push_str(&verdict);
        out.push('\n');
    }
    out
}

fn edb_verdict(value: Value) -> String {
    value.to_string()
}

pub fn outcome_exit_code(value: &Value) -> i32 {
    match value {
        Value::True => 0,
        Value::False => 2,
        Value::Unknown => 1,
    }
}

pub fn run_exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Supported => 0,
        Outcome::Unsupported => 1,
        Outcome::Contradicted => 2,
    }
}

/// A `run` session read back from its log.
#[derive(Debug, Clone)]
pub struct RunLog {
    pub outcome: Outcome,
    pub repo_name: String,
    pub requires: Vec<RequireTrace>,
    pub steps: Vec<GroundStep>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
}

/// Read a run log.
pub fn load_run_log(path: &Path) -> Result<RunLog, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read trace {}: {err}", path.display()))?;
    let events = crate::log::parse_jsonl(&src)
        .map_err(|err| format!("could not read trace {}: {err}", path.display()))?;
    assemble_run(events, path)
}

#[derive(serde::Deserialize)]
struct RunSummary {
    outcome: Outcome,
    #[serde(default)]
    repo_name: String,
    #[serde(default)]
    requires: Vec<RequireTrace>,
    #[serde(default)]
    started_at_ms: Option<u64>,
    #[serde(default)]
    ended_at_ms: Option<u64>,
}

fn assemble_run(events: Vec<serde_json::Value>, path: &Path) -> Result<RunLog, String> {
    let mut steps = Vec::new();
    let mut summary = None;
    for event in events {
        match event.get("kind").and_then(|kind| kind.as_str()) {
            Some("step") => {
                steps
                    .push(serde_json::from_value(event).map_err(|err| {
                        format!("could not read trace {}: {err}", path.display())
                    })?);
            }
            Some("run") => {
                summary =
                    Some(serde_json::from_value::<RunSummary>(event).map_err(|err| {
                        format!("could not read trace {}: {err}", path.display())
                    })?);
            }
            _ => {}
        }
    }
    let Some(summary) = summary else {
        return Err(format!("{} is not a full run trace", path.display()));
    };
    Ok(RunLog {
        outcome: summary.outcome,
        repo_name: summary.repo_name,
        requires: summary.requires,
        steps,
        started_at_ms: summary.started_at_ms,
        ended_at_ms: summary.ended_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compile;
    use crate::eval::run;
    use crate::syntax::parse;

    #[test]
    fn run_log_reloads_steps_and_outcome() {
        let path = std::env::temp_dir().join(format!(
            "autoground-run-log-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = crate::log::Log::open(&path).unwrap();
        let step = GroundStep::Result {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            status: "found".into(),
            bindings: vec![],
        };
        log.append_event("step", &step).unwrap();
        log.append_event(
            "run",
            &serde_json::json!({
                "outcome": "supported",
                "repo_name": "toy",
                "requires": [],
                "started_at_ms": 10,
                "ended_at_ms": 20,
            }),
        )
        .unwrap();
        let loaded = load_run_log(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.outcome, Outcome::Supported);
        assert_eq!(loaded.repo_name, "toy");
        assert_eq!(loaded.steps, vec![step]);
        assert_eq!(loaded.started_at_ms, Some(10));
        assert_eq!(loaded.ended_at_ms, Some(20));
    }

    #[test]
    fn allocate_unique_file_uses_distinct_names() {
        let dir =
            std::env::temp_dir().join(format!("autoground-trace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = allocate_unique_file(&dir, "json").unwrap();
        let b = allocate_unique_file(&dir, "json").unwrap();
        assert_ne!(a, b);
        assert!(a.extension().is_some_and(|e| e == "json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_run_prints_header_each_require_and_edb_verdicts() {
        let src = r#"
(require (component "Frontend"))
(require (component "Missing"))
(require (component "Open"))
"#;
        let compiled = compile(&parse(src).unwrap()).unwrap();
        let mut store = crate::edb::Store::new();
        store.insert_fact("component", vec!["Frontend".into()], "test");
        store.close_pattern("component", vec![Some("Missing".into())]);
        store.bind("Frontend", "ui/app.py");
        let result = run(&compiled, &mut store, None);
        let trace = Path::new("/tmp/autoground/trace-test.json");
        let text = format_run(&compiled, &result, trace, 7);
        assert!(
            text.starts_with(
                "CONTRADICTED\ntrace: /tmp/autoground/trace-test.json\njev requests: 7\n"
            ),
            "{text}"
        );
        assert!(!text.contains(" ← "), "{text}");
        assert!(
            text.contains(
                "(require\n  (component \"Frontend\"))\n  true\n  (component \"Frontend\")  true\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "(require\n  (component \"Missing\"))\n  false\n  (component \"Missing\")  false\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "(require\n  (component \"Open\"))\n  unknown\n  (component \"Open\")  unknown\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn outcome_label_reads_as_how_the_codebase_stands() {
        assert_eq!(outcome_label(&Outcome::Unsupported), "UNSUPPORTED");
        assert_eq!(outcome_label(&Outcome::Supported), "SUPPORTED");
        assert_eq!(outcome_label(&Outcome::Contradicted), "CONTRADICTED");
    }

    #[test]
    fn jev_request_count_sums_exchanges() {
        use crate::navigator::judge::JudgeExchange;
        let empty = GroundStep::Result {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            status: "bound".into(),
            bindings: vec![],
        };
        let listing = GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: String::new(),
            children: vec![],
            choices: vec![],
            effects: vec![],
            exchanges: vec![
                JudgeExchange::new(serde_json::json!({}), serde_json::json!({})),
                JudgeExchange::new(serde_json::json!({}), serde_json::json!({})),
            ],
        };
        assert_eq!(jev_request_count(&[empty, listing]), 2);
    }

    #[test]
    fn format_duration_picks_units() {
        assert_eq!(format_duration(Duration::from_millis(340)), "340ms");
        assert_eq!(format_duration(Duration::from_millis(1420)), "1.42s");
        assert_eq!(format_duration(Duration::from_secs(72)), "1m 12s");
    }

    #[test]
    fn jev_request_cost_sums_reported_costs() {
        use crate::navigator::judge::JudgeExchange;
        let listing = GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: String::new(),
            children: vec![],
            choices: vec![],
            effects: vec![],
            exchanges: vec![
                JudgeExchange {
                    cost_usd: Some(0.0001),
                    ..JudgeExchange::default()
                },
                JudgeExchange {
                    cost_usd: Some(0.0002),
                    ..JudgeExchange::default()
                },
            ],
        };
        assert!((jev_request_cost_usd(&[listing]).unwrap() - 0.0003).abs() < 1e-12);
        assert_eq!(jev_request_cost_usd(&[]), None);
    }
}
