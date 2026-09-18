pub mod step;
pub mod tui;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::Compiled;
use crate::eval::{EdbAtom, Outcome, RunResult, Value};
use crate::navigator::GroundStep;
use crate::syntax::{print_ground_atom, print_require};

pub const DEFAULT_TRACE_DIR: &str = "/tmp/autoground";

/// Create a new uniquely named file under `dir` and return its path.
pub fn allocate_unique_file(dir: &Path, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ext = ext.trim_start_matches('.');
    for n in 0u32..10_000 {
        let path = dir.join(format!("trace-{nanos}-{pid}-{n}.{ext}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique trace filename",
    ))
}

pub fn allocate_record_path() -> std::io::Result<PathBuf> {
    allocate_unique_file(Path::new(DEFAULT_TRACE_DIR), "json")
}

pub fn jev_request_count(steps: &[GroundStep]) -> usize {
    steps.iter().map(|s| s.exchanges().len()).sum()
}

fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Grounded => "GROUNDED",
        Outcome::Violated => "VIOLATED",
        Outcome::Ungrounded => "UNKNOWN",
    }
}

/// Human-readable summary of a run: header, then each `require` and the EDB it bottomed out on.
pub fn format_run(
    compiled: &Compiled,
    result: &RunResult,
    trace_path: &Path,
    jev_requests: usize,
) -> String {
    let mut out = String::new();
    out.push_str(outcome_label(&result.outcome));
    out.push('\n');
    out.push_str(&format!("trace: {}\n", trace_path.display()));
    out.push_str(&format!("jev requests: {jev_requests}\n"));

    for (i, req) in result.requires.iter().enumerate() {
        out.push('\n');
        let formula = compiled
            .requires
            .get(i)
            .map(print_require)
            .unwrap_or_else(|| format!("(require #{i})"));
        out.push_str(&formula);
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

fn format_edb_leaves(atoms: &[EdbAtom]) -> String {
    if atoms.is_empty() {
        return String::new();
    }
    let rows: Vec<(String, &'static str)> = atoms
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
        out.push_str(verdict);
        out.push('\n');
    }
    out
}

fn edb_verdict(value: Value) -> &'static str {
    match value {
        Value::True => "proved",
        Value::False => "disproved",
        Value::Unknown => "unknown",
    }
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
        Outcome::Grounded => 0,
        Outcome::Ungrounded => 1,
        Outcome::Violated => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compile;
    use crate::eval::run;
    use crate::syntax::parse;

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
            text.starts_with("VIOLATED\ntrace: /tmp/autoground/trace-test.json\njev requests: 7\n"),
            "{text}"
        );
        assert!(!text.contains(" ← "), "{text}");
        assert!(
            text.contains(
                "(require\n  (component \"Frontend\"))\n  true\n  (component \"Frontend\")  proved\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "(require\n  (component \"Missing\"))\n  false\n  (component \"Missing\")  disproved\n"
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
    fn outcome_label_prints_unknown_for_ungrounded() {
        assert_eq!(outcome_label(&Outcome::Ungrounded), "UNKNOWN");
        assert_eq!(outcome_label(&Outcome::Grounded), "GROUNDED");
        assert_eq!(outcome_label(&Outcome::Violated), "VIOLATED");
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
                JudgeExchange {
                    request: serde_json::json!({}),
                    response: serde_json::json!({}),
                },
                JudgeExchange {
                    request: serde_json::json!({}),
                    response: serde_json::json!({}),
                },
            ],
        };
        assert_eq!(jev_request_count(&[empty, listing]), 2);
    }
}
