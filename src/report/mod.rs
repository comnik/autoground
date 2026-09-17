pub mod step;
pub mod tui;

use crate::core::Compiled;
use crate::edb::Store;
use crate::eval::{Outcome, RunResult, Value};

/// Human-readable summary of a run.
pub fn format_run(compiled: &Compiled, store: &Store, result: &RunResult) -> String {
    let mut out = String::new();
    match result.outcome {
        Outcome::Grounded => out.push_str("GROUNDED\n"),
        Outcome::Violated => out.push_str("VIOLATED\n"),
        Outcome::Ungrounded => out.push_str("UNGROUNDED\n"),
    }

    let bindings = store.all_bindings();
    if !bindings.is_empty() {
        for b in bindings {
            out.push_str(&format!("  \"{}\"  ←  {}\n", b.constant, b.entity));
        }
    }

    for (i, req) in result.requires.iter().enumerate() {
        out.push_str(&format!("  require {i}: {}\n", req.value));
        if let Some(w) = &req.witness {
            let bits: Vec<String> = w
                .assignment
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            out.push_str(&format!("    witness: {}\n", bits.join(", ")));
        }
    }

    let mut edb: Vec<_> = compiled.edb_preds.iter().collect();
    edb.sort();
    let mut idb: Vec<_> = compiled.idb_preds.iter().collect();
    idb.sort();
    out.push_str(&format!("  EDB: {}\n", join_names(&edb)));
    out.push_str(&format!("  IDB: {}\n", join_names(&idb)));
    out
}

fn join_names(names: &[&String]) -> String {
    names
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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
