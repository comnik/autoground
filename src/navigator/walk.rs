use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::judge::Child;

const SAMPLE_N: usize = 8;

/// Gitignored paths, plus the model file when it lives inside the repo.
#[derive(Default)]
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
        if self.model_rel.as_deref() == Some(rel) {
            return true;
        }
        match &self.gitignore {
            Some(gi) => gi.matched(rel, is_dir).is_ignore(),
            None => false,
        }
    }
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

pub fn list_dir(root: &Path, rel: &str, filter: &ListingFilter) -> io::Result<Vec<Child>> {
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
        let sample = if is_dir {
            sample_names(root, &rel_path, filter)
        } else {
            Vec::new()
        };
        children.push(Child {
            name,
            rel_path,
            is_dir,
            sample,
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
        for child in list_dir(root, &dir, filter)? {
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

fn sample_names(root: &Path, dir_rel: &str, filter: &ListingFilter) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(repo_join(root, dir_rel)) else {
        return names;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = if dir_rel.is_empty() {
            name.clone()
        } else {
            format!("{dir_rel}/{name}")
        };
        let is_dir = entry.path().is_dir();
        if filter.hide(&child_rel, is_dir) {
            continue;
        }
        names.push(name);
        if names.len() >= SAMPLE_N {
            break;
        }
    }
    names
}

pub fn parent_dir(rel: &str) -> Option<&str> {
    rel.rsplit_once('/').map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_toy_layered_root() {
        let root = Path::new("examples/toy-layered");
        let kids = list_dir(root, "", &ListingFilter::from_repo(root)).unwrap();
        let names: Vec<_> = kids.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"frontend"));
        assert!(names.contains(&"backend"));
        assert!(names.contains(&"decoy"));
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autoground-walk-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn gitignore_hides_listed_paths() {
        let root = scratch_dir("gitignore");
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join(".gitignore"), "secret.txt\nbuild/\n").unwrap();
        std::fs::write(root.join("secret.txt"), "nope\n").unwrap();
        std::fs::write(root.join("keep.txt"), "yes\n").unwrap();
        std::fs::write(root.join("build/out.txt"), "nope\n").unwrap();
        let kids = list_dir(&root, "", &ListingFilter::from_repo(&root)).unwrap();
        let names: Vec<_> = kids.iter().map(|c| c.name.as_str()).collect();
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.txt"), "{names:?}");
        assert!(names.contains(&".gitignore"), "{names:?}");
        assert!(!names.contains(&"secret.txt"), "{names:?}");
        assert!(!names.contains(&"build"), "{names:?}");
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
        let kids = list_dir(&root, "", &filter).unwrap();
        let names: Vec<_> = kids.iter().map(|c| c.name.as_str()).collect();
        let _ = std::fs::remove_dir_all(&root);
        assert!(names.contains(&"keep.ag"), "{names:?}");
        assert!(!names.contains(&"model.ag"), "{names:?}");
    }
}
