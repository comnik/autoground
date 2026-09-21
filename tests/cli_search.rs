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
    assert!(stdout.contains("frontend/"), "{stdout}");
    assert!(!stdout.contains("decoy"), "{stdout}");
    assert!(!stdout.contains("backend"), "{stdout}");
    assert!(stderr.contains("requests=0"), "{stderr}");
}
