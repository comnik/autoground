pub mod step;
pub mod tui;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::Compiled;
use crate::edb::Store;
use crate::eval::{Outcome, RunResult, Value};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
