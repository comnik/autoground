use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

#[test]
fn heuristic_read_of_frontend_is_true_and_stops() {
    let out = bin()
        .args([
            "read",
            "frontend",
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
    assert!(stdout.starts_with("true\n"), "{stdout}");
    assert!(stdout.contains("frontend/__init__.py\n"), "{stdout}");
    assert!(stdout.contains("\nopen\n"), "{stdout}");
    assert!(!stdout.contains("app.py"), "{stdout}");
    assert!(!stdout.contains("api.py"), "{stdout}");
    assert!(!stdout.contains("noise.py"), "{stdout}");
    let path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("trace: "))
        .expect("trace path");
    assert!(path.starts_with("/tmp/autoground/trace-"), "{path}");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"kind\":\"read\""), "{body}");
    assert!(body.contains("frontend/__init__.py"), "{body}");

    let again = bin()
        .args(["read", "--inspect", path, "--full"])
        .output()
        .expect("inspect");
    let again_out = String::from_utf8_lossy(&again.stdout);
    assert_eq!(again.status.code(), Some(0), "{again_out}");
    assert!(again_out.starts_with("true\n"), "{again_out}");
    assert!(
        again_out.contains("frontend/__init__.py  entailed"),
        "{again_out}"
    );
}

#[test]
fn heuristic_read_of_an_unmatched_proposition_is_false() {
    let out = bin()
        .args([
            "read",
            "zzzz-not-a-path",
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
    assert!(stdout.starts_with("false\n"), "{stdout}");
    assert!(stdout.contains("\nexhausted\n"), "{stdout}");
    assert!(!stdout.contains("app.py"), "{stdout}");
    assert!(!stdout.contains("api.py"), "{stdout}");
}
