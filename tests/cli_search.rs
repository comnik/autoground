use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

#[test]
fn heuristic_search_keeps_the_named_directory() {
    let out = bin()
        .args([
            "search",
            "frontend",
            "--repo",
            "examples/toy-layered",
            "--judge",
            "heuristic",
        ])
        .output()
        .expect("run autoground");
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    assert!(stdout.contains("requests=0"), "{stdout}");
    let smaller = stdout.find("% smaller").expect("reduction");
    let usage_at = stdout.find("requests=0").unwrap();
    let trace_at = stdout.find("trace: ").expect("trace");
    let tree_at = stdout.find("frontend/\n").unwrap();
    assert!(
        smaller < usage_at && usage_at < trace_at && trace_at < tree_at,
        "{stdout}"
    );
    assert!(stdout.contains("frontend/\n"), "{stdout}");
    assert!(stdout.contains("  __init__.py\n"), "{stdout}");
    assert!(stdout.contains("  app.py\n"), "{stdout}");
    assert!(stdout.contains("backend/\n"), "{stdout}");
    assert!(stdout.contains("decoy/\n"), "{stdout}");
    assert!(!stdout.contains("api.py"), "{stdout}");
    assert!(!stdout.contains("noise.py"), "{stdout}");
    assert_eq!(stdout.matches("__init__.py").count(), 1, "{stdout}");
    let path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("trace: "))
        .expect("trace path");
    assert!(path.starts_with("/tmp/autoground/trace-"), "{path}");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"kind\":\"search\""), "{body}");
    assert!(!body.contains("from backend import api"), "{body}");
    assert!(body.contains("frontend/app.py"), "{body}");

    let again = bin()
        .args(["search", "--inspect", path])
        .output()
        .expect("inspect");
    let again_out = String::from_utf8_lossy(&again.stdout);
    assert_eq!(again.status.code(), Some(0), "{again_out}");
    assert!(again_out.contains("frontend/\n"), "{again_out}");
    assert!(again_out.contains("  app.py\n"), "{again_out}");
    assert!(again_out.contains("backend/\n"), "{again_out}");
    assert!(!again_out.contains("api.py"), "{again_out}");
}

#[test]
fn transcript_search_records_the_prompt() {
    let dir =
        std::env::temp_dir().join(format!("autoground-cli-transcript-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chat_history.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nWhere is the universe trait?\n</user_query>"}]}
{"type":"assistant","content":"","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"src/universe/mod.rs\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"pub trait Universe"}
"#,
    )
    .unwrap();
    let out = bin()
        .args([
            "search",
            "universe trait",
            "--transcript",
            path.to_str().unwrap(),
            "--judge",
            "heuristic",
        ])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={stderr} stdout={stdout}"
    );
    let trace = stdout
        .lines()
        .find_map(|line| line.strip_prefix("trace: "))
        .expect("trace path");
    let body = std::fs::read_to_string(trace).unwrap();
    assert!(body.contains("Where is the universe trait?"), "{body}");
    assert!(body.contains("read_file src/universe/mod.rs"), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
