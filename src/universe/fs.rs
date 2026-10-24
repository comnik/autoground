use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{Universe, UniverseError};

/// Gitignored paths, `.git` / `.gitignore`, `.brief` files, plus the model file
/// when it lives inside the repo.
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
        if is_git_meta(rel) || (!is_dir && is_brief(rel)) {
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

/// A `.brief` file is a model, not evidence. The namespace scan still reads these.
fn is_brief(rel: &str) -> bool {
    rel.rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(rel)
        .ends_with(".brief")
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

    /// Drop `model` from listings when it lives inside this repo.
    pub fn hide_model(&mut self, model: &Path) {
        self.filter.set_model(&self.root, model);
        if let Some(rel) = self.filter.model_rel.clone() {
            self.databases.retain(|id| id != &rel);
        }
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
        crate::universe::within_budget(id, text, budget)
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

    fn toy_layered() -> PathBuf {
        let root = scratch_dir("toy-layered");
        let files = [
            ("backend/__init__.py", ""),
            ("backend/api.py", "def handle():\n    return \"ok\"\n"),
            (
                "decoy/noise.py",
                "# Not part of the layered architecture.\nVALUE = 1\n",
            ),
            ("frontend/__init__.py", ""),
            (
                "frontend/app.py",
                "from backend import api\n\ndef main():\n    api.handle()\n",
            ),
        ];
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        root
    }

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
    fn brief_files_are_hidden() {
        let root = scratch_dir("brief");
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(root.join("models/map.brief"), "(name X)\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        std::fs::write(root.join("notes.md"), "# hi\n").unwrap();
        let filter = ListingFilter::from_repo(&root);
        let kids = list_entries(&root, "", &filter).unwrap();
        let names = names(&kids);
        let nested = list_entries(&root, "models", &filter).unwrap();
        let leaves = list_files_recursive(&root, "", &filter).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.txt"), "{names:?}");
        assert!(names.contains(&"notes.md"), "{names:?}");
        assert!(names.contains(&"models"), "{names:?}");
        assert!(nested.is_empty(), "{nested:?}");
        assert!(
            !leaves.iter().any(|path| path.ends_with(".brief")),
            "{leaves:?}"
        );
    }

    #[test]
    fn model_file_in_repo_is_hidden() {
        let root = scratch_dir("model");
        std::fs::create_dir_all(&root).unwrap();
        let model = root.join("README.md");
        std::fs::write(&model, "# model\n").unwrap();
        std::fs::write(root.join("notes.md"), "# other\n").unwrap();
        let mut filter = ListingFilter::from_repo(&root);
        filter.set_model(&root, &model);
        let kids = list_entries(&root, "", &filter).unwrap();
        let names = names(&kids);
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"notes.md"), "{names:?}");
        assert!(!names.contains(&"README.md"), "{names:?}");
    }

    #[test]
    fn toy_layered_opens_on_leaf_files() {
        let fs = Filesystem::open(toy_layered()).unwrap();
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
        let mut fs = Filesystem::open(toy_layered()).unwrap();
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        let text = fs.read("frontend/app.py", usize::MAX).unwrap();
        assert!(text.contains("from backend import api"), "{text}");
        assert!(!text.contains("==="), "{text}");
        assert!(fs.summary("frontend/app.py").unwrap().is_none());
        assert_eq!(fs.size().unwrap(), text.len());
    }

    #[test]
    fn directory_summary_is_a_listing_not_file_bodies() {
        let mut fs = Filesystem::open(toy_layered()).unwrap();
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
    fn directory_view_is_smaller_than_leaves() {
        let mut fs = Filesystem::open(toy_layered()).unwrap();
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
        let mut fs = Filesystem::open(toy_layered()).unwrap();
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
        let mut fs = Filesystem::open(toy_layered()).unwrap();
        top_dirs(&mut fs);
        fs.set(vec!["frontend/app.py".into()]).unwrap();
        assert_eq!(fs.databases(), &["frontend/app.py".to_string()]);
    }

    #[test]
    fn insert_unknown_is_error() {
        let mut fs = Filesystem::open(toy_layered()).unwrap();
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
    fn read_returns_the_body_when_it_fits_and_refuses_when_it_does_not() {
        let root = scratch_dir("budget");
        std::fs::create_dir_all(&root).unwrap();
        let body = "0123456789";
        std::fs::write(root.join("a.txt"), body).unwrap();
        let fs = Filesystem::open(&root).unwrap();
        assert_eq!(fs.read("a.txt", body.len()).unwrap(), body);
        assert_eq!(fs.read("a.txt", usize::MAX).unwrap(), body);
        let err = fs.read("a.txt", body.len() - 1).unwrap_err();
        assert!(
            matches!(err, UniverseError::TooLarge { ref id, bytes } if id == "a.txt" && bytes == body.len()),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hide_model_drops_the_file_from_the_root_view() {
        let root = scratch_dir("hide-model");
        std::fs::create_dir_all(&root).unwrap();
        let model = root.join("model.md");
        std::fs::write(&model, "# model\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        std::fs::write(root.join("claims.brief"), "(name X)\n").unwrap();
        let mut fs = Filesystem::open(&root).unwrap();
        assert!(fs.databases().contains(&"model.md".to_string()));
        assert!(!fs.databases().iter().any(|id| id.ends_with(".brief")));
        fs.hide_model(&model);
        assert_eq!(fs.databases(), &["keep.txt".to_string()]);
        assert_eq!(fs.children("").unwrap(), vec!["keep.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn children_of_a_file_are_empty() {
        let fs = Filesystem::open(toy_layered()).unwrap();
        assert!(fs.children("frontend/app.py").unwrap().is_empty());
        assert_eq!(
            fs.children("frontend").unwrap(),
            vec!["frontend/__init__.py", "frontend/app.py"]
        );
    }
}
