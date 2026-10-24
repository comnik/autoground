//! One patch file per change in `git diff`.
//!
//! The body includes the `diff --git` header. A rename or a copy uses the
//! destination path. A `.brief` path is omitted, and so is `hide` when it
//! lives under the repository.

use std::collections::HashSet;
use std::path::{Component, Path};
use std::process::Command;

pub struct Patch {
    pub id: String,
    pub body: String,
}

/// `git_args` are appended to `git diff`. An empty list is the worktree
/// against the index.
pub fn changes(
    root: &Path,
    git_args: impl IntoIterator<Item = impl AsRef<str>>,
    hide: Option<&Path>,
) -> Result<Vec<Patch>, String> {
    let bytes = run_git(root, git_args)?;
    let mut patches = parse_diff(&bytes)?;
    let model = hide.and_then(|path| model_rel(root, path));
    patches.retain(|patch| visible(&patch.id, model.as_deref()));
    let mut seen = HashSet::new();
    for patch in &patches {
        if !seen.insert(patch.id.clone()) {
            return Err(format!("git diff listed {} twice", patch.id));
        }
        guard_id(&patch.id)?;
    }
    Ok(patches)
}

fn guard_id(id: &str) -> Result<(), String> {
    let path = Path::new(id);
    if path.is_absolute() {
        return Err(format!("git diff path `{id}` is absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("git diff path `{id}` leaves the tree"));
    }
    Ok(())
}

/// Format flags precede the caller's arguments so a `--` and its pathspecs
/// stay pathspecs. Exit status 1 is git's "differences exist" code.
fn run_git(
    root: &Path,
    git_args: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-pager", "-c", "color.ui=never", "-C"])
        .arg(root)
        .args(["diff", "--raw", "-z", "-p", "--no-ext-diff", "--no-color"]);
    for arg in git_args {
        cmd.arg(arg.as_ref());
    }
    let output = cmd.output().map_err(|err| format!("git diff: {err}"))?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        Some(1) if !output.stdout.is_empty() => Ok(output.stdout),
        _ => {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            Err(if err.is_empty() {
                if output.status.code() == Some(1) {
                    "git diff produced no patch".to_string()
                } else {
                    format!("git diff exited {}", output.status)
                }
            } else {
                format!("git diff: {err}")
            })
        }
    }
}

/// `--raw -z -p` writes NUL-terminated records, then one patch per change.
/// A rename or a copy record carries the source path, then the destination.
fn parse_diff(bytes: &[u8]) -> Result<Vec<Patch>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.starts_with(b":") && !bytes.starts_with(b"diff --") {
        return Err("git diff did not return a patch".into());
    }
    let mut pos = 0usize;
    let mut ids = Vec::new();
    while pos < bytes.len() {
        if bytes[pos..].starts_with(b"diff --") {
            break;
        }
        let field = cstr(bytes, &mut pos)?;
        if field.is_empty() {
            continue;
        }
        if !field.starts_with(b":") {
            return Err("git diff did not return a raw inventory".into());
        }
        let meta = std::str::from_utf8(field)
            .map_err(|_| "git diff: raw record is not utf-8".to_string())?;
        let status = meta.split(' ').next_back().unwrap_or("");
        let paths = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        let mut names = Vec::with_capacity(paths);
        for _ in 0..paths {
            let path = cstr(bytes, &mut pos)?;
            let path = std::str::from_utf8(path)
                .map_err(|_| "git diff: path is not utf-8".to_string())?;
            if path.is_empty() {
                return Err("git diff: empty path".into());
            }
            names.push(path.to_string());
        }
        ids.push(names.pop().expect("at least one path"));
    }
    let patch = String::from_utf8_lossy(&bytes[pos..]).into_owned();
    let bodies = split_patches(&patch);
    if ids.len() != bodies.len() {
        return Err(format!(
            "git diff listed {} changes and returned {} patches",
            ids.len(),
            bodies.len()
        ));
    }
    Ok(ids
        .into_iter()
        .zip(bodies)
        .map(|(id, body)| Patch { id, body })
        .collect())
}

fn cstr<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<&'a [u8], String> {
    let rest = &bytes[*pos..];
    let Some(index) = rest.iter().position(|byte| *byte == 0) else {
        return Err("git diff: truncated record".into());
    };
    *pos += index + 1;
    Ok(&rest[..index])
}

fn split_patches(patch: &str) -> Vec<String> {
    if patch.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in patch.split_inclusive('\n') {
        let header = line.starts_with("diff --git ")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ");
        if header && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn visible(id: &str, model: Option<&str>) -> bool {
    let name = id.rsplit('/').next().unwrap_or(id);
    if name.ends_with(".brief") {
        return false;
    }
    model != Some(id)
}

fn model_rel(root: &Path, model: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let model = model.canonicalize().ok()?;
    let rel = model.strip_prefix(&root).ok()?;
    let text = rel.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "autoground-diff-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git_text(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(git(dir, args).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn init(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", message]);
    }

    fn ids(patches: &[Patch]) -> Vec<String> {
        let mut ids: Vec<_> = patches.iter().map(|patch| patch.id.clone()).collect();
        ids.sort();
        ids
    }

    fn body<'a>(patches: &'a [Patch], id: &str) -> &'a str {
        patches
            .iter()
            .find(|patch| patch.id == id)
            .unwrap()
            .body
            .as_str()
    }

    #[test]
    fn a_worktree_diff_is_one_patch_per_change() {
        let dir = scratch("worktree");
        init(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "old line\n").unwrap();
        std::fs::write(dir.join("keep.txt"), "keep\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("oldname.txt"), "plain\n").unwrap();
        std::fs::write(dir.join("my file.txt"), "space\n").unwrap();
        std::fs::write(dir.join("model.brief"), "(name X)\n").unwrap();
        std::fs::write(dir.join("notes.md"), "old notes\n").unwrap();
        commit(&dir, "init");

        std::fs::write(dir.join("keep.txt"), "staged\n").unwrap();
        git(&dir, &["add", "keep.txt"]);
        git(&dir, &["mv", "oldname.txt", "new name.txt"]);
        std::fs::write(dir.join("src/a.txt"), "new line\n").unwrap();
        std::fs::write(dir.join("my file.txt"), "spaced\n").unwrap();
        std::fs::write(dir.join("model.brief"), "(name Y)\n").unwrap();
        std::fs::write(dir.join("notes.md"), "new notes\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("extra.txt"), "untracked\n").unwrap();

        let diff = changes(&dir, Vec::<&str>::new(), None).unwrap();
        assert_eq!(
            ids(&diff),
            vec![
                "gone.txt".to_string(),
                "my file.txt".to_string(),
                "notes.md".to_string(),
                "src/a.txt".to_string(),
            ]
        );
        let patch = body(&diff, "src/a.txt");
        assert!(patch.contains("diff --git"), "{patch}");
        assert!(patch.contains("-old line"), "{patch}");
        assert!(patch.contains("+new line"), "{patch}");
        let deleted = body(&diff, "gone.txt");
        assert!(deleted.contains("-gone"), "{deleted}");

        let hidden = changes(&dir, Vec::<&str>::new(), Some(&dir.join("notes.md"))).unwrap();
        assert!(!ids(&hidden).iter().any(|id| id == "notes.md"));
        assert!(ids(&hidden).iter().any(|id| id == "src/a.txt"));

        let cached = changes(&dir, ["--cached", "-M"], None).unwrap();
        assert_eq!(
            ids(&cached),
            vec!["keep.txt".to_string(), "new name.txt".to_string()]
        );
        let renamed = body(&cached, "new name.txt");
        assert!(renamed.contains("rename to new name.txt"), "{renamed}");
        let staged = body(&cached, "keep.txt");
        assert!(staged.contains("+staged"), "{staged}");

        let pathspec = changes(&dir, ["--", "src/a.txt"], None).unwrap();
        assert_eq!(ids(&pathspec), vec!["src/a.txt".to_string()]);

        let with_code = changes(&dir, ["--exit-code"], None).unwrap();
        assert!(ids(&with_code).iter().any(|id| id == "src/a.txt"));

        match changes(&dir, ["--quiet"], None) {
            Err(err) => assert!(err.contains("no patch"), "{err}"),
            Ok(_) => panic!("quiet diff opened"),
        }
    }

    #[test]
    fn commit_ranges_are_git_diffs() {
        let dir = scratch("range");
        init(&dir);
        std::fs::write(dir.join("a.txt"), "BASE\n").unwrap();
        commit(&dir, "base");
        let base = git_text(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "MID\n").unwrap();
        std::fs::write(dir.join("b.txt"), "added\n").unwrap();
        commit(&dir, "mid");
        let mid = git_text(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("b.txt"), "LATER\n").unwrap();
        commit(&dir, "later");
        let later = git_text(&dir, &["rev-parse", "HEAD"]);

        let between = changes(&dir, [format!("{base}..{mid}")], None).unwrap();
        assert_eq!(
            ids(&between),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        let a = body(&between, "a.txt");
        assert!(a.contains("-BASE"), "{a}");
        assert!(a.contains("+MID"), "{a}");
        assert!(!a.contains("LATER"), "{a}");
        let b = body(&between, "b.txt");
        assert!(b.contains("+added"), "{b}");
        assert!(!b.contains("LATER"), "{b}");

        let after = changes(&dir, [format!("{mid}..{later}")], None).unwrap();
        assert_eq!(ids(&after), vec!["b.txt".to_string()]);
        let later_patch = body(&after, "b.txt");
        assert!(later_patch.contains("+LATER"), "{later_patch}");

        let same = changes(&dir, [format!("{later}..{later}")], None).unwrap();
        assert!(same.is_empty());

        git(&dir, &["checkout", "-q", "-b", "side", base.as_str()]);
        std::fs::write(dir.join("a.txt"), "SIDE\n").unwrap();
        commit(&dir, "side");
        let side = git_text(&dir, &["rev-parse", "HEAD"]);
        let three = changes(&dir, [format!("{later}...{side}")], None).unwrap();
        assert_eq!(ids(&three), vec!["a.txt".to_string()]);
        let side_patch = body(&three, "a.txt");
        assert!(side_patch.contains("-BASE"), "{side_patch}");
        assert!(side_patch.contains("+SIDE"), "{side_patch}");
        assert!(!side_patch.contains("MID"), "{side_patch}");
        assert!(!side_patch.contains("LATER"), "{side_patch}");
    }

    #[test]
    fn an_unknown_revision_is_gits_error() {
        let dir = scratch("bad-rev");
        init(&dir);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        commit(&dir, "init");
        let err = match changes(&dir, ["not-a-rev"], None) {
            Err(err) => err,
            Ok(_) => panic!("unknown revision opened"),
        };
        assert!(err.contains("unknown revision"), "{err}");
    }

    #[test]
    fn a_brief_path_is_omitted() {
        let dir = scratch("brief");
        init(&dir);
        std::fs::write(dir.join("keep.txt"), "a\n").unwrap();
        std::fs::write(dir.join("note.brief"), "(name X)\n").unwrap();
        commit(&dir, "init");
        std::fs::write(dir.join("keep.txt"), "b\n").unwrap();
        std::fs::write(dir.join("note.brief"), "(name Y)\n").unwrap();
        let diff = changes(&dir, Vec::<&str>::new(), None).unwrap();
        assert_eq!(ids(&diff), vec!["keep.txt".to_string()]);
        assert!(body(&diff, "keep.txt").contains("diff --git"));
    }
}
