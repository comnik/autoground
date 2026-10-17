//! A git diff as a tree of databases.
//!
//! Each changed file is a leaf. Its body is that file's patch. Directory
//! prefixes are summaries, the same shape as a filesystem. Arguments are
//! forwarded to `git diff`.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use super::{Universe, UniverseError};

/// In-memory diff. `clone` shares the parsed tree; `set` replaces the window.
#[derive(Clone)]
pub struct Diff {
    tree: Arc<Tree>,
    databases: Vec<String>,
}

struct Tree {
    nodes: HashMap<String, Node>,
}

struct Node {
    /// Summary text. Absent when the database is read instead.
    summary: Option<String>,
    /// Full text of a database with no summary.
    body: String,
    children: Vec<String>,
}

struct Change {
    id: String,
    body: String,
}

impl Diff {
    /// `git_args` are appended to `git diff`. An empty list is the worktree
    /// against the index.
    ///
    /// A path ending in `.lect` is omitted. `hide`, when it lives under
    /// `root`, is omitted too.
    pub fn open(
        root: &Path,
        git_args: impl IntoIterator<Item = impl AsRef<str>>,
        hide: Option<&Path>,
    ) -> Result<Self, UniverseError> {
        let bytes = run_git(root, git_args)?;
        let mut changes = parse_diff(&bytes)?;
        let model = hide.and_then(|path| model_rel(root, path));
        changes.retain(|change| visible(&change.id, model.as_deref()));
        build(changes)
    }

    fn node(&self, id: &str) -> Result<&Node, UniverseError> {
        self.tree
            .nodes
            .get(id)
            .ok_or_else(|| UniverseError::Missing(id.to_string()))
    }
}

impl Node {
    fn readable(body: String) -> Self {
        Self {
            summary: None,
            body,
            children: Vec::new(),
        }
    }

    fn summarized(summary: String, children: Vec<String>) -> Self {
        Self {
            summary: Some(summary),
            body: String::new(),
            children,
        }
    }
}

impl Universe for Diff {
    fn databases(&self) -> &[String] {
        &self.databases
    }

    fn parent(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            None
        } else {
            Some(
                id.rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("")
                    .to_string(),
            )
        }
    }

    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError> {
        Ok(self.node(id)?.children.clone())
    }

    fn summary(&self, id: &str) -> Result<Option<String>, UniverseError> {
        Ok(self.node(id)?.summary.clone())
    }

    fn size(&self) -> Result<usize, UniverseError> {
        let mut total = 0usize;
        for id in &self.databases {
            let node = self.node(id)?;
            total += match &node.summary {
                Some(text) => text.len(),
                None => node.body.len(),
            };
        }
        Ok(total)
    }

    fn read(&self, id: &str, budget: usize) -> Result<String, UniverseError> {
        let node = self.node(id)?;
        if node.summary.is_some() {
            return Err(UniverseError::Summarizable(id.to_string()));
        }
        crate::universe::within_budget(id, node.body.clone(), budget)
    }

    fn insert(&mut self, id: String) -> Result<(), UniverseError> {
        let _ = self.node(&id)?;
        insert_sorted(&mut self.databases, id);
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        if let Ok(index) = self.databases.binary_search(&id.to_string()) {
            self.databases.remove(index);
        }
    }

    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError> {
        let mut next = ids;
        next.sort();
        next.dedup();
        for id in &next {
            let _ = self.node(id)?;
        }
        self.databases = next;
        Ok(())
    }
}

/// Format flags precede the caller's arguments so a `--` and its pathspecs
/// stay pathspecs. Exit status 1 is git's "differences exist" code.
fn run_git(
    root: &Path,
    git_args: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<Vec<u8>, UniverseError> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-pager", "-c", "color.ui=never", "-C"])
        .arg(root)
        .args(["diff", "--raw", "-z", "-p", "--no-ext-diff", "--no-color"]);
    for arg in git_args {
        cmd.arg(arg.as_ref());
    }
    let output = cmd
        .output()
        .map_err(|err| fail(format!("git diff: {err}")))?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        // `--exit-code` exits 1 and still prints the patch. `--quiet` exits 1
        // with an empty patch, which leaves sweep nothing to judge.
        Some(1) if !output.stdout.is_empty() => Ok(output.stdout),
        _ => {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            Err(fail(if err.is_empty() {
                if output.status.code() == Some(1) {
                    "git diff produced no patch".to_string()
                } else {
                    format!("git diff exited {}", output.status)
                }
            } else {
                format!("git diff: {err}")
            }))
        }
    }
}

/// `--raw -z -p` writes NUL-terminated records, then one patch per change.
/// A rename or a copy record carries the source path, then the destination.
fn parse_diff(bytes: &[u8]) -> Result<Vec<Change>, UniverseError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.starts_with(b":") && !bytes.starts_with(b"diff --") {
        return Err(fail("git diff did not return a patch"));
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
            return Err(fail("git diff did not return a raw inventory"));
        }
        let meta =
            std::str::from_utf8(field).map_err(|_| fail("git diff: raw record is not utf-8"))?;
        let status = meta.split(' ').next_back().unwrap_or("");
        let paths = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        let mut names = Vec::with_capacity(paths);
        for _ in 0..paths {
            let path = cstr(bytes, &mut pos)?;
            let path =
                std::str::from_utf8(path).map_err(|_| fail("git diff: path is not utf-8"))?;
            if path.is_empty() {
                return Err(fail("git diff: empty path"));
            }
            names.push(path.to_string());
        }
        ids.push(names.pop().expect("at least one path"));
    }
    let patch = String::from_utf8_lossy(&bytes[pos..]).into_owned();
    let patches = split_patches(&patch);
    if ids.len() != patches.len() {
        return Err(fail(format!(
            "git diff listed {} changes and returned {} patches",
            ids.len(),
            patches.len()
        )));
    }
    Ok(ids
        .into_iter()
        .zip(patches)
        .map(|(id, body)| Change { id, body })
        .collect())
}

fn cstr<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<&'a [u8], UniverseError> {
    let rest = &bytes[*pos..];
    let Some(index) = rest.iter().position(|byte| *byte == 0) else {
        return Err(fail("git diff: truncated record"));
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
    if name.ends_with(".lect") {
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

fn build(changes: Vec<Change>) -> Result<Diff, UniverseError> {
    let mut seen = HashSet::new();
    for change in &changes {
        if !seen.insert(change.id.clone()) {
            return Err(fail(format!("git diff listed {} twice", change.id)));
        }
    }
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    child_map.insert(String::new(), Vec::new());
    let mut leaves: HashMap<String, String> = HashMap::new();
    for change in changes {
        let segments: Vec<&str> = change.id.split('/').filter(|seg| !seg.is_empty()).collect();
        if segments.is_empty() {
            return Err(fail("git diff: empty path"));
        }
        let mut parent = String::new();
        for (index, segment) in segments.iter().enumerate() {
            let id = if parent.is_empty() {
                (*segment).to_string()
            } else {
                format!("{parent}/{segment}")
            };
            let kids = child_map.entry(parent.clone()).or_default();
            if !kids.contains(&id) {
                kids.push(id.clone());
            }
            if index + 1 < segments.len() {
                child_map.entry(id.clone()).or_default();
            }
            parent = id;
        }
        leaves.insert(change.id, change.body);
    }
    for kids in child_map.values_mut() {
        kids.sort_by(|left, right| {
            let left_name = left.rsplit('/').next().unwrap_or(left);
            let right_name = right.rsplit('/').next().unwrap_or(right);
            left_name.cmp(right_name).then(left.cmp(right))
        });
    }
    let mut nodes = HashMap::new();
    for (id, kids) in &child_map {
        nodes.insert(
            id.clone(),
            Node::summarized(render(id, kids, &child_map, &leaves), kids.clone()),
        );
    }
    let mut databases = Vec::new();
    for (id, body) in leaves {
        nodes.insert(id.clone(), Node::readable(body));
        databases.push(id);
    }
    databases.sort();
    Ok(Diff {
        tree: Arc::new(Tree { nodes }),
        databases,
    })
}

fn render(
    id: &str,
    kids: &[String],
    dirs: &HashMap<String, Vec<String>>,
    leaves: &HashMap<String, String>,
) -> String {
    let mut text = if id.is_empty() {
        "/\n".to_string()
    } else {
        format!("{id}/\n")
    };
    for kid in kids {
        let name = kid.rsplit('/').next().unwrap_or(kid);
        text.push_str("  ");
        text.push_str(name);
        if dirs.contains_key(kid) && !leaves.contains_key(kid) {
            text.push('/');
        }
        text.push('\n');
    }
    text
}

fn insert_sorted(vec: &mut Vec<String>, id: String) {
    if let Err(index) = vec.binary_search(&id) {
        vec.insert(index, id);
    }
}

fn fail(message: impl Into<String>) -> UniverseError {
    UniverseError::Io(io::Error::new(io::ErrorKind::Other, message.into()))
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

    #[test]
    fn a_worktree_diff_is_one_database_per_change() {
        let dir = scratch("worktree");
        init(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "old line\n").unwrap();
        std::fs::write(dir.join("keep.txt"), "keep\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("oldname.txt"), "plain\n").unwrap();
        std::fs::write(dir.join("my file.txt"), "space\n").unwrap();
        std::fs::write(dir.join("model.lect"), "(name X)\n").unwrap();
        std::fs::write(dir.join("notes.md"), "old notes\n").unwrap();
        commit(&dir, "init");

        std::fs::write(dir.join("keep.txt"), "staged\n").unwrap();
        git(&dir, &["add", "keep.txt"]);
        git(&dir, &["mv", "oldname.txt", "new name.txt"]);
        std::fs::write(dir.join("src/a.txt"), "new line\n").unwrap();
        std::fs::write(dir.join("my file.txt"), "spaced\n").unwrap();
        std::fs::write(dir.join("model.lect"), "(name Y)\n").unwrap();
        std::fs::write(dir.join("notes.md"), "new notes\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("extra.txt"), "untracked\n").unwrap();

        let mut diff = Diff::open(&dir, Vec::<&str>::new(), None).unwrap();
        assert_eq!(
            diff.databases(),
            &[
                "gone.txt".to_string(),
                "my file.txt".to_string(),
                "notes.md".to_string(),
                "src/a.txt".to_string(),
            ]
        );
        let patch = diff.read("src/a.txt", usize::MAX).unwrap();
        assert!(patch.contains("diff --git"), "{patch}");
        assert!(patch.contains("-old line"), "{patch}");
        assert!(patch.contains("+new line"), "{patch}");
        let deleted = diff.read("gone.txt", usize::MAX).unwrap();
        assert!(deleted.contains("-gone"), "{deleted}");
        assert_eq!(diff.parent("src/a.txt").as_deref(), Some("src"));
        assert_eq!(diff.children("src").unwrap(), vec!["src/a.txt".to_string()]);
        assert_eq!(diff.summary("src").unwrap().unwrap(), "src/\n  a.txt\n");
        assert!(diff
            .read("src", usize::MAX)
            .unwrap_err()
            .to_string()
            .contains("summary"));
        let root_children = diff.children("").unwrap();
        assert!(root_children.iter().any(|id| id == "src"));
        assert!(root_children.iter().any(|id| id == "gone.txt"));

        let full = diff.size().unwrap();
        let gone = diff.read("gone.txt", usize::MAX).unwrap();
        diff.set(vec!["gone.txt".into()]).unwrap();
        assert_eq!(diff.databases(), &["gone.txt".to_string()]);
        assert_eq!(diff.size().unwrap(), gone.len());
        assert!(diff.size().unwrap() < full);
        assert_eq!(diff.children("").unwrap(), root_children);

        let hidden = Diff::open(&dir, Vec::<&str>::new(), Some(&dir.join("notes.md"))).unwrap();
        assert!(!hidden.databases().iter().any(|id| id == "notes.md"));
        assert!(hidden.databases().iter().any(|id| id == "src/a.txt"));

        let cached = Diff::open(&dir, ["--cached", "-M"], None).unwrap();
        assert_eq!(
            cached.databases(),
            &["keep.txt".to_string(), "new name.txt".to_string()]
        );
        let renamed = cached.read("new name.txt", usize::MAX).unwrap();
        assert!(renamed.contains("rename to new name.txt"), "{renamed}");
        let staged = cached.read("keep.txt", usize::MAX).unwrap();
        assert!(staged.contains("+staged"), "{staged}");

        let pathspec = Diff::open(&dir, ["--", "src/a.txt"], None).unwrap();
        assert_eq!(pathspec.databases(), &["src/a.txt".to_string()]);

        let with_code = Diff::open(&dir, ["--exit-code"], None).unwrap();
        assert!(with_code.databases().iter().any(|id| id == "src/a.txt"));

        match Diff::open(&dir, ["--quiet"], None) {
            Err(err) => assert!(err.to_string().contains("no patch"), "{err}"),
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

        let between = Diff::open(&dir, [format!("{base}..{mid}")], None).unwrap();
        assert_eq!(
            between.databases(),
            &["a.txt".to_string(), "b.txt".to_string()]
        );
        let a = between.read("a.txt", usize::MAX).unwrap();
        assert!(a.contains("-BASE"), "{a}");
        assert!(a.contains("+MID"), "{a}");
        assert!(!a.contains("LATER"), "{a}");
        let b = between.read("b.txt", usize::MAX).unwrap();
        assert!(b.contains("+added"), "{b}");
        assert!(!b.contains("LATER"), "{b}");

        let after = Diff::open(&dir, [format!("{mid}..{later}")], None).unwrap();
        assert_eq!(after.databases(), &["b.txt".to_string()]);
        let later_patch = after.read("b.txt", usize::MAX).unwrap();
        assert!(later_patch.contains("+LATER"), "{later_patch}");

        let same = Diff::open(&dir, [format!("{later}..{later}")], None).unwrap();
        assert!(same.databases().is_empty());

        git(&dir, &["checkout", "-q", "-b", "side", base.as_str()]);
        std::fs::write(dir.join("a.txt"), "SIDE\n").unwrap();
        commit(&dir, "side");
        let side = git_text(&dir, &["rev-parse", "HEAD"]);
        let three = Diff::open(&dir, [format!("{later}...{side}")], None).unwrap();
        assert_eq!(three.databases(), &["a.txt".to_string()]);
        let side_patch = three.read("a.txt", usize::MAX).unwrap();
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
        let err = match Diff::open(&dir, ["not-a-rev"], None) {
            Err(err) => err,
            Ok(_) => panic!("unknown revision opened"),
        };
        assert!(err.to_string().contains("unknown revision"), "{err}");
    }
}
