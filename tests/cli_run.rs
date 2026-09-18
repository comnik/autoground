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
fn run_prints_header_requires_and_trace_path() {
    let (out, err, code) = stdout(&[
        "run",
        "examples/layered.ag",
        "--repo",
        "examples/toy-layered",
        "--judge",
        "heuristic",
    ]);
    assert_eq!(err, "", "{out}");
    assert_eq!(code, 0);
    assert!(out.starts_with("GROUNDED\n"), "{out}");
    assert!(out.contains("jev requests: "), "{out}");
    assert!(!out.contains(" ← "), "{out}");
    assert!(
        out.contains("(require\n  (and\n    (component \"Frontend\")"),
        "{out}"
    );
    assert!(out.contains("(component \"Frontend\")"), "{out}");
    assert!(!out.contains("proved"), "{out}");
    assert!(!out.contains("disproved"), "{out}");
    assert!(
        out.contains("(require\n  (depends_on \"Frontend\" \"Backend\"))"),
        "{out}"
    );
    assert!(out.contains("(includes \"frontend/app.py\""), "{out}");
    assert!(
        out.contains("(belongs_to \"frontend/app.py\" \"Frontend\")"),
        "{out}"
    );
    let path = trace_path(&out);
    assert!(
        path.starts_with("/tmp/autoground/trace-"),
        "unexpected trace path {path}"
    );
    assert!(path.ends_with(".json"), "{path}");
    assert!(
        out.contains(&format!("trace: {path}\njev requests: ")),
        "{out}"
    );
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"listings\""), "{body}");
    assert!(body.contains("\"documents\""), "{body}");
    let started = json_u64(&body, "started_at_ms");
    let ended = json_u64(&body, "ended_at_ms");
    assert!(ended >= started, "{body}");
    assert!(body.contains("\"outcome\": \"grounded\""), "{body}");
    assert!(body.contains("\"steps\""), "{body}");
    assert!(body.contains("\"requires\""), "{body}");
    assert!(body.contains("\"repo_name\""), "{body}");
}

fn json_u64(body: &str, key: &str) -> u64 {
    let prefix = format!("\"{key}\": ");
    let start = body
        .find(&prefix)
        .unwrap_or_else(|| panic!("missing {key} in {body}"))
        + prefix.len();
    let digits: String = body[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("bad {key} in {body}"))
}

#[test]
fn run_defaults_repo_to_cwd() {
    let out = bin()
        .current_dir("examples/toy-layered")
        .args(["run", "../layered.ag", "--judge", "heuristic"])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().unwrap_or(1);
    assert_eq!(stderr.as_ref(), "", "{stdout}");
    assert_eq!(code, 0);
    assert!(stdout.starts_with("GROUNDED\n"), "{stdout}");
}

#[test]
fn run_accepts_inline_model_source() {
    let (out, err, code) = stdout(&[
        "run",
        r#"(require (component "Frontend"))"#,
        "--repo",
        "examples/toy-layered",
        "--judge",
        "heuristic",
    ]);
    assert_eq!(err, "", "{out}");
    assert_eq!(code, 0);
    assert!(out.contains("GROUNDED"), "{out}");
    assert!(out.contains("trace: /tmp/autoground/"), "{out}");
}

#[test]
fn run_defaults_to_jev_judge() {
    let out = bin()
        .env_remove("TYPESAFE_API_KEY")
        .args([
            "run",
            r#"(require (component "Frontend"))"#,
            "--repo",
            "examples/toy-layered",
        ])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().unwrap_or(1);
    assert_eq!(code, 3, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("TYPESAFE_API_KEY"), "{stderr}");
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
fn inspect_rerenders_a_recorded_run() {
    let (out, err, code) = stdout(&[
        "run",
        "examples/layered.ag",
        "--repo",
        "examples/toy-layered",
        "--judge",
        "heuristic",
    ]);
    assert_eq!(err, "", "{out}");
    assert_eq!(code, 0);
    let path = trace_path(&out).to_string();
    let (again, err2, code2) = stdout(&["run", "--inspect", &path]);
    assert_eq!(err2, "", "{again}");
    assert_eq!(code2, code);
    assert_eq!(again, out);
}

#[test]
fn inspect_fixture_without_run_result_errors() {
    let (out, err, code) = stdout(&["run", "--inspect", "tests/fixtures/toy-layered.json"]);
    assert_eq!(code, 3);
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("not a full run trace") || err.contains("judge fixture"),
        "{err}"
    );
}

#[test]
fn inspect_missing_file_errors() {
    let (out, err, code) = stdout(&["run", "--inspect", "tests/fixtures/does-not-exist.json"]);
    assert_eq!(code, 3);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("could not read trace"), "{err}");
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
