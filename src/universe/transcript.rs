//! A coding-agent transcript as a tree of databases.
//!
//! One `chat_history.jsonl`. The root's children are user prompts
//! (`prompt_index`). Each prompt's children are model loops. Each loop's
//! children are read in full: a tool call and its result, in that order. System
//! text and synthetic reminders are omitted. Ids stay short (`p0/c0/00-call-read_file`);
//! the rendered listing carries the prompt and the tool lines.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::{Universe, UniverseError};

/// Prompt text kept on the prompt node. Longer prompts keep the head.
const PROMPT_HEADER_BUDGET: usize = 4 * 1024;
/// Prompt text repeated above the tools of one model loop.
const LOOP_PROMPT_EXCERPT: usize = 480;

/// In-memory transcript. `clone` shares the parsed tree; `set` replaces the window.
#[derive(Clone)]
pub struct Transcript {
    tree: Arc<Tree>,
    databases: Vec<String>,
}

struct Tree {
    nodes: HashMap<String, Node>,
}

struct Node {
    /// Summary text. Absent when the database is read instead.
    summary: Option<String>,
    /// Full text of a database with no summary.
    body: String,
    children: Vec<String>,
}

impl Transcript {
    /// `path` is `chat_history.jsonl`, or a directory that contains that file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, UniverseError> {
        let path = path.as_ref();
        let file = if path.is_dir() {
            path.join("chat_history.jsonl")
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file).map_err(|err| {
            UniverseError::Io(io::Error::new(
                err.kind(),
                format!("reading {}: {err}", file.display()),
            ))
        })?;
        Self::parse(&text)
    }

    pub fn parse(jsonl: &str) -> Result<Self, UniverseError> {
        let prompts = read_prompts(jsonl)?;
        if prompts.is_empty() {
            return Err(invalid("transcript has no user prompts"));
        }
        Ok(Self::from_prompts(prompts))
    }

    fn from_prompts(prompts: Vec<PromptDraft>) -> Self {
        let mut nodes = HashMap::new();
        let mut prompt_ids = Vec::new();
        for (index, prompt) in prompts.into_iter().enumerate() {
            let prompt_id = format!("p{index}");
            let mut cycle_ids = Vec::new();
            for (cycle_index, cycle) in prompt.cycles.iter().enumerate() {
                let cycle_id = format!("{prompt_id}/c{cycle_index}");
                let drafts = leaves_of(cycle);
                let mut leaf_ids = Vec::new();
                let mut lines = Vec::new();
                for (leaf_index, leaf) in drafts.iter().enumerate() {
                    let leaf_id = format!("{cycle_id}/{:02}-{}", leaf_index, leaf.slug);
                    leaf_ids.push(leaf_id.clone());
                    lines.push(leaf.line.clone());
                    nodes.insert(leaf_id, Node::readable(leaf.body.clone()));
                }
                let listing =
                    render_block(&head_excerpt(&prompt.text, LOOP_PROMPT_EXCERPT), &lines);
                nodes.insert(cycle_id.clone(), Node::summarized(listing, leaf_ids));
                cycle_ids.push(cycle_id);
            }
            let child_lines: Vec<String> = cycle_ids
                .iter()
                .map(|id| format!("{}/", id.rsplit('/').next().unwrap_or(id)))
                .collect();
            let listing = render_block(
                &head_excerpt(&prompt.text, PROMPT_HEADER_BUDGET),
                &child_lines,
            );
            nodes.insert(prompt_id.clone(), Node::summarized(listing, cycle_ids));
            prompt_ids.push(prompt_id);
        }
        let root_lines: Vec<String> = prompt_ids.iter().map(|id| format!("{id}/")).collect();
        nodes.insert(
            String::new(),
            Node::summarized(render_block("/", &root_lines), prompt_ids.clone()),
        );
        let mut databases = prompt_ids;
        databases.sort();
        Self {
            tree: Arc::new(Tree { nodes }),
            databases,
        }
    }

    fn node(&self, id: &str) -> Result<&Node, UniverseError> {
        self.tree
            .nodes
            .get(id)
            .ok_or_else(|| UniverseError::Missing(id.to_string()))
    }
}

impl Node {
    fn readable(body: String) -> Self {
        Self {
            summary: None,
            body,
            children: Vec::new(),
        }
    }

    fn summarized(summary: String, children: Vec<String>) -> Self {
        Self {
            summary: Some(summary),
            body: String::new(),
            children,
        }
    }
}

impl Universe for Transcript {
    fn databases(&self) -> &[String] {
        &self.databases
    }

    fn parent(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            None
        } else {
            Some(
                id.rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("")
                    .to_string(),
            )
        }
    }

    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError> {
        Ok(self.node(id)?.children.clone())
    }

    fn summary(&self, id: &str) -> Result<Option<String>, UniverseError> {
        Ok(self.node(id)?.summary.clone())
    }

    fn size(&self) -> Result<usize, UniverseError> {
        let mut total = 0usize;
        for id in &self.databases {
            let node = self.node(id)?;
            total += match &node.summary {
                Some(text) => text.len(),
                None => node.body.len(),
            };
        }
        Ok(total)
    }

    fn read(&self, id: &str, budget: usize) -> Result<String, UniverseError> {
        let node = self.node(id)?;
        if node.summary.is_some() {
            return Err(UniverseError::Summarizable(id.to_string()));
        }
        Ok(trim_text(&node.body, budget))
    }

    fn insert(&mut self, id: String) -> Result<(), UniverseError> {
        let _ = self.node(&id)?;
        insert_sorted(&mut self.databases, id);
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        if let Ok(i) = self.databases.binary_search(&id.to_string()) {
            self.databases.remove(i);
        }
    }

    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError> {
        let mut next = ids;
        next.sort();
        next.dedup();
        for id in &next {
            let _ = self.node(id)?;
        }
        self.databases = next;
        Ok(())
    }
}

struct PromptDraft {
    text: String,
    cycles: Vec<CycleDraft>,
}

struct CycleDraft {
    text: String,
    calls: Vec<Call>,
    results: HashMap<String, String>,
}

struct Call {
    id: String,
    name: String,
    arguments: String,
}

struct LeafDraft {
    slug: String,
    line: String,
    body: String,
}

fn read_prompts(jsonl: &str) -> Result<Vec<PromptDraft>, UniverseError> {
    let mut prompts: Vec<PromptDraft> = Vec::new();
    for (number, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|err| invalid(format!("line {}: {err}", number + 1)))?;
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "user" => {
                if value
                    .get("synthetic_reason")
                    .and_then(Value::as_str)
                    .is_some()
                {
                    continue;
                }
                if value.get("prompt_index").and_then(Value::as_u64).is_none() {
                    continue;
                }
                let text = user_prompt(&json_text(value.get("content").unwrap_or(&Value::Null)));
                if text.is_empty() {
                    continue;
                }
                prompts.push(PromptDraft {
                    text,
                    cycles: Vec::new(),
                });
            }
            "assistant" => {
                let Some(prompt) = prompts.last_mut() else {
                    continue;
                };
                let text = json_text(value.get("content").unwrap_or(&Value::Null));
                let calls = parse_calls(value.get("tool_calls"));
                if calls.is_empty() && text.trim().is_empty() {
                    continue;
                }
                prompt.cycles.push(CycleDraft {
                    text,
                    calls,
                    results: HashMap::new(),
                });
            }
            "tool_result" => {
                let Some(prompt) = prompts.last_mut() else {
                    continue;
                };
                let Some(cycle) = prompt.cycles.last_mut() else {
                    continue;
                };
                let Some(id) = value.get("tool_call_id").and_then(Value::as_str) else {
                    continue;
                };
                cycle.results.insert(
                    id.to_string(),
                    json_text(value.get("content").unwrap_or(&Value::Null)),
                );
            }
            _ => {}
        }
    }
    Ok(prompts)
}

fn parse_calls(value: Option<&Value>) -> Vec<Call> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
            let name = if name.is_empty() { "tool" } else { name };
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call-{index}"));
            let arguments = match item.get("arguments") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            Call {
                id,
                name: name.to_string(),
                arguments,
            }
        })
        .collect()
}

fn leaves_of(cycle: &CycleDraft) -> Vec<LeafDraft> {
    if cycle.calls.is_empty() {
        let text = cycle.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        return vec![LeafDraft {
            slug: "message".to_string(),
            line: one_line(text, 120),
            body: text.to_string(),
        }];
    }
    let mut leaves = Vec::new();
    for call in &cycle.calls {
        let slug = tool_slug(&call.name);
        let sketch = arg_sketch(&call.arguments);
        let line = if sketch.is_empty() {
            call.name.clone()
        } else {
            format!("{} {sketch}", call.name)
        };
        leaves.push(LeafDraft {
            slug: format!("call-{slug}"),
            line,
            body: call_body(&call.name, &call.arguments),
        });
        let content = cycle
            .results
            .get(&call.id)
            .map(String::as_str)
            .unwrap_or("");
        leaves.push(LeafDraft {
            slug: format!("result-{slug}"),
            line: result_line(&call.name, content),
            body: format!("{}\n{content}", call.name),
        });
    }
    leaves
}

fn user_prompt(text: &str) -> String {
    let trimmed = text.trim();
    if let (Some(start), Some(end)) = (trimmed.find("<user_query>"), trimmed.find("</user_query>"))
    {
        if end > start {
            return trimmed[start + "<user_query>".len()..end]
                .trim()
                .to_string();
        }
    }
    trimmed.to_string()
}

fn json_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(json_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                text.to_string()
            } else if let Some(content) = map.get("content") {
                json_text(content)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

const ARG_KEYS: &[&str] = &[
    "target_file",
    "file_path",
    "command",
    "pattern",
    "query",
    "path",
    "target_directory",
    "url",
];

fn arg_sketch(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return one_line(arguments, 120);
    };
    let Some(map) = value.as_object() else {
        return one_line(value.as_str().unwrap_or(""), 120);
    };
    for key in ARG_KEYS {
        if let Some(text) = map.get(*key).and_then(scalar_text) {
            if !text.is_empty() {
                return one_line(&text, 120);
            }
        }
    }
    for value in map.values() {
        if let Some(text) = scalar_text(value) {
            if !text.is_empty() {
                return one_line(&text, 120);
            }
        }
    }
    String::new()
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn call_body(name: &str, arguments: &str) -> String {
    let pretty = match serde_json::from_str::<Value>(arguments) {
        Ok(value) if value.is_object() => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| arguments.to_string())
        }
        _ => arguments.to_string(),
    };
    if pretty.is_empty() {
        name.to_string()
    } else {
        format!("{name}\n{pretty}")
    }
}

fn result_line(name: &str, content: &str) -> String {
    let first = content
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let first = one_line(first, 80);
    if first.is_empty() {
        format!("{name} result")
    } else {
        format!("{name} result: {first}")
    }
}

fn tool_slug(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "tool".to_string()
    } else {
        slug.chars().take(40).collect()
    }
}

fn render_block(header: &str, lines: &[String]) -> String {
    let mut out = header.trim_end_matches('\n').to_string();
    out.push('\n');
    for line in lines {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    head_excerpt(&flat, max)
}

fn head_excerpt(text: &str, budget: usize) -> String {
    let text = text.trim();
    if text.len() <= budget {
        return text.to_string();
    }
    let marker = "…";
    let keep = budget.saturating_sub(marker.len());
    let end = floor_boundary(text, keep.min(text.len()));
    format!("{}{marker}", &text[..end])
}

fn insert_sorted(vec: &mut Vec<String>, id: String) {
    if let Err(index) = vec.binary_search(&id) {
        vec.insert(index, id);
    }
}

fn invalid(message: impl Into<String>) -> UniverseError {
    UniverseError::Io(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

/// Head and tail of `text`, at most `budget` bytes, with a marker in the middle.
fn trim_text(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let marker = "\n…\n";
    if budget <= marker.len() {
        return prefix_bytes(text, budget);
    }
    let body = budget - marker.len();
    let head_end = floor_boundary(text, body / 2);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(body - body / 2));
    if tail_start <= head_end {
        return prefix_bytes(text, budget);
    }
    let mut out = String::new();
    out.push_str(&text[..head_end]);
    out.push_str(marker);
    out.push_str(&text[tail_start..]);
    if out.len() > budget {
        return prefix_bytes(text, budget);
    }
    out
}

fn prefix_bytes(text: &str, budget: usize) -> String {
    let end = floor_boundary(text, budget.min(text.len()));
    text[..end].to_string()
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
{"type":"system","content":"you are a bot SECRET_SYSTEM"}
{"type":"user","content":[{"type":"text","text":"SECRET_CONTEXT user info"}]}
{"type":"user","content":[{"type":"text","text":"SECRET_REMINDER"}],"synthetic_reason":"system_reminder"}
{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nWhere is the universe trait?\n</user_query>"}]}
{"type":"reasoning","summary":[]}
{"type":"assistant","content":"I'll look.","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"src/universe/mod.rs\"}"},{"id":"c2","name":"grep","arguments":"{\"pattern\":\"trait Universe\",\"path\":\"src\"}"}]}
{"type":"tool_result","tool_call_id":"c2","content":"src/universe/mod.rs:18:pub trait Universe"}
{"type":"tool_result","tool_call_id":"c1","content":"pub trait Universe {\n    fn objects(&self) -> &[String];\n"}
{"type":"assistant","content":"","tool_calls":[{"id":"c3","name":"list_dir","arguments":"{\"target_directory\":\"src/universe\"}"}]}
{"type":"tool_result","tool_call_id":"c3","content":"mod.rs\nfs.rs\n"}
"#;

    fn transcript() -> Transcript {
        Transcript::parse(FIXTURE).unwrap()
    }

    #[test]
    fn prompt_listing_is_the_prompt_text() {
        let tree = transcript();
        let listing = tree.summary("p0").unwrap().unwrap();
        assert!(
            listing.starts_with("Where is the universe trait?"),
            "{listing}"
        );
        assert!(listing.contains("\n  c0/\n"), "{listing}");
        assert!(listing.contains("\n  c1/\n"), "{listing}");
        assert!(!listing.contains("p0/"), "{listing}");
        assert!(!listing.contains("SECRET_SYSTEM"), "{listing}");
        assert!(!listing.contains("SECRET_CONTEXT"), "{listing}");
        assert!(!listing.contains("SECRET_REMINDER"), "{listing}");
        assert_eq!(tree.parent("p0").as_deref(), Some(""));
        assert_eq!(tree.databases(), &["p0".to_string()]);
    }

    #[test]
    fn loop_listing_includes_the_prompt_and_its_tools() {
        let tree = transcript();
        let listing = tree.summary("p0/c0").unwrap().unwrap();
        assert!(
            listing.contains("Where is the universe trait?"),
            "{listing}"
        );
        assert!(
            listing.contains("read_file src/universe/mod.rs"),
            "{listing}"
        );
        assert!(listing.contains("grep trait Universe"), "{listing}");
        assert!(
            listing.contains("read_file result: pub trait Universe {"),
            "{listing}"
        );
        assert!(tree.summary("p0/c0").unwrap().is_some());
        assert!(matches!(
            tree.read("p0/c0", usize::MAX),
            Err(UniverseError::Summarizable(_))
        ));
        assert_eq!(
            tree.children("p0/c0").unwrap(),
            vec![
                "p0/c0/00-call-read_file".to_string(),
                "p0/c0/01-result-read_file".to_string(),
                "p0/c0/02-call-grep".to_string(),
                "p0/c0/03-result-grep".to_string(),
            ]
        );
        let call = tree.read("p0/c0/00-call-read_file", usize::MAX).unwrap();
        assert!(call.starts_with("read_file\n"), "{call}");
        let result = tree.read("p0/c0/01-result-read_file", usize::MAX).unwrap();
        assert!(result.starts_with("read_file\n"), "{result}");
        assert!(result.contains("fn objects"), "{result}");
        assert!(tree.children("p0/c0/00-call-read_file").unwrap().is_empty());
        assert_eq!(tree.parent("p0/c0").as_deref(), Some("p0"));
        assert_eq!(
            tree.parent("p0/c0/00-call-read_file").as_deref(),
            Some("p0/c0")
        );
        assert_eq!(tree.parent(""), None);
    }

    #[test]
    fn a_long_prompt_is_excerpted_on_the_loop() {
        let mut text = "HEAD-START ".to_string();
        while text.len() <= LOOP_PROMPT_EXCERPT {
            text.push_str("padding ");
        }
        text.push_str("TAIL-END");
        let user = serde_json::json!({
            "type": "user",
            "prompt_index": 0,
            "content": [{"type": "text", "text": format!("<user_query>\n{text}\n</user_query>")}],
        });
        let assistant = serde_json::json!({
            "type": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "c1",
                "name": "read_file",
                "arguments": "{\"target_file\":\"src/lib.rs\"}",
            }],
        });
        let result = serde_json::json!({
            "type": "tool_result",
            "tool_call_id": "c1",
            "content": "fn main",
        });
        let jsonl = format!("{user}\n{assistant}\n{result}\n");
        let tree = Transcript::parse(&jsonl).unwrap();
        let prompt = tree.summary("p0").unwrap().unwrap();
        assert!(prompt.contains("HEAD-START"), "{prompt}");
        assert!(prompt.contains("TAIL-END"), "{prompt}");
        let cycle = tree.summary("p0/c0").unwrap().unwrap();
        assert!(cycle.contains("HEAD-START"), "{cycle}");
        assert!(cycle.contains('…'), "{cycle}");
        assert!(!cycle.contains("TAIL-END"), "{cycle}");
        assert!(cycle.contains("read_file src/lib.rs"), "{cycle}");
    }

    #[test]
    fn set_narrows_the_window_and_leaves_the_tree() {
        let tree = transcript();
        let mut window = tree.clone();
        let children = tree.children("p0/c0").unwrap();
        window.set(vec!["p0/c1".to_string()]).unwrap();
        assert_eq!(tree.databases(), &["p0".to_string()]);
        assert_eq!(window.databases(), &["p0/c1".to_string()]);
        assert_eq!(window.children("p0/c0").unwrap(), children);
    }

    #[test]
    fn a_text_reply_is_a_message_leaf() {
        let jsonl = r#"
{"type":"user","prompt_index":0,"content":"Say hello"}
{"type":"assistant","content":"Hello from the agent."}
"#;
        let tree = Transcript::parse(jsonl).unwrap();
        assert!(tree.summary("p0/c0/00-message").unwrap().is_none());
        let body = tree.read("p0/c0/00-message", usize::MAX).unwrap();
        assert_eq!(body, "Hello from the agent.");
        assert!(tree
            .summary("p0/c0")
            .unwrap()
            .unwrap()
            .contains("Hello from the agent."));
    }

    #[test]
    fn open_reads_chat_history_from_a_directory() {
        let dir = std::env::temp_dir().join(format!(
            "autoground-transcript-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chat_history.jsonl"), FIXTURE).unwrap();
        let tree = Transcript::open(&dir).unwrap();
        assert!(tree
            .summary("p0")
            .unwrap()
            .unwrap()
            .contains("Where is the universe trait?"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
