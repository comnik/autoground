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
fn sweep_rejects_a_witness_before_the_oracle() {
    let dir = scratch("witness");
    let lect = dir.join("model.lect");
    std::fs::write(&lect, "(witness (\"alpha\") \"note.txt\")\n").unwrap();
    let out = bin()
        .current_dir(&dir)
        .args(["sweep", "model.lect"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!out.status.success(), "{stderr}");
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("`witness` is not a sweep constraint"),
        "{stderr}"
    );
}
