use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoground-cli-prepare-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const TRANSCRIPT: &str = r#"{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nWhere is the universe trait?\n</user_query>"}]}
{"type":"assistant","content":"I'll look.","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"src/universe/mod.rs\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"pub trait Universe"}
{"type":"assistant","content":"It is in src/universe/mod.rs."}
"#;

#[test]
fn prepare_transcript_prints_a_directory_that_stays() {
    let dir = scratch("transcript");
    std::fs::write(dir.join("chat_history.jsonl"), TRANSCRIPT).unwrap();
    let out = bin()
        .args(["prepare", "transcript", dir.to_str().unwrap()])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let prepared = PathBuf::from(stdout.trim());
    assert!(prepared.is_dir(), "{stdout}");
    let body = std::fs::read_to_string(prepared.join("p0/c0/00-read_file")).unwrap();
    assert!(body.contains("Where is the universe trait?"), "{body}");
    let _ = std::fs::remove_dir_all(&prepared);
}

#[test]
fn prepare_diff_prints_the_patch_and_a_bad_revision_fails() {
    let dir = scratch("diff");
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "test"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.txt"), "old\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    std::fs::write(dir.join("src/a.txt"), "new\n").unwrap();

    let out = bin()
        .current_dir(&dir)
        .args(["prepare", "diff"])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let prepared = PathBuf::from(stdout.trim());
    let patch = std::fs::read_to_string(prepared.join("src/a.txt")).unwrap();
    assert!(patch.contains("+new"), "{patch}");
    let _ = std::fs::remove_dir_all(&prepared);

    let bad = bin()
        .current_dir(&dir)
        .args(["prepare", "diff", "not-a-rev"])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let bad_out = String::from_utf8(bad.stdout).unwrap();
    let bad_err = String::from_utf8(bad.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(bad.status.code(), Some(3), "{bad_err}");
    assert!(bad_out.trim().is_empty(), "{bad_out}");
    assert!(!bad_err.is_empty(), "{bad_err}");
}
