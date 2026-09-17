use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::judge::Child;

const SAMPLE_N: usize = 8;

pub fn repo_join(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

pub fn list_dir(root: &Path, rel: &str) -> io::Result<Vec<Child>> {
    let dir = repo_join(root, rel);
    let mut children = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if should_hide(&name) {
            continue;
        }
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
        let sample = if is_dir {
            sample_names(&entry.path())
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

pub fn list_files_recursive(root: &Path, rel: &str) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![rel.to_string()];
    while let Some(dir) = stack.pop() {
        for child in list_dir(root, &dir)? {
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

fn sample_names(path: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return names;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries.into_iter().take(SAMPLE_N) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if should_hide(&name) {
            continue;
        }
        names.push(name);
    }
    names
}

fn should_hide(name: &str) -> bool {
    name.starts_with('.')
        || name == "target"
        || name == "node_modules"
        || name == "__pycache__"
        || name.ends_with(".pyc")
        || name == ".DS_Store"
}

/// Names that must not be entered while locating a component, even if a
/// judge says descend. Stops decoy/ file bodies from being read.
pub fn is_noise_name(name: &str) -> bool {
    let n: String = name
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| *c != '-' && *c != '_')
        .collect();
    n == "decoy" || n == "noise" || n == "tmp" || n == "temp"
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
        let kids = list_dir(root, "").unwrap();
        let names: Vec<_> = kids.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"frontend"));
        assert!(names.contains(&"backend"));
        assert!(names.contains(&"decoy"));
    }
}
