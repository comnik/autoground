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
        "(witness (\"alpha is the first letter.\") \"note.txt\")\n",
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
    let context = std::fs::read_to_string(line(&stdout, "context available at ")).unwrap();
    assert_eq!(
        context,
        "\
alpha is the first letter.
(see note.txt)
"
    );
    let diagram = std::fs::read_to_string(line(&stdout, "diagram available at ")).unwrap();
    assert!(diagram.starts_with("flowchart TD\n"), "{diagram}");
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
    std::fs::write(&brief, "(\"nothing cites this\")\n").unwrap();
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
    let context = PathBuf::from(line(&stdout, "context available at "));
    let diagram = PathBuf::from(line(&stdout, "diagram available at "));
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
    assert_eq!(context, session.join("context.md"));
    assert_eq!(diagram, session.join("diagram.mmd"));
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
        "# notes\n\n```brief\n(\"Frontend is a component\")\n```\n",
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
