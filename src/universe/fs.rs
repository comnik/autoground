use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{Universe, UniverseError};

/// Gitignored paths, `.git` / `.gitignore`, plus the model file when it lives
/// inside the repo.
#[derive(Default, Clone)]
pub struct ListingFilter {
    gitignore: Option<Gitignore>,
    model_rel: Option<String>,
    /// When set, listings omit paths outside this directory. Ancestors stay
    /// visible so a walk can enter it.
    only: Option<String>,
}

impl ListingFilter {
    pub fn from_repo(root: &Path) -> Self {
        Self {
            gitignore: load_gitignore(root),
            model_rel: None,
            only: None,
        }
    }

    pub fn set_only(&mut self, prefix: impl Into<String>) {
        self.only = Some(prefix.into());
    }

    pub fn set_model(&mut self, root: &Path, model: &Path) {
        self.model_rel = rel_posix(root, model);
    }

    pub fn hide(&self, rel: &str, is_dir: bool) -> bool {
        if self.outside(rel) {
            return true;
        }
        if is_git_meta(rel) {
            return true;
        }
        if self.model_rel.as_deref() == Some(rel) {
            return true;
        }
        match &self.gitignore {
            Some(gi) => gi.matched(rel, is_dir).is_ignore(),
            None => false,
        }
    }

    fn outside(&self, rel: &str) -> bool {
        let Some(prefix) = &self.only else {
            return false;
        };
        if rel.is_empty() {
            return false;
        }
        let under = rel == prefix.as_str() || rel.starts_with(&format!("{prefix}/"));
        let ancestor = prefix.starts_with(&format!("{rel}/"));
        !under && !ancestor
    }
}

/// `.git` (and anything under it) and `.gitignore` are never listing children.
fn is_git_meta(rel: &str) -> bool {
    rel.split('/')
        .any(|part| part == ".git" || part == ".gitignore")
}

fn load_gitignore(root: &Path) -> Option<Gitignore> {
    let path = root.join(".gitignore");
    if !path.is_file() {
        return None;
    }
    let mut builder = GitignoreBuilder::new(root);
    let _ = builder.add(&path);
    builder.build().ok()
}

fn rel_posix(root: &Path, path: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    let rel = path.strip_prefix(&root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn repo_join(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// Parent path component. Top-level names (`frontend`) have no parent here;
/// [`Filesystem::parent`] maps that to the root id `""`.
pub fn parent_dir(rel: &str) -> Option<&str> {
    rel.rsplit_once('/').map(|(p, _)| p)
}

pub fn parent_id(id: &str) -> Option<String> {
    if id.is_empty() {
        None
    } else {
        Some(parent_dir(id).unwrap_or("").to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
}

pub fn list_entries(root: &Path, rel: &str, filter: &ListingFilter) -> io::Result<Vec<Entry>> {
    let dir = repo_join(root, rel);
    let mut children = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let rel_path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let is_dir = meta.is_dir();
        if filter.hide(&rel_path, is_dir) {
            continue;
        }
        children.push(Entry {
            name,
            rel_path,
            is_dir,
        });
    }
    Ok(children)
}

pub fn list_files_recursive(
    root: &Path,
    rel: &str,
    filter: &ListingFilter,
) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![rel.to_string()];
    while let Some(dir) = stack.pop() {
        for child in list_entries(root, &dir, filter)? {
            if child.is_dir {
                stack.push(child.rel_path);
            } else {
                out.push(child.rel_path);
            }
        }
    }
    out.sort();
    Ok(out)
}

pub fn read_trimmed(root: &Path, rel: &str, budget: usize) -> io::Result<(String, bool)> {
    let raw = fs::read_to_string(repo_join(root, rel))?;
    if raw.len() <= budget {
        Ok((raw, false))
    } else {
        let head = budget / 2;
        let tail = budget - head;
        let mut s = String::new();
        s.push_str(&raw[..head]);
        s.push_str("\n…\n");
        s.push_str(&raw[raw.len() - tail..]);
        Ok((s, true))
    }
}

/// Local filesystem as a tree of databases. The initial view is every reachable
/// leaf file.
#[derive(Clone)]
pub struct Filesystem {
    root: PathBuf,
    filter: ListingFilter,
    databases: Vec<String>,
}

impl Filesystem {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, UniverseError> {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        let databases = list_files_recursive(&root, "", &filter)?;
        Ok(Self {
            root,
            filter,
            databases,
        })
    }

    /// Children of the repo root. Search starts here; it does not list every leaf.
    pub fn at_root(root: impl Into<PathBuf>) -> Result<Self, UniverseError> {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        let mut databases: Vec<String> = list_entries(&root, "", &filter)?
            .into_iter()
            .map(|entry| entry.rel_path)
            .collect();
        databases.sort();
        Ok(Self {
            root,
            filter,
            databases,
        })
    }

    /// Drop `model` from listings when it lives inside this repo.
    pub fn hide_model(&mut self, model: &Path) {
        self.filter.set_model(&self.root, model);
        if let Some(rel) = self.filter.model_rel.clone() {
            self.databases.retain(|id| id != &rel);
        }
    }

    /// Hide every path outside `prefix`. The prefix and its ancestors remain.
    pub fn set_only(&mut self, prefix: &str) {
        self.filter.set_only(prefix);
        self.databases.retain(|id| !self.filter.outside(id));
    }

    fn lookup(&self, id: &str) -> Result<Entry, UniverseError> {
        if id.is_empty() {
            let meta = fs::symlink_metadata(&self.root)?;
            if !meta.is_dir() {
                return Err(UniverseError::Missing(id.to_string()));
            }
            return Ok(Entry {
                name: String::new(),
                rel_path: String::new(),
                is_dir: true,
            });
        }
        let path = repo_join(&self.root, id);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(UniverseError::Missing(id.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(UniverseError::Missing(id.to_string()));
        }
        let is_dir = meta.is_dir();
        if self.filter.hide(id, is_dir) {
            return Err(UniverseError::Missing(id.to_string()));
        }
        let name = id.rsplit('/').next().unwrap_or(id).to_string();
        Ok(Entry {
            name,
            rel_path: id.to_string(),
            is_dir,
        })
    }

    fn insert_sorted(vec: &mut Vec<String>, id: String) {
        if let Err(i) = vec.binary_search(&id) {
            vec.insert(i, id);
        }
    }

    /// Summary length of a directory, or the metadata length of a file.
    fn measured_len(&self, id: &str) -> Result<usize, UniverseError> {
        let entry = self.lookup(id)?;
        if entry.is_dir {
            Ok(render_listing(id, &list_entries(&self.root, id, &self.filter)?).len())
        } else {
            Ok(fs::metadata(repo_join(&self.root, id))?.len() as usize)
        }
    }
}

fn listing_title(id: &str) -> String {
    if id.is_empty() {
        "/".into()
    } else {
        format!("{}/", id.trim_end_matches('/'))
    }
}

fn render_listing(id: &str, children: &[Entry]) -> String {
    let mut s = listing_title(id);
    s.push('\n');
    for child in children {
        s.push_str("  ");
        if child.is_dir {
            s.push_str(&child.name);
            s.push('/');
        } else {
            s.push_str(&child.name);
        }
        s.push('\n');
    }
    s
}

enum LeafBody {
    Text(String),
    Binary(usize),
}

fn read_leaf(path: &Path) -> io::Result<LeafBody> {
    let bytes = fs::read(path)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(LeafBody::Text(text)),
        Err(e) => Ok(LeafBody::Binary(e.as_bytes().len())),
    }
}

/// Head and tail of `text`, at most `budget` bytes, with a marker in the middle.
fn trim_text(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let marker = "\n…\n";
    if budget <= marker.len() {
        return prefix_bytes(text, budget);
    }
    let body = budget - marker.len();
    let head_end = floor_boundary(text, body / 2);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(body - body / 2));
    if tail_start <= head_end {
        return prefix_bytes(text, budget);
    }
    let mut out = String::new();
    out.push_str(&text[..head_end]);
    out.push_str(marker);
    out.push_str(&text[tail_start..]);
    if out.len() > budget {
        return prefix_bytes(text, budget);
    }
    out
}

fn prefix_bytes(text: &str, budget: usize) -> String {
    let end = floor_boundary(text, budget.min(text.len()));
    text[..end].to_string()
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

impl Universe for Filesystem {
    fn databases(&self) -> &[String] {
        &self.databases
    }

    fn parent(&self, id: &str) -> Option<String> {
        parent_id(id)
    }

    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError> {
        let entry = self.lookup(id)?;
        if !entry.is_dir {
            return Ok(Vec::new());
        }
        Ok(list_entries(&self.root, id, &self.filter)?
            .into_iter()
            .map(|e| e.rel_path)
            .collect())
    }

    fn summary(&self, id: &str) -> Result<Option<String>, UniverseError> {
        let entry = self.lookup(id)?;
        if entry.is_dir {
            Ok(Some(render_listing(
                id,
                &list_entries(&self.root, id, &self.filter)?,
            )))
        } else {
            Ok(None)
        }
    }

    fn size(&self) -> Result<usize, UniverseError> {
        let mut total = 0usize;
        for id in &self.databases {
            total += self.measured_len(id)?;
        }
        Ok(total)
    }

    fn read(&self, id: &str, budget: usize) -> Result<String, UniverseError> {
        let entry = self.lookup(id)?;
        if entry.is_dir {
            return Err(UniverseError::Summarizable(id.to_string()));
        }
        let text = match read_leaf(&repo_join(&self.root, id))? {
            LeafBody::Text(text) => text,
            LeafBody::Binary(n) => format!("<binary {n} bytes>"),
        };
        Ok(trim_text(&text, budget))
    }

    fn insert(&mut self, id: String) -> Result<(), UniverseError> {
        let _ = self.lookup(&id)?;
        Self::insert_sorted(&mut self.databases, id);
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        if let Ok(i) = self.databases.binary_search(&id.to_string()) {
            self.databases.remove(i);
        }
    }

    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError> {
        let mut next = ids;
        next.sort();
        next.dedup();
        for id in &next {
            let _ = self.lookup(id)?;
        }
        self.databases = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autoground-universe-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn gitignore_hides_listed_paths() {
        let root = scratch_dir("gitignore");
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join(".gitignore"), "secret.txt\nbuild/\n").unwrap();
        std::fs::write(root.join("secret.txt"), "nope\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        std::fs::write(root.join("build/out.txt"), "nope\n").unwrap();
        let kids = list_entries(&root, "", &ListingFilter::from_repo(&root)).unwrap();
        let names = names(&kids);
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.txt"), "{names:?}");
        assert!(!names.contains(&".gitignore"), "{names:?}");
        assert!(!names.contains(&"secret.txt"), "{names:?}");
        assert!(!names.contains(&"build"), "{names:?}");
    }

    #[test]
    fn git_dir_and_gitignore_are_hidden() {
        let root = scratch_dir("git-meta");
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/target\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        std::fs::write(root.join(".gitkeep"), "keep\n").unwrap();
        let filter = ListingFilter::from_repo(&root);
        let kids = list_entries(&root, "", &filter).unwrap();
        let names = names(&kids);
        let leaves = list_files_recursive(&root, "", &filter).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.txt"), "{names:?}");
        assert!(names.contains(&".gitkeep"), "{names:?}");
        assert!(!names.contains(&".git"), "{names:?}");
        assert!(!names.contains(&".gitignore"), "{names:?}");
        assert!(
            leaves
                .iter()
                .all(|p| !p.split('/').any(|part| part == ".git")),
            "{leaves:?}"
        );
        assert!(!leaves.iter().any(|p| p == ".gitignore"), "{leaves:?}");
    }

    #[test]
    fn model_file_in_repo_is_hidden() {
        let root = scratch_dir("model");
        std::fs::create_dir_all(&root).unwrap();
        let model = root.join("model.ag");
        std::fs::write(&model, "(require (component \"X\"))\n").unwrap();
        std::fs::write(root.join("keep.ag"), "(component \"X\")\n").unwrap();
        let mut filter = ListingFilter::from_repo(&root);
        filter.set_model(&root, &model);
        let kids = list_entries(&root, "", &filter).unwrap();
        let names = names(&kids);
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.ag"), "{names:?}");
        assert!(!names.contains(&"model.ag"), "{names:?}");
    }

    #[test]
    fn toy_layered_opens_on_leaf_files() {
        let fs = Filesystem::open("examples/toy-layered").unwrap();
        assert_eq!(
            fs.databases(),
            &[
                "backend/__init__.py",
                "backend/api.py",
                "decoy/noise.py",
                "frontend/__init__.py",
                "frontend/app.py",
            ]
        );
    }

    #[test]
    fn leaf_read_is_the_body_and_size_is_its_length() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        let text = fs.read("frontend/app.py", usize::MAX).unwrap();
        assert!(text.contains("from backend import api"), "{text}");
        assert!(!text.contains("==="), "{text}");
        assert!(fs.summary("frontend/app.py").unwrap().is_none());
        assert_eq!(fs.size().unwrap(), text.len());
    }

    #[test]
    fn directory_summary_is_a_listing_not_file_bodies() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        fs.set(vec!["frontend".into()]).unwrap();
        let text = fs.summary("frontend").unwrap().unwrap();
        assert!(text.starts_with("frontend/\n"), "{text}");
        assert!(text.contains("  __init__.py"), "{text}");
        assert!(text.contains("  app.py"), "{text}");
        assert!(!text.contains("from backend import api"), "{text}");
        assert!(matches!(
            fs.read("frontend", usize::MAX),
            Err(UniverseError::Summarizable(_))
        ));
        assert_eq!(fs.size().unwrap(), text.len());
    }

    fn top_dirs(fs: &mut Filesystem) {
        fs.set(vec!["backend".into(), "decoy".into(), "frontend".into()])
            .unwrap();
    }

    #[test]
    fn set_only_hides_paths_outside_the_prefix() {
        let root = std::env::temp_dir().join(format!("autoground-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("evals")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a").unwrap();
        std::fs::write(root.join("evals/b.txt"), "b").unwrap();
        std::fs::write(root.join("README.md"), "r").unwrap();
        let mut fs = Filesystem::at_root(&root).unwrap();
        fs.set_only("src");
        let children = fs.children("").unwrap();
        assert!(children.contains(&"src".to_string()), "{children:?}");
        assert!(!children.iter().any(|id| id == "evals"), "{children:?}");
        assert!(!children.iter().any(|id| id == "README.md"), "{children:?}");
        assert_eq!(fs.children("src").unwrap(), vec!["src/a.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn at_root_opens_on_children() {
        let fs = Filesystem::at_root("examples/toy-layered").unwrap();
        assert_eq!(
            fs.databases(),
            &[
                "backend".to_string(),
                "decoy".to_string(),
                "frontend".to_string()
            ]
        );
    }

    #[test]
    fn directory_view_is_smaller_than_leaves() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        let leaf_size = fs.size().unwrap();
        top_dirs(&mut fs);
        let dir_size = fs.size().unwrap();
        assert!(
            dir_size < leaf_size,
            "dirs {dir_size} vs leaves {leaf_size}"
        );
    }

    #[test]
    fn remove_and_insert_dir() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        fs.remove("decoy");
        assert_eq!(
            fs.databases(),
            &["backend".to_string(), "frontend".to_string()]
        );
        fs.insert("decoy".into()).unwrap();
        assert_eq!(
            fs.databases(),
            &[
                "backend".to_string(),
                "decoy".to_string(),
                "frontend".to_string()
            ]
        );
    }

    #[test]
    fn set_replaces_the_view() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        assert_eq!(fs.databases(), &["frontend/app.py".to_string()]);
    }

    #[test]
    fn insert_unknown_is_error() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        let err = fs.insert("no/such.py".into()).unwrap_err();
        assert!(matches!(err, UniverseError::Missing(id) if id == "no/such.py"));
    }

    #[test]
    fn binary_leaf_reads_as_a_placeholder() {
        let root = scratch_dir("binary");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.txt"), "hello\n").unwrap();
        std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let mut fs = Filesystem::open(&root).unwrap();
        fs.set(vec!["blob.bin".into()]).unwrap();
        let text = fs.read("blob.bin", usize::MAX).unwrap();
        let summary = fs.summary("blob.bin").unwrap();
        let size = fs.size().unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(text, "<binary 4 bytes>");
        assert!(summary.is_none());
        assert_eq!(size, 4);
    }

    #[test]
    fn hide_model_drops_the_file_from_the_root_view() {
        let root = scratch_dir("hide-model");
        std::fs::create_dir_all(&root).unwrap();
        let model = root.join("model.ag");
        std::fs::write(&model, "(name X)\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        let mut fs = Filesystem::at_root(&root).unwrap();
        fs.hide_model(&model);
        assert_eq!(fs.databases(), &["keep.txt".to_string()]);
        assert_eq!(fs.children("").unwrap(), vec!["keep.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn children_of_a_file_are_empty() {
        let fs = Filesystem::open("examples/toy-layered").unwrap();
        assert!(fs.children("frontend/app.py").unwrap().is_empty());
        assert_eq!(
            fs.children("frontend").unwrap(),
            vec!["frontend/__init__.py", "frontend/app.py"]
        );
    }
}
