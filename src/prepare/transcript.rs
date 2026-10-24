//! One file per tool call in a `chat_history.jsonl`.
//!
//! `path` is that file, or a directory that contains it. System text,
//! synthetic reminders, and user-role context that is not a turn are omitted.
//! A `<user_query>` with no `prompt_index` is still a turn. Each result is
//! paired with its call by id.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

pub struct Exchange {
    pub id: String,
    pub body: String,
}

/// Read `chat_history.jsonl`, or that file inside a directory.
pub fn exchanges(path: &Path) -> Result<Vec<Exchange>, String> {
    let file = if path.is_dir() {
        path.join("chat_history.jsonl")
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|err| format!("reading {}: {err}", file.display()))?;
    parse(&text)
}

pub fn parse(jsonl: &str) -> Result<Vec<Exchange>, String> {
    let prompts = read_prompts(jsonl)?;
    if prompts.is_empty() {
        return Err("transcript has no user prompts".into());
    }
    Ok(files_of(&prompts))
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

fn files_of(prompts: &[PromptDraft]) -> Vec<Exchange> {
    let mut out = Vec::new();
    for (prompt_index, prompt) in prompts.iter().enumerate() {
        for (cycle_index, cycle) in prompt.cycles.iter().enumerate() {
            let later = later_reaction(prompt, cycle_index);
            let prefix = format!("p{prompt_index}/c{cycle_index}");
            if cycle.calls.is_empty() {
                let reaction = cycle.text.trim();
                if reaction.is_empty() {
                    continue;
                }
                out.push(Exchange {
                    id: format!("{prefix}/message"),
                    body: format!("prompt:\n{}\n\nreaction:\n{reaction}\n", prompt.text.trim()),
                });
                continue;
            }
            let issuing = cycle.text.trim();
            for (index, call) in cycle.calls.iter().enumerate() {
                let reaction = later.as_deref().unwrap_or(issuing);
                let show_issuing = later.is_some() && !issuing.is_empty();
                let result = cycle
                    .results
                    .get(&call.id)
                    .map(String::as_str)
                    .unwrap_or("");
                out.push(Exchange {
                    id: format!("{prefix}/{index:02}-{}", tool_slug(&call.name)),
                    body: format!(
                        "prompt:\n{}\n\ntool:\n{}\n\nresult:\n{result}\n\nreaction:\n{reaction}\n",
                        prompt.text.trim(),
                        tool_section(issuing, show_issuing, &call.name, &call.arguments)
                    ),
                });
            }
        }
    }
    out
}

/// The first non-empty assistant text in a later cycle of this prompt.
fn later_reaction(prompt: &PromptDraft, cycle_index: usize) -> Option<String> {
    prompt
        .cycles
        .iter()
        .skip(cycle_index + 1)
        .find_map(|cycle| {
            let text = cycle.text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
}

fn tool_section(issuing: &str, show_issuing: bool, name: &str, arguments: &str) -> String {
    let mut out = String::new();
    if show_issuing {
        out.push_str(issuing);
        out.push('\n');
    }
    out.push_str(name);
    let pretty = pretty_arguments(arguments);
    if !pretty.is_empty() {
        out.push('\n');
        out.push_str(&pretty);
    }
    out
}

fn pretty_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(value) if value.is_object() => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| arguments.to_string())
        }
        _ => arguments.to_string(),
    }
}

fn read_prompts(jsonl: &str) -> Result<Vec<PromptDraft>, String> {
    let mut prompts: Vec<PromptDraft> = Vec::new();
    for (number, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).map_err(|err| format!("line {}: {err}", number + 1))?;
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
                let raw = json_text(value.get("content").unwrap_or(&Value::Null));
                let text = user_prompt(&raw);
                if text.is_empty() {
                    continue;
                }
                let indexed = value.get("prompt_index").and_then(Value::as_u64).is_some();
                if !indexed && !is_user_turn(&raw) {
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

/// A `<user_query>` is a turn even when compaction omitted `prompt_index`.
/// Other user-role text is context.
fn is_user_turn(raw: &str) -> bool {
    raw.contains("<user_query>")
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

    fn body<'a>(files: &'a [Exchange], id: &str) -> &'a str {
        files
            .iter()
            .find(|file| file.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .body
            .as_str()
    }

    #[test]
    fn a_call_carries_the_prompt_the_result_and_its_own_text() {
        let files = parse(FIXTURE).unwrap();
        let ids: Vec<_> = files.iter().map(|file| file.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["p0/c0/00-read_file", "p0/c0/01-grep", "p0/c1/00-list_dir",]
        );
        let call = body(&files, "p0/c0/00-read_file");
        assert!(
            call.contains("prompt:\nWhere is the universe trait?"),
            "{call}"
        );
        assert!(call.contains("read_file"), "{call}");
        assert!(call.contains("src/universe/mod.rs"), "{call}");
        assert!(call.contains("fn objects"), "{call}");
        assert!(call.contains("reaction:\nI'll look."), "{call}");
        assert!(!call.contains("SECRET_SYSTEM"), "{call}");
        assert!(!call.contains("SECRET_CONTEXT"), "{call}");
        assert!(!call.contains("SECRET_REMINDER"), "{call}");
        let grep = body(&files, "p0/c0/01-grep");
        assert!(grep.contains("trait Universe"), "{grep}");
        assert!(grep.contains("pub trait Universe"), "{grep}");
        assert!(grep.contains("reaction:\nI'll look."), "{grep}");
    }

    #[test]
    fn a_later_reaction_is_shared_and_the_issuing_text_stays_with_the_tool() {
        let jsonl = r#"
{"type":"system","content":"SECRET_SYSTEM"}
{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nFind alpha\n</user_query>"}]}
{"type":"assistant","content":"I'll look.","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"a.txt\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"alpha is here"}
{"type":"assistant","content":"Found it."}
"#;
        let files = parse(jsonl).unwrap();
        assert_eq!(files.len(), 2);
        let call = body(&files, "p0/c0/00-read_file");
        assert!(call.contains("prompt:\nFind alpha\n"), "{call}");
        assert!(call.contains("I'll look.\nread_file\n"), "{call}");
        assert!(call.contains("result:\nalpha is here\n"), "{call}");
        assert!(call.contains("reaction:\nFound it.\n"), "{call}");
        assert!(!call.contains("SECRET_SYSTEM"), "{call}");
        let message = body(&files, "p0/c1/message");
        assert!(message.contains("prompt:\nFind alpha\n"), "{message}");
        assert!(message.contains("reaction:\nFound it.\n"), "{message}");
        assert!(!message.contains("tool:"), "{message}");
    }

    #[test]
    fn a_text_reply_is_a_message_file() {
        let jsonl = r#"
{"type":"user","prompt_index":0,"content":"Say hello"}
{"type":"assistant","content":"Hello from the agent."}
"#;
        let files = parse(jsonl).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, "p0/c0/message");
        assert_eq!(
            files[0].body,
            "prompt:\nSay hello\n\nreaction:\nHello from the agent.\n"
        );
    }

    #[test]
    fn a_replayed_query_keeps_the_tool_calls_that_follow_it() {
        let jsonl = r#"
{"type":"user","synthetic_reason":"compaction_meta","content":[{"type":"text","text":"<user_info>\nSECRET_INFO\n</user_info>"}]}
{"type":"user","content":[{"type":"text","text":"<user_query>\nPrepare the image witness.\n</user_query>"}]}
{"type":"user","synthetic_reason":"compaction_meta","content":"SECRET_SUMMARY mentions src/check/mod.rs"}
{"type":"user","content":[{"type":"text","text":"<user_info>\nOS Version: macos\n</user_info>"}]}
{"type":"assistant","content":"","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"src/check/mod.rs\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"fn check"}
{"type":"user","prompt_index":9,"content":[{"type":"text","text":"<user_query>\nWhat is autocfg?\n</user_query>"}]}
{"type":"assistant","content":"","tool_calls":[{"id":"c2","name":"read_file","arguments":"{\"target_file\":\"Cargo.toml\"}"}]}
{"type":"tool_result","tool_call_id":"c2","content":"[package]"}
"#;
        let files = parse(jsonl).unwrap();
        let ids: Vec<_> = files.iter().map(|file| file.id.as_str()).collect();
        assert_eq!(ids, vec!["p0/c0/00-read_file", "p1/c0/00-read_file"]);
        let first = body(&files, "p0/c0/00-read_file");
        assert!(
            first.contains("prompt:\nPrepare the image witness."),
            "{first}"
        );
        assert!(first.contains("src/check/mod.rs"), "{first}");
        assert!(!first.contains("SECRET_INFO"), "{first}");
        assert!(!first.contains("SECRET_SUMMARY"), "{first}");
        assert!(!first.contains("OS Version"), "{first}");
        let second = body(&files, "p1/c0/00-read_file");
        assert!(second.contains("prompt:\nWhat is autocfg?"), "{second}");
        assert!(second.contains("Cargo.toml"), "{second}");
    }

    #[test]
    fn open_reads_chat_history_from_a_directory() {
        let dir = std::env::temp_dir().join(format!(
            "autoground-transcript-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chat_history.jsonl"), FIXTURE).unwrap();
        let files = exchanges(&dir).unwrap();
        assert!(body(&files, "p0/c0/00-read_file").contains("Where is the universe trait?"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
