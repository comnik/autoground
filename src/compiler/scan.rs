use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

use crate::syntax::{brief_source, extract_briefs, mentions_namespace, parse, Program};

/// A model file that declares a namespace. `path` is relative to the repo
/// when the file lives inside it.
#[derive(Debug)]
pub struct Loaded {
    pub path: String,
    pub program: Program,
}

#[derive(Debug)]
pub struct LoadError {
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LoadError {}

/// Path of `path` relative to `root`, or the absolute path when it is outside.
pub fn display_path(root: &Path, path: &Path) -> String {
    let root = canonicalize(root);
    let path = canonicalize(path);
    match path.strip_prefix(&root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Read `.brief` files and Markdown `brief` fences under `root`.
///
/// Gitignored paths and `.git` are skipped. A file with no `ns` is left out.
/// A file that declares `ns` and does not parse is an error. The directory
/// layout is not the namespace.
pub fn scan_namespaces(root: &Path) -> Result<Vec<Loaded>, LoadError> {
    let (root, walker) = walk(root)?;
    let mut files = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|err| LoadError {
            message: err.to_string(),
        })?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                return Err(LoadError {
                    message: format!("{}: {err}", display_path(&root, path)),
                })
            }
        };
        let owned;
        let src = if has_ext(path, "md") {
            match brief_source(&text) {
                Ok(src) => {
                    owned = src;
                    owned.as_str()
                }
                Err(_) => continue,
            }
        } else if has_ext(path, "brief") {
            text.as_str()
        } else {
            continue;
        };
        let display = display_path(&root, path);
        match parse(src) {
            Ok(program) if program.namespace.is_some() => files.push(Loaded {
                path: display,
                program,
            }),
            Ok(_) => {}
            Err(err) if mentions_namespace(src) => {
                return Err(LoadError {
                    message: format!("{display}: {err}"),
                })
            }
            Err(_) => {}
        }
    }
    Ok(files)
}

/// A `.brief` file, or a Markdown file that contains a `brief` fence.
///
/// `path` is relative to the directory that was scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefFile {
    pub path: String,
    pub absolute: PathBuf,
}

/// `.brief` files and Markdown files with a `brief` fence under `root`.
///
/// Gitignored paths and `.git` are skipped. A `.brief` file is listed even
/// when it does not parse. Paths are relative to `root` and sorted.
pub fn scan_briefs(root: &Path) -> Result<Vec<BriefFile>, LoadError> {
    let (root, walker) = walk(root)?;
    let mut files = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|err| LoadError {
            message: err.to_string(),
        })?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if has_ext(path, "brief") {
            if let Err(err) = std::fs::read_to_string(path) {
                return Err(LoadError {
                    message: format!("{}: {err}", display_path(&root, path)),
                });
            }
        } else if has_ext(path, "md") {
            let text = std::fs::read_to_string(path).map_err(|err| LoadError {
                message: format!("{}: {err}", display_path(&root, path)),
            })?;
            if extract_briefs(&text).is_empty() {
                continue;
            }
        } else {
            continue;
        }
        files.push(BriefFile {
            path: display_path(&root, path),
            absolute: canonicalize(path),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn walk(root: &Path) -> Result<(PathBuf, ignore::Walk), LoadError> {
    if !root.is_dir() {
        return Err(LoadError {
            message: format!("repository not found: {}", root.display()),
        });
    }
    let root = canonicalize(root);
    let gitignore = load_gitignore(&root);
    let walker = {
        let root = root.clone();
        let mut builder = WalkBuilder::new(&root);
        builder
            .standard_filters(false)
            .follow_links(false)
            .filter_entry(move |entry| {
                if is_git_meta(entry.file_name()) {
                    return false;
                }
                let rel = match entry.path().strip_prefix(&root) {
                    Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                    Err(_) => return true,
                };
                if rel.is_empty() {
                    return true;
                }
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                match &gitignore {
                    Some(gitignore) => !gitignore.matched(&rel, is_dir).is_ignore(),
                    None => true,
                }
            });
        builder.build()
    };
    Ok((root, walker))
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

fn is_git_meta(name: &std::ffi::OsStr) -> bool {
    name == ".git" || name == ".gitignore"
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::link;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autoground-ns-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn a_namespace_is_found_by_name_from_any_path() {
        let root = scratch("found");
        std::fs::create_dir_all(root.join("weird/place")).unwrap();
        std::fs::create_dir_all(root.join("docs/nested")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join(".gitignore"), "secret/\n").unwrap();
        std::fs::write(
            root.join("weird/place/notes.brief"),
            r#"
(ns com.acme.product)
(name Frontend)
(sense part_of "{X} is part of {Y}")
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("docs/nested/guide.md"),
            r#"
# Guide

```brief
(ns com.acme.client
  (:require [com.acme.product :as product]))
(name Backend)
(assume (product/part_of product/Frontend Backend))
```

```
(ns com.acme.ignored)
```
"#,
        )
        .unwrap();
        std::fs::write(root.join("secret/hidden.brief"), "(ns com.acme.hidden)\n").unwrap();
        std::fs::write(root.join("legacy.brief"), "(declare component \"{X}\")\n").unwrap();
        std::fs::write(root.join("plain.md"), "no fences\n").unwrap();

        let files = scan_namespaces(&root).unwrap();
        let client = files
            .iter()
            .find(|file| file.path == "docs/nested/guide.md")
            .unwrap();
        let compiled = link(&client.program, Some(client.path.as_str()), &files).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            files
                .iter()
                .any(|file| file.path == "weird/place/notes.brief"),
            "{:?}",
            files.iter().map(|file| &file.path).collect::<Vec<_>>()
        );
        assert!(files.iter().all(|file| file.path != "secret/hidden.brief"));
        assert_eq!(
            compiled.claims,
            vec![crate::syntax::Proposition::Assume(
                "Frontend is part of Backend".into()
            )]
        );
    }

    #[test]
    fn a_second_declaration_names_both_paths() {
        let root = scratch("dup");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("weird/place")).unwrap();
        std::fs::write(
            root.join("docs/guide.md"),
            "```brief\n(ns com.acme.product)\n```\n",
        )
        .unwrap();
        std::fs::write(
            root.join("weird/place/notes.brief"),
            "(ns com.acme.product)\n",
        )
        .unwrap();
        let files = scan_namespaces(&root).unwrap();
        let err = link(&files[0].program, Some(files[0].path.as_str()), &files).unwrap_err();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            err.message.contains("docs/guide.md")
                && err.message.contains("weird/place/notes.brief")
                && err.message.contains("com.acme.product"),
            "{err}"
        );
    }

    #[test]
    fn scan_briefs_lists_every_model() {
        let root = scratch("briefs");
        std::fs::create_dir_all(root.join("weird/place")).unwrap();
        std::fs::create_dir_all(root.join("docs/nested")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "secret/\n").unwrap();
        std::fs::write(root.join("weird/place/notes.brief"), "(name Frontend)\n").unwrap();
        std::fs::write(
            root.join("docs/nested/guide.md"),
            "# Guide\n\n```brief\n\"a note\"\n```\n",
        )
        .unwrap();
        std::fs::write(root.join("broken.brief"), "(claim\n").unwrap();
        std::fs::write(root.join("plain.md"), "no fences\n").unwrap();
        std::fs::write(root.join("secret/hidden.brief"), "(name Hidden)\n").unwrap();
        std::fs::write(root.join(".git/hidden.brief"), "(name Git)\n").unwrap();

        let files = scan_briefs(&root).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            paths,
            vec![
                "broken.brief",
                "docs/nested/guide.md",
                "weird/place/notes.brief",
            ]
        );
    }
}
