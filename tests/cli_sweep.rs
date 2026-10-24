use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autoground"))
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoground-cli-sweep-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sweep_rejects_a_brief_without_a_dollar_before_the_oracle() {
    let dir = scratch("dollar");
    let brief = dir.join("model.brief");
    std::fs::write(&brief, "(claim (\"alpha\") (witness \"note.txt\"))\n").unwrap();
    std::fs::write(dir.join("note.txt"), "alpha\n").unwrap();
    let out = bin()
        .current_dir(&dir)
        .args(["sweep", "model.brief"])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("a sweep brief needs `(witness $)`"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("TYPESAFE_API_KEY"),
        "{stderr}"
    );
}
