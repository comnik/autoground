use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoground-cli-check-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A one-line file and a witness that quotes it.
fn quoted_witness(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("note.txt"), "alpha is the first letter.\n").unwrap();
    let brief = dir.join("model.brief");
    std::fs::write(
        &brief,
        "(claim \"alpha is the first letter.\" (witness \"note.txt\"))\n",
    )
    .unwrap();
    brief
}

fn line<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("{prefix} missing from {stdout}"))
}

#[test]
fn check_discharges_a_quoted_witness() {
    let dir = scratch("quote");
    let brief = quoted_witness(&dir);
    let out = bin()
        .args(["check", brief.to_str().unwrap()])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={stderr} stdout={stdout}"
    );
    assert!(
        stdout.contains("true  alpha is the first letter."),
        "{stdout}"
    );
    assert!(stdout.contains("note.txt"), "{stdout}");
    let trace = line(&stdout, "trace available at ");
    assert!(
        trace.starts_with("/tmp/autoground/") && trace.ends_with("/trace.jsonl"),
        "{trace}"
    );
    assert_eq!(
        trace.trim_end_matches("/trace.jsonl").matches('/').count(),
        3,
        "{trace}"
    );
    let body = std::fs::read_to_string(trace).unwrap();
    assert!(body.contains("\"kind\":\"check\""), "{body}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("1/1 claims validated in ")),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_exits_nonzero_when_a_claim_is_undischarged() {
    let dir = scratch("open");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "\"nothing cites this\"\n").unwrap();
    let out = bin()
        .args(["check", brief.to_str().unwrap()])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr={stderr} stdout={stdout}"
    );
    assert!(stdout.contains(";; unpinned"), "{stdout}");
    let trace = line(&stdout, "trace available at ");
    assert!(
        trace.starts_with("/tmp/autoground/") && trace.ends_with("/trace.jsonl"),
        "{trace}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("0/1 claims validated in ")),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_out_writes_the_session_there() {
    let dir = scratch("out");
    let brief = quoted_witness(&dir);
    let out_dir = dir.join("sessions");
    let out = bin()
        .args([
            "check",
            brief.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
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
    let trace = PathBuf::from(line(&stdout, "trace available at "));
    let session = trace.parent().expect("session directory");
    assert_eq!(
        session.parent(),
        Some(out_dir.as_path()),
        "{}",
        trace.display()
    );
    assert_eq!(
        trace.file_name().and_then(|name| name.to_str()),
        Some("trace.jsonl")
    );
    assert!(std::fs::read_to_string(&trace)
        .unwrap()
        .contains("\"kind\":\"check\""));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_rejects_an_undeclared_name() {
    let out = bin()
        .args(["check", "(component Frontend)"])
        .output()
        .expect("run autoground");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("`Frontend` is not a declared name"),
        "{stderr}"
    );
}

#[test]
fn check_reads_brief_blocks_from_markdown() {
    let dir = scratch("md");
    let path = dir.join("notes.md");
    std::fs::write(
        &path,
        "# notes\n\n```brief\n\"Frontend is a component\"\n```\n",
    )
    .unwrap();
    let out = bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr={stderr} stdout={stdout}"
    );
    assert!(stdout.contains("Frontend is a component"), "{stdout}");
    assert!(stdout.contains(";; unpinned"), "{stdout}");
}

#[test]
fn check_rejects_markdown_without_brief_blocks() {
    let dir = scratch("md-empty");
    let path = dir.join("notes.md");
    std::fs::write(&path, "# notes\n\n```rust\nfn main() {}\n```\n").unwrap();
    let out = bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .expect("run autoground");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("no brief blocks"), "{stderr}");
}

#[test]
fn check_rejects_a_dollar_without_a_file() {
    let dir = scratch("dollar");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "(claim \"alpha\" (witness $))\n").unwrap();
    let out = bin()
        .args(["check", brief.to_str().unwrap()])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("`$` needs a file"), "{stderr}");
    assert!(!stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
}

#[test]
fn check_dash_reads_stdin_before_the_oracle() {
    let dir = scratch("stdin");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "(claim \"alpha\" (witness $))\n").unwrap();
    let mut child = bin()
        .args(["check", brief.to_str().unwrap(), "-"])
        .env_remove("TYPESAFE_API_KEY")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"alpha\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
    assert!(!stderr.contains("`$` needs a file"), "{stderr}");
}

#[test]
fn check_dash_without_a_dollar_does_not_call_the_oracle() {
    let dir = scratch("stdin-plain");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "(claim \"alpha\" (witness \"note.txt\"))\n").unwrap();
    let out = bin()
        .args(["check", brief.to_str().unwrap(), "-"])
        .env_remove("TYPESAFE_API_KEY")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("a file needs `(witness $)`"), "{stderr}");
    assert!(!stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
}

#[test]
fn check_rejects_a_file_without_a_dollar() {
    let dir = scratch("file");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "(claim \"alpha\" (witness \"note.txt\"))\n").unwrap();
    let note = dir.join("note.txt");
    std::fs::write(&note, "alpha\n").unwrap();
    let out = bin()
        .args(["check", brief.to_str().unwrap(), note.to_str().unwrap()])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("a file needs `(witness $)`"), "{stderr}");
    assert!(!stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
}
