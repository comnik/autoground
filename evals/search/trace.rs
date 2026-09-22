use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFiles {
    pub reads: Vec<String>,
    pub outside: Vec<String>,
    pub navigation: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Read,
    Nav,
    Other,
}

struct OpenCall {
    kind: ToolKind,
    paths: BTreeSet<String>,
    done: bool,
}

pub fn files_from_updates(jsonl: &str, repo: &Path) -> Result<TraceFiles, String> {
    let mut calls: BTreeMap<String, OpenCall> = BTreeMap::new();
    let mut reads = BTreeSet::new();
    let mut outside = BTreeSet::new();
    let mut navigation = BTreeSet::new();

    for (number, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).map_err(|err| format!("line {}: {err}", number + 1))?;
        let Some(update) = value.pointer("/params/update") else {
            continue;
        };
        let event = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        if event != "tool_call" && event != "tool_call_update" {
            continue;
        }
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            continue;
        };
        let kind = classify(
            tool_name(update),
            update.get("kind").and_then(Value::as_str),
        );
        let call = calls.entry(id.to_string()).or_insert(OpenCall {
            kind: ToolKind::Other,
            paths: BTreeSet::new(),
            done: false,
        });
        if call.kind == ToolKind::Other && kind != ToolKind::Other {
            call.kind = kind;
        }
        collect_paths(update, &mut call.paths);
        let status = update.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "completed" && !call.done {
            call.done = true;
            let mut ignored = BTreeSet::new();
            match call.kind {
                ToolKind::Read => place(&call.paths, repo, &mut reads, &mut outside),
                ToolKind::Nav => place(&call.paths, repo, &mut navigation, &mut ignored),
                ToolKind::Other => {}
            }
        }
    }

    Ok(TraceFiles {
        reads: reads.into_iter().collect(),
        outside: outside.into_iter().collect(),
        navigation: navigation.into_iter().collect(),
    })
}

pub fn partition_universe(
    reads: &[String],
    leaves: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut evidence = Vec::new();
    let mut dropped = Vec::new();
    for path in reads {
        if leaves.contains(path) {
            evidence.push(path.clone());
        } else {
            dropped.push(path.clone());
        }
    }
    (evidence, dropped)
}

fn place(
    paths: &BTreeSet<String>,
    repo: &Path,
    inside: &mut BTreeSet<String>,
    outside: &mut BTreeSet<String>,
) {
    for path in paths {
        match relativize(path, repo) {
            Some(rel) => {
                inside.insert(rel);
            }
            None => {
                outside.insert(path.clone());
            }
        }
    }
}

fn relativize(path: &str, repo: &Path) -> Option<String> {
    let raw = Path::new(path);
    if raw.as_os_str().is_empty() {
        return None;
    }
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo.join(raw)
    };
    let repo = if repo.exists() {
        repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())
    } else {
        repo.to_path_buf()
    };
    let absolute = if absolute.exists() {
        absolute.canonicalize().unwrap_or(absolute)
    } else {
        absolute
    };
    let rel = absolute.strip_prefix(&repo).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(".".to_string());
    }
    if rel
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn tool_name(update: &Value) -> Option<&str> {
    update
        .pointer("/_meta/x.ai/tool/name")
        .and_then(Value::as_str)
        .or_else(|| update.get("title").and_then(Value::as_str))
        .or_else(|| update.get("toolName").and_then(Value::as_str))
}

fn classify(name: Option<&str>, kind: Option<&str>) -> ToolKind {
    match name.unwrap_or("") {
        "read_file" => return ToolKind::Read,
        "grep" | "list_dir" => return ToolKind::Nav,
        _ => {}
    }
    match kind.unwrap_or("") {
        "read" => ToolKind::Read,
        "search" | "list" => ToolKind::Nav,
        _ => ToolKind::Other,
    }
}

fn collect_paths(update: &Value, paths: &mut BTreeSet<String>) {
    if let Some(raw) = update.get("rawInput") {
        for key in ["target_file", "target_directory", "path"] {
            if let Some(path) = raw.get(key).and_then(Value::as_str) {
                if !path.is_empty() {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    if let Some(locations) = update.get("locations").and_then(Value::as_array) {
        for location in locations {
            if let Some(path) = location.get("path").and_then(Value::as_str) {
                if !path.is_empty() {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    if let Some(path) = update
        .pointer("/_meta/x.ai/tool/input/path")
        .and_then(Value::as_str)
    {
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"r1","title":"read_file","rawInput":{"target_file":"/repo/src/lib.rs"}}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"r1","status":"completed","kind":"read","locations":[{"path":"/repo/src/lib.rs"}]}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"r2","title":"read_file","rawInput":{"target_file":"/repo/src/lib.rs"}}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"r2","status":"completed"}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"r3","title":"read_file","rawInput":{"target_file":"/other/secret.rs"}}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"r3","status":"completed","locations":[{"path":"/other/secret.rs"}]}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"g1","title":"grep","rawInput":{"pattern":"fn","path":"/repo/src/main.rs"}}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"g1","status":"completed","kind":"search","locations":[{"path":"/repo/src/main.rs"}]}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"r4","title":"read_file","rawInput":{"target_file":"/repo/src/pending.rs"}}}}
"#
    }

    #[test]
    fn completed_reads_are_repo_relative_and_grep_is_navigation() {
        let files = files_from_updates(sample(), Path::new("/repo")).unwrap();
        assert_eq!(files.reads, vec!["src/lib.rs".to_string()]);
        assert_eq!(files.outside, vec!["/other/secret.rs".to_string()]);
        assert_eq!(files.navigation, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn listing_the_repo_root_is_navigation() {
        let jsonl = r#"{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"l1","title":"list_dir","rawInput":{"target_directory":"/repo"}}}}
{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"l1","status":"completed"}}}
"#;
        let files = files_from_updates(jsonl, Path::new("/repo")).unwrap();
        assert_eq!(files.navigation, vec![".".to_string()]);
        assert!(files.reads.is_empty());
    }
}
