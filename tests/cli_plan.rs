use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

#[test]
fn plan_lists_each_sentence_in_diagram() {
    let out = bin()
        .args(["plan", "examples/diagram.ag"])
        .output()
        .expect("run autoground");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={stderr} stdout={stdout}"
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec![
            "(\"Frontend depends on Backend\")",
            "(\"Backend depends on Frontend\")",
            "(\"Oracle calls TypeSafe\")",
            "(\"Oracle calls the TypeSafe API\")",
            "(\"Frontend is a component of the system\")",
            "(\"Backend is a component of the system\")",
            "(\"Frontend calls Backend\")",
        ]
    );
}

#[test]
fn plan_rejects_an_undeclared_name() {
    let out = bin()
        .args(["plan", "(component Frontend)"])
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
fn heuristic_run_combines_and_or_and_not() {
    let model = r#"
(not ("there is a GPU compiler"))
(and ("Frontend is part of the system") (not ("there is a GPU compiler")))
(or ("there is a GPU compiler") ("Backend is part of the system"))
(and ("there is a GPU compiler") ("Frontend is part of the system"))
"#;
    let out = bin()
        .args([
            "run",
            model,
            "--repo",
            "examples/toy-layered",
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
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(
        lines[..4],
        [
            "true     (not (\"there is a GPU compiler\"))",
            "true     (and (\"Frontend is part of the system\") (not (\"there is a GPU compiler\")))",
            "true     (or (\"there is a GPU compiler\") (\"Backend is part of the system\"))",
            "false    (and (\"there is a GPU compiler\") (\"Frontend is part of the system\"))",
        ]
    );
    assert!(lines[4].starts_with("(requests="), "{stdout}");
    assert!(
        !lines[4].contains("p50="),
        "percentiles belong on the latency chart, got {}",
        lines[4]
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("3/4 claims validated in ")),
        "{stdout}"
    );
    let path = lines
        .iter()
        .find_map(|line| line.strip_prefix("trace: "))
        .expect("trace path");
    assert!(path.starts_with("/tmp/autoground/trace-"), "{path}");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"kind\":\"run\""), "{body}");
    assert!(body.contains("there is a GPU compiler"), "{body}");
}
