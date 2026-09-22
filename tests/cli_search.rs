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
    assert!(stdout.contains("[TRUE]"), "{stdout}");
    assert!(stdout.contains("requests=0"), "{stdout}");
    assert!(stdout.contains("true    frontend/"), "{stdout}");
    assert!(!stdout.contains("decoy"), "{stdout}");
    assert!(!stdout.contains("backend"), "{stdout}");
    let path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("trace: "))
        .expect("trace path");
    assert!(path.starts_with("/tmp/autoground/trace-"), "{path}");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"kind\": \"search\""), "{body}");
    assert!(!body.contains("from backend import api"), "{body}");
    assert!(body.contains("frontend/app.py"), "{body}");

    let again = bin()
        .args(["search", "--inspect", path])
        .output()
        .expect("inspect");
    let again_out = String::from_utf8_lossy(&again.stdout);
    assert_eq!(again.status.code(), Some(0), "{again_out}");
    assert!(again_out.contains("[TRUE]"), "{again_out}");
    assert!(again_out.contains("frontend/"), "{again_out}");
}
