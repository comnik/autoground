use std::collections::VecDeque;
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
}

impl ListingFilter {
    pub fn from_repo(root: &Path) -> Self {
        Self {
            gitignore: load_gitignore(root),
            model_rel: None,
        }
    }

    pub fn set_model(&mut self, root: &Path, model: &Path) {
        self.model_rel = rel_posix(root, model);
    }

    pub fn hide(&self, rel: &str, is_dir: bool) -> bool {
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
    objects: Vec<String>,
    samples: Vec<String>,
}

impl Filesystem {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, UniverseError> {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        let objects = list_files_recursive(&root, "", &filter)?;
        Ok(Self {
            root,
            filter,
            objects,
            samples: Vec::new(),
        })
    }

    /// Children of the repo root. Search starts here; it does not list every leaf.
    pub fn at_root(root: impl Into<PathBuf>) -> Result<Self, UniverseError> {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        let mut objects: Vec<String> = list_entries(&root, "", &filter)?
            .into_iter()
            .map(|entry| entry.rel_path)
            .collect();
        objects.sort();
        Ok(Self {
            root,
            filter,
            objects,
            samples: Vec::new(),
        })
    }

    pub fn samples(&self) -> &[String] {
        &self.samples
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

    fn render_one(&self, id: &str) -> Result<String, UniverseError> {
        let entry = self.lookup(id)?;
        if entry.is_dir {
            Ok(render_listing(
                id,
                &list_entries(&self.root, id, &self.filter)?,
            ))
        } else {
            Ok(render_leaf(id, &read_leaf(&repo_join(&self.root, id))?))
        }
    }

    fn estimate_one(&self, id: &str) -> Result<usize, UniverseError> {
        let entry = self.lookup(id)?;
        if entry.is_dir {
            Ok(render_listing(id, &list_entries(&self.root, id, &self.filter)?).len())
        } else {
            let len = fs::metadata(repo_join(&self.root, id))?.len() as usize;
            Ok(leaf_header(id).len() + len)
        }
    }

    fn estimate_blocks(&self, ids: &[String]) -> Result<usize, UniverseError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut n = 0;
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                n += 2;
            }
            n += self.estimate_one(id)?;
        }
        Ok(n)
    }

    fn view_ids(&self) -> Vec<String> {
        let mut ids = self.objects.clone();
        for s in &self.samples {
            if !ids.iter().any(|o| o == s) {
                ids.push(s.clone());
            }
        }
        ids
    }

    fn insert_sorted(vec: &mut Vec<String>, id: String) {
        if let Err(i) = vec.binary_search(&id) {
            vec.insert(i, id);
        }
    }
}

fn leaf_header(id: &str) -> String {
    format!("=== {id} ===\n")
}

fn render_leaf(id: &str, body: &LeafBody) -> String {
    let mut s = leaf_header(id);
    match body {
        LeafBody::Text(text) => s.push_str(text),
        LeafBody::Binary(n) => s.push_str(&format!("<binary {n} bytes>")),
    }
    s
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

impl Universe for Filesystem {
    fn objects(&self) -> &[String] {
        &self.objects
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

    fn is_leaf(&self, id: &str) -> Result<bool, UniverseError> {
        Ok(!self.lookup(id)?.is_dir)
    }

    fn estimate(&self) -> Result<usize, UniverseError> {
        self.estimate_blocks(&self.view_ids())
    }

    fn size(&self) -> Result<usize, UniverseError> {
        Ok(self.render()?.len())
    }

    fn render(&self) -> Result<String, UniverseError> {
        let ids = self.view_ids();
        if ids.is_empty() {
            return Ok(String::new());
        }
        let mut blocks = Vec::with_capacity(ids.len());
        for id in &ids {
            blocks.push(self.render_one(id)?);
        }
        Ok(blocks.join("\n\n"))
    }

    fn render_capped(&self, id: &str, budget: usize) -> Result<String, UniverseError> {
        let rendered = self.render_one(id)?;
        if rendered.len() <= budget || !self.is_leaf(id)? {
            return Ok(rendered);
        }
        let header = leaf_header(id);
        let marker_len = "\n…\n".len();
        let body_budget = budget
            .saturating_sub(header.len())
            .saturating_sub(marker_len);
        match read_trimmed(&self.root, id, body_budget.max(1)) {
            Ok((body, _)) => Ok(render_leaf(id, &LeafBody::Text(body))),
            Err(_) => Ok(rendered),
        }
    }

    fn samples(&self) -> &[String] {
        &self.samples
    }

    fn sample(&mut self, extra_budget: usize) -> Result<(), UniverseError> {
        if extra_budget == 0 || self.objects.is_empty() {
            return Ok(());
        }
        let base = self.estimate_blocks(&self.objects)?;
        let cap = base.saturating_add(extra_budget);
        let mut queues: Vec<VecDeque<String>> = Vec::new();
        for parent in &self.objects {
            queues.push(self.children(parent)?.into());
        }
        loop {
            let mut added = false;
            for queue in &mut queues {
                while let Some(child) = queue.pop_front() {
                    if self.objects.binary_search(&child).is_ok()
                        || self.samples.binary_search(&child).is_ok()
                    {
                        continue;
                    }
                    let candidate = child.clone();
                    Self::insert_sorted(&mut self.samples, child);
                    if self.estimate()? > cap {
                        if let Ok(i) = self.samples.binary_search(&candidate) {
                            self.samples.remove(i);
                        }
                        continue;
                    }
                    added = true;
                    break;
                }
            }
            if !added {
                break;
            }
        }
        Ok(())
    }

    fn insert(&mut self, id: String) -> Result<(), UniverseError> {
        let _ = self.lookup(&id)?;
        if let Ok(i) = self.samples.binary_search(&id) {
            self.samples.remove(i);
        }
        Self::insert_sorted(&mut self.objects, id);
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        if let Ok(i) = self.objects.binary_search(&id.to_string()) {
            self.objects.remove(i);
        }
    }

    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError> {
        let mut next = ids;
        next.sort();
        next.dedup();
        for id in &next {
            let _ = self.lookup(id)?;
        }
        self.objects = next;
        self.samples.clear();
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
            fs.objects(),
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
    fn leaf_render_includes_path_and_body() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        let text = fs.render().unwrap();
        assert!(text.starts_with("=== frontend/app.py ===\n"), "{text}");
        assert!(text.contains("from backend import api"), "{text}");
        assert!(!text.contains("frontend/\n"), "{text}");
    }

    #[test]
    fn dir_render_is_a_listing_not_file_bodies() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        fs.set(vec!["frontend".into()]).unwrap();
        let text = fs.render().unwrap();
        assert!(text.starts_with("frontend/\n"), "{text}");
        assert!(text.contains("  __init__.py"), "{text}");
        assert!(text.contains("  app.py"), "{text}");
        assert!(!text.contains("from backend import api"), "{text}");
        assert!(!text.contains("==="), "{text}");
    }

    fn top_dirs(fs: &mut Filesystem) {
        fs.set(vec!["backend".into(), "decoy".into(), "frontend".into()])
            .unwrap();
    }

    #[test]
    fn at_root_opens_on_children() {
        let fs = Filesystem::at_root("examples/toy-layered").unwrap();
        assert_eq!(
            fs.objects(),
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
        let leaf_est = fs.estimate().unwrap();
        let leaf_size = fs.size().unwrap();
        assert_eq!(leaf_size, fs.render().unwrap().len());
        top_dirs(&mut fs);
        let dir_est = fs.estimate().unwrap();
        let dir_size = fs.size().unwrap();
        assert_eq!(dir_size, fs.render().unwrap().len());
        assert_eq!(dir_est, dir_size, "directory listings are exact");
        assert!(dir_est < leaf_est, "dirs {dir_est} vs leaves {leaf_est}");
        assert!(
            dir_size < leaf_size,
            "dirs {dir_size} vs leaves {leaf_size}"
        );
    }

    #[test]
    fn sample_adds_context_not_candidates() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        let dirs = fs.objects().to_vec();
        fs.sample(4096).unwrap();
        assert_eq!(fs.objects(), dirs);
        assert!(!fs.samples().is_empty(), "expected sampled children");
        let text = fs.render().unwrap();
        assert!(text.contains("==="), "{text}");
        assert!(
            text.contains("def handle():")
                || text.contains("from backend import api")
                || text.contains("Not part of the layered architecture"),
            "{text}"
        );
    }

    #[test]
    fn sample_zero_budget_is_noop() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        fs.sample(0).unwrap();
        assert!(fs.samples().is_empty());
        assert!(!fs.render().unwrap().contains("==="));
    }

    #[test]
    fn remove_and_insert_dir() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        fs.remove("decoy");
        assert_eq!(
            fs.objects(),
            &["backend".to_string(), "frontend".to_string()]
        );
        let text = fs.render().unwrap();
        assert!(!text.contains("decoy"), "{text}");
        fs.insert("decoy".into()).unwrap();
        assert_eq!(
            fs.objects(),
            &[
                "backend".to_string(),
                "decoy".to_string(),
                "frontend".to_string()
            ]
        );
        assert!(fs.render().unwrap().contains("decoy/"));
    }

    #[test]
    fn set_replaces_view_and_drops_samples() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        top_dirs(&mut fs);
        fs.sample(4096).unwrap();
        assert!(!fs.samples().is_empty());
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        assert_eq!(fs.objects(), &["frontend/app.py".to_string()]);
        assert!(fs.samples().is_empty());
    }

    #[test]
    fn insert_unknown_is_error() {
        let mut fs = Filesystem::open("examples/toy-layered").unwrap();
        let err = fs.insert("no/such.py".into()).unwrap_err();
        assert!(matches!(err, UniverseError::Missing(id) if id == "no/such.py"));
    }

    #[test]
    fn binary_leaf_renders_placeholder() {
        let root = scratch_dir("binary");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.txt"), "hello\n").unwrap();
        std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let mut fs = Filesystem::open(&root).unwrap();
        fs.set(vec!["blob.bin".into()]).unwrap();
        let text = fs.render().unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(text.contains("=== blob.bin ==="), "{text}");
        assert!(text.contains("<binary 4 bytes>"), "{text}");
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
