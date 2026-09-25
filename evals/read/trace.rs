use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path};

use autoground::universe::{Filesystem, Universe, UniverseError};
use autoground::Value;
use serde::Deserialize;
use serde_json::Value as Json;

use crate::record::Provenance;

const MIN_EXCERPT: usize = 12;
const MAX_EXCERPT: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFiles {
    pub reads: Vec<String>,
    pub outside: Vec<String>,
    pub navigation: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentJudgement {
    pub value: String,
    pub provenance: Vec<AgentCitation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCitation {
    pub path: String,
    pub verdict: String,
    pub excerpt: String,
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
        let value: Json =
            serde_json::from_str(line).map_err(|err| format!("line {}: {err}", number + 1))?;
        let Some(update) = value.pointer("/params/update") else {
            continue;
        };
        let event = update
            .get("sessionUpdate")
            .and_then(Json::as_str)
            .unwrap_or("");
        if event != "tool_call" && event != "tool_call_update" {
            continue;
        }
        let Some(id) = update.get("toolCallId").and_then(Json::as_str) else {
            continue;
        };
        let kind = classify(tool_name(update), update.get("kind").and_then(Json::as_str));
        let call = calls.entry(id.to_string()).or_insert(OpenCall {
            kind: ToolKind::Other,
            paths: BTreeSet::new(),
            done: false,
        });
        if call.kind == ToolKind::Other && kind != ToolKind::Other {
            call.kind = kind;
        }
        collect_paths(update, &mut call.paths);
        let status = update.get("status").and_then(Json::as_str).unwrap_or("");
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

pub fn universe_leaves(universe: &Filesystem) -> Result<HashSet<String>, UniverseError> {
    let mut leaves = HashSet::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in universe.children(&dir)? {
            if universe.summary(&child)?.is_none() {
                leaves.insert(child);
            } else {
                stack.push(child);
            }
        }
    }
    Ok(leaves)
}

pub fn judgement_from_value(value: &Json) -> Result<AgentJudgement, String> {
    if let Some(output) = value
        .get("structured_output")
        .filter(|output| !output.is_null())
    {
        if let Some(text) = output.as_str() {
            return parse_judgement_text(text);
        }
        if output.is_object() {
            return serde_json::from_value(output.clone()).map_err(|err| err.to_string());
        }
    }
    let Some(text) = value.get("text").and_then(Json::as_str) else {
        return Err("headless JSON has no judgement".into());
    };
    parse_judgement_text(text)
}

pub fn accept_judgement(
    judgement: &AgentJudgement,
    repo: &Path,
    leaves: &HashSet<String>,
    opened: &HashSet<String>,
) -> Result<(Value, Vec<Provenance>), String> {
    let value = parse_value(&judgement.value)?;
    let mut provenance = Vec::with_capacity(judgement.provenance.len());
    for citation in &judgement.provenance {
        provenance.push(accept_citation(citation, repo, leaves, opened)?);
    }
    provenance.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.verdict.cmp(&right.verdict))
            .then(left.excerpt.cmp(&right.excerpt))
    });
    provenance.dedup();
    let entailed = provenance.iter().any(|item| item.verdict == "entailed");
    let contradicted = provenance.iter().any(|item| item.verdict == "contradicted");
    match value {
        Value::True if !entailed => Err("true needs an entailed file".into()),
        Value::False if !contradicted => Err("false needs a contradicted file".into()),
        _ => Ok((value, provenance)),
    }
}

fn accept_citation(
    citation: &AgentCitation,
    repo: &Path,
    leaves: &HashSet<String>,
    opened: &HashSet<String>,
) -> Result<Provenance, String> {
    let Some(path) = relativize(&citation.path, repo) else {
        return Err(format!("{} is outside the repository", citation.path));
    };
    if !leaves.contains(&path) {
        return Err(format!("{path} is not a file"));
    }
    if !opened.contains(&path) {
        return Err(format!("{path} was not opened"));
    }
    if citation.verdict != "entailed" && citation.verdict != "contradicted" {
        return Err(format!(
            "verdict {} is not entailed or contradicted",
            citation.verdict
        ));
    }
    let quote = collapse(&citation.excerpt);
    let length = quote.chars().count();
    if length < MIN_EXCERPT {
        return Err(format!("excerpt in {path} is too short"));
    }
    if length > MAX_EXCERPT {
        return Err(format!("excerpt in {path} is too long"));
    }
    let body = fs::read_to_string(repo.join(&path)).map_err(|err| format!("{path}: {err}"))?;
    if !excerpt_in_file(&citation.excerpt, &body) {
        return Err(format!("excerpt is not in {path}"));
    }
    Ok(Provenance {
        path,
        verdict: citation.verdict.clone(),
        excerpt: citation.excerpt.clone(),
    })
}

fn parse_value(text: &str) -> Result<Value, String> {
    match text {
        "true" => Ok(Value::True),
        "false" => Ok(Value::False),
        "unknown" => Ok(Value::Unknown),
        other => Err(format!("value {other} is not true, false, or unknown")),
    }
}

/// `--json-schema` stamps a judgement on every turn, and headless `text`
/// concatenates them. Tool turns emit an empty unknown. The last object is
/// the answer.
fn parse_judgement_text(text: &str) -> Result<AgentJudgement, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("judgement was not JSON".into());
    }
    let mut last = None;
    let stream = serde_json::Deserializer::from_str(text).into_iter::<AgentJudgement>();
    for item in stream {
        last = Some(item.map_err(|err| format!("judgement was not JSON: {err}"))?);
    }
    last.ok_or_else(|| "judgement was not JSON".into())
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Each non-empty excerpt line has to occur in the file. A wrapped doc
/// comment is joined by dropping the `//!` or `///` marker on the continuation.
/// A rustdoc link such as `[`Usage::requests`]` matches the words it displays.
fn excerpt_in_file(excerpt: &str, body: &str) -> bool {
    let surface = collapse(&strip_markers(body));
    let mut any = false;
    let mut lines_ok = true;
    for line in excerpt.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        any = true;
        if !line_in_file(trimmed, body, &surface) {
            lines_ok = false;
        }
    }
    if any && lines_ok {
        return true;
    }
    let quote = quote_surface(excerpt);
    let file = quote_surface(body);
    any && !quote.is_empty() && file.contains(&quote)
}

fn line_in_file(trimmed: &str, body: &str, surface: &str) -> bool {
    if body.contains(trimmed) {
        return true;
    }
    let bare = strip_marker_line(trimmed);
    if bare != trimmed && body.contains(bare) {
        return true;
    }
    let collapsed = collapse(bare);
    !collapsed.is_empty() && surface.contains(&collapsed)
}

fn strip_markers(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        out.push_str(strip_marker_line(line.trim()));
        out.push('\n');
    }
    out
}

fn strip_marker_line(trimmed: &str) -> &str {
    match trimmed
        .strip_prefix("//!")
        .or_else(|| trimmed.strip_prefix("///"))
        .or_else(|| trimmed.strip_prefix("//"))
    {
        Some(rest) => rest.trim(),
        None => trimmed,
    }
}

/// Doc markers removed, rustdoc links reduced to their visible text, and
/// `Type::` prefixes dropped, then whitespace collapsed.
fn quote_surface(text: &str) -> String {
    let stripped = strip_markers(text);
    let mut flat = String::new();
    let chars: Vec<char> = stripped.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '[' {
            if let Some(end) = chars[index + 1..].iter().position(|ch| *ch == ']') {
                let inner: String = chars[index + 1..index + 1 + end]
                    .iter()
                    .filter(|ch| **ch != '`')
                    .collect();
                flat.push_str(inner.trim());
                index += end + 2;
                continue;
            }
        }
        if chars[index] != '`' {
            flat.push(chars[index]);
        }
        index += 1;
    }
    strip_path_prefixes(&collapse(&flat))
}

fn strip_path_prefixes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if is_ident_start(chars[index]) {
            let start = index;
            index += 1;
            while index < chars.len() && is_ident_continue(chars[index]) {
                index += 1;
            }
            if index + 1 < chars.len() && chars[index] == ':' && chars[index + 1] == ':' {
                index += 2;
                continue;
            }
            out.extend(chars[start..index].iter().copied());
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
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

fn tool_name(update: &Json) -> Option<&str> {
    update
        .pointer("/_meta/x.ai/tool/name")
        .and_then(Json::as_str)
        .or_else(|| update.get("title").and_then(Json::as_str))
        .or_else(|| update.get("toolName").and_then(Json::as_str))
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

fn collect_paths(update: &Json, paths: &mut BTreeSet<String>) {
    if let Some(raw) = update.get("rawInput") {
        for key in ["target_file", "target_directory", "path"] {
            if let Some(path) = raw.get(key).and_then(Json::as_str) {
                if !path.is_empty() {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    if let Some(locations) = update.get("locations").and_then(Json::as_array) {
        for location in locations {
            if let Some(path) = location.get("path").and_then(Json::as_str) {
                if !path.is_empty() {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    if let Some(path) = update
        .pointer("/_meta/x.ai/tool/input/path")
        .and_then(Json::as_str)
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
    fn the_headless_object_is_read_from_structured_output_or_text() {
        let structured = serde_json::json!({
            "structured_output": {"value": "unknown", "provenance": []},
            "text": "not json"
        });
        let parsed = judgement_from_value(&structured).unwrap();
        assert_eq!(parsed.value, "unknown");
        assert!(parsed.provenance.is_empty());

        let text = serde_json::json!({
            "text": "{\"value\":\"false\",\"provenance\":[]}"
        });
        let parsed = judgement_from_value(&text).unwrap();
        assert_eq!(parsed.value, "false");
    }

    #[test]
    fn the_last_judgement_in_a_turn_stream_wins() {
        let text = serde_json::json!({
            "text": "{\"value\":\"unknown\",\"provenance\":[]}{\"value\":\"true\",\"provenance\":[]}"
        });
        let parsed = judgement_from_value(&text).unwrap();
        assert_eq!(parsed.value, "true");
    }

    #[test]
    fn a_wrapped_doc_comment_is_still_in_the_file() {
        let body = "//! Prune a universe down to where information is likely\n//! to be found.\n";
        let excerpt = "//! Prune a universe down to where information is likely to be found.";
        assert!(excerpt_in_file(excerpt, body));
        let linked = "/// the impl's max context per request. Batching should raise batch size, raise\n/// fill, and cut [`Usage::requests`].\n";
        let sentence = "Batching should raise batch size, raise fill, and cut requests.";
        assert!(excerpt_in_file(sentence, linked));
        assert!(!excerpt_in_file(
            "Batching should raise batch size and add a GPU compiler.",
            linked
        ));
        let split = "/// Note.\n#[derive(Debug)]\npub enum Value {\n    True,\n}\n";
        let quote = "/// Note.\npub enum Value {\nTrue,\n}";
        assert!(excerpt_in_file(quote, split));
        assert!(!excerpt_in_file("this sentence is not in the file", body));
    }

    fn cite(path: &str, verdict: &str, excerpt: &str) -> AgentCitation {
        AgentCitation {
            path: path.to_string(),
            verdict: verdict.to_string(),
            excerpt: excerpt.to_string(),
        }
    }

    fn judgement(value: &str, provenance: &[AgentCitation]) -> AgentJudgement {
        AgentJudgement {
            value: value.to_string(),
            provenance: provenance.to_vec(),
        }
    }

    fn message(result: Result<(Value, Vec<Provenance>), String>) -> String {
        result.expect_err("expected rejection")
    }

    #[test]
    fn a_judgement_must_cite_an_opened_file() {
        let root = std::env::temp_dir().join(format!(
            "autoground-read-eval-{}-judgement",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn search() {}\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let leaves = universe_leaves(&universe).unwrap();
        let opened = HashSet::from(["src/lib.rs".to_string()]);
        let quote = "pub fn\nsearch()";
        let absolute = root.join("src/lib.rs");

        let (value, cites) = accept_judgement(
            &judgement(
                "true",
                &[cite(absolute.to_str().unwrap(), "entailed", quote)],
            ),
            &root,
            &leaves,
            &opened,
        )
        .unwrap();
        assert_eq!(value, Value::True);
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0].path, "src/lib.rs");
        assert_eq!(cites[0].verdict, "entailed");
        assert_eq!(cites[0].excerpt, quote);

        let missing = message(accept_judgement(
            &judgement(
                "true",
                &[cite("src/lib.rs", "entailed", "no such sentence here")],
            ),
            &root,
            &leaves,
            &opened,
        ));
        assert!(missing.contains("excerpt is not in"), "{missing}");

        let directory = message(accept_judgement(
            &judgement("true", &[cite("src", "entailed", quote)]),
            &root,
            &leaves,
            &opened,
        ));
        assert!(directory.contains("not a file"), "{directory}");

        let unread = message(accept_judgement(
            &judgement("true", &[cite("src/lib.rs", "entailed", quote)]),
            &root,
            &leaves,
            &HashSet::new(),
        ));
        assert!(unread.contains("was not opened"), "{unread}");

        let no_entailed = message(accept_judgement(
            &judgement("true", &[cite("src/lib.rs", "contradicted", quote)]),
            &root,
            &leaves,
            &opened,
        ));
        assert!(no_entailed.contains("entailed"), "{no_entailed}");

        let no_contradicted = message(accept_judgement(
            &judgement("false", &[cite("src/lib.rs", "entailed", quote)]),
            &root,
            &leaves,
            &opened,
        ));
        assert!(
            no_contradicted.contains("contradicted"),
            "{no_contradicted}"
        );

        let (value, cites) =
            accept_judgement(&judgement("unknown", &[]), &root, &leaves, &opened).unwrap();
        assert_eq!(value, Value::Unknown);
        assert!(cites.is_empty());

        let _ = fs::remove_dir_all(&root);
    }
}
