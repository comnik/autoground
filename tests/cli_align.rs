use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_autoground"));
    command.env_remove("TYPESAFE_API_KEY");
    command
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoground-cli-align-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn align_accepts_a_brief_markdown_and_a_flowchart() {
    let dir = scratch("same");
    let brief = "\
(name Frontend)
(name Backend)
(sense depends_on \"{X} depends on {Y}\")
(depends_on Frontend Backend)
(not (depends_on Backend Frontend))
";
    let brief_path = dir.join("model.brief");
    std::fs::write(&brief_path, brief).unwrap();
    let markdown = dir.join("notes.md");
    std::fs::write(&markdown, format!("Prose.\n\n```brief\n{brief}```\n")).unwrap();
    let picture = dir.join("picture.mmd");
    std::fs::write(
        &picture,
        "\
%% (sense depends_on \"{X} depends on {Y}\")
flowchart LR
  Frontend -->|depends_on| Backend
  Backend -->|not depends_on| Frontend
",
    )
    .unwrap();

    let same = bin()
        .args([
            "align",
            brief_path.to_str().unwrap(),
            brief_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(0), "{}", text(&same));
    assert_eq!(String::from_utf8_lossy(&same.stdout), "");

    let md = bin()
        .args([
            "align",
            brief_path.to_str().unwrap(),
            markdown.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(md.status.code(), Some(0), "{}", text(&md));

    let mmd = bin()
        .args([
            "align",
            brief_path.to_str().unwrap(),
            picture.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(mmd.status.code(), Some(0), "{}", text(&mmd));
}

#[test]
fn align_names_a_one_sided_statement() {
    let dir = scratch("differ");
    let left = dir.join("left.brief");
    let right = dir.join("right.brief");
    std::fs::write(&left, "(\"alpha\")\n").unwrap();
    std::fs::write(&right, "(\"beta\")\n").unwrap();
    let out = bin()
        .args(["align", left.to_str().unwrap(), right.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(stdout.contains("only in left"), "{stdout}");
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("only in right"), "{stdout}");
    assert!(stdout.contains("beta"), "{stdout}");
}

#[test]
fn align_rejects_an_unknown_diagram_line() {
    let dir = scratch("bad");
    let brief = dir.join("model.brief");
    let picture = dir.join("picture.mmd");
    std::fs::write(&brief, "(name Frontend)\n").unwrap();
    std::fs::write(&picture, "flowchart LR\n  click Frontend cb\n").unwrap();
    let out = bin()
        .args(["align", brief.to_str().unwrap(), picture.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    assert!(stderr.contains("click Frontend cb"), "{stderr}");
}

fn text(out: &std::process::Output) -> String {
    format!(
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
