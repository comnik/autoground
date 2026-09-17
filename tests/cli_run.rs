use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

fn stdout(args: &[&str]) -> (String, String, i32) {
    let out = bin().args(args).output().expect("run autoground");
    let code = out.status.code().unwrap_or(1);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        code,
    )
}

fn trace_path(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("trace: "))
        .expect("trace path")
}

#[test]
fn run_prints_outcome_bindings_and_trace_path() {
    let (out, err, code) = stdout(&[
        "run",
        "examples/layered.ag",
        "--repo",
        "examples/toy-layered",
    ]);
    assert_eq!(err, "", "{out}");
    assert_eq!(code, 0);
    assert!(out.contains("GROUNDED"), "{out}");
    assert!(out.contains("\"Frontend\""), "{out}");
    assert!(out.contains("require 0: true"), "{out}");
    let path = trace_path(&out);
    assert!(
        path.starts_with("/tmp/autoground/trace-"),
        "unexpected trace path {path}"
    );
    assert!(path.ends_with(".json"), "{path}");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"listings\""), "{body}");
    assert!(body.contains("\"documents\""), "{body}");
}

#[test]
fn run_accepts_inline_model_source() {
    let (out, err, code) = stdout(&[
        "run",
        r#"(require (component "Frontend"))"#,
        "--repo",
        "examples/toy-layered",
    ]);
    assert_eq!(err, "", "{out}");
    assert_eq!(code, 0);
    assert!(out.contains("GROUNDED"), "{out}");
    assert!(out.contains("trace: /tmp/autoground/"), "{out}");
}

#[test]
fn run_missing_ag_file_errors() {
    let (out, err, code) = stdout(&["run", "examples/does-not-exist.ag"]);
    assert_eq!(code, 3);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("model file not found"), "{err}");
}

#[test]
fn ground_subcommand_is_gone() {
    let (out, err, code) = stdout(&["ground", "--help"]);
    assert_ne!(code, 0);
    let text = format!("{out}{err}");
    assert!(
        text.contains("unrecognized subcommand") || text.contains("error"),
        "{text}"
    );
}

#[test]
fn record_flag_is_gone() {
    let (out, err, code) = stdout(&[
        "run",
        "examples/layered.ag",
        "--record",
        "/tmp/autoground/nope.json",
    ]);
    assert_ne!(code, 0);
    let text = format!("{out}{err}");
    assert!(
        text.contains("unexpected argument") || text.contains("error"),
        "{text}"
    );
}
