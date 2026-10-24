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

/// Local filesystem as a tree of databases. The databases are every reachable
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

impl Universe for Filesystem {
    fn databases(&self) -> &[String] {
        &self.databases
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
    fn a_file_has_no_summary() {
        let fs = Filesystem::open(toy_layered()).unwrap();
        assert!(fs.summary("frontend/app.py").unwrap().is_none());
    }

    #[test]
    fn directory_summary_is_a_listing_not_file_bodies() {
        let fs = Filesystem::open(toy_layered()).unwrap();
        let text = fs.summary("frontend").unwrap().unwrap();
        assert!(text.starts_with("frontend/\n"), "{text}");
        assert!(text.contains("  __init__.py"), "{text}");
        assert!(text.contains("  app.py"), "{text}");
        assert!(!text.contains("from backend import api"), "{text}");
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
