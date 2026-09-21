//! A refinable view over a tree of databases.
//!
//! Files and directories are the same kind of object: a serializable database.
//! Leaves render as path plus contents; non-leaves render as a listing.

pub mod fs;

use std::collections::BTreeSet;
use std::fmt;
use std::io;

pub use fs::Filesystem;

/// A refinable view over a universe of databases.
///
/// Object ids are path-like strings. The filesystem uses POSIX relative paths
/// (`frontend/app.py`, root `""`). Oracle classification is not part of this
/// trait: callers `insert` / `remove` / `set` after the oracle labels objects.
pub trait Universe {
    fn objects(&self) -> &[String];

    fn parent(&self, id: &str) -> Option<String>;
    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError>;

    /// Cheap size of the current view (objects plus samples). File bodies are
    /// not read: files use metadata length plus header overhead.
    fn estimate(&self) -> Result<usize, UniverseError>;
    /// Exact size: `render()?.len()`.
    fn size(&self) -> Result<usize, UniverseError>;
    fn render(&self) -> Result<String, UniverseError>;

    /// Replace every object with its immediate parent. Root stays root.
    fn collapse(&mut self);
    /// Fill leftover budget with children of current objects (one level).
    ///
    /// Samples are extra render context, not classification targets.
    fn sample(&mut self, extra_budget: usize) -> Result<(), UniverseError>;

    fn insert(&mut self, id: String) -> Result<(), UniverseError>;
    fn remove(&mut self, id: &str);
    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError>;
}

#[derive(Debug)]
pub enum UniverseError {
    Io(io::Error),
    Missing(String),
}

impl fmt::Display for UniverseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UniverseError::Io(e) => write!(f, "{e}"),
            UniverseError::Missing(id) => {
                if id.is_empty() {
                    write!(f, "no such object: /")
                } else {
                    write!(f, "no such object: {id}")
                }
            }
        }
    }
}

impl std::error::Error for UniverseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UniverseError::Io(e) => Some(e),
            UniverseError::Missing(_) => None,
        }
    }
}

impl From<io::Error> for UniverseError {
    fn from(e: io::Error) -> Self {
        UniverseError::Io(e)
    }
}

/// One collapse step: each id becomes its parent; root stays.
pub(crate) fn collapse_ids(ids: &[String], parent: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut next = BTreeSet::new();
    for id in ids {
        match parent(id) {
            Some(p) => {
                next.insert(p);
            }
            None => {
                next.insert(id.clone());
            }
        }
    }
    next.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::collapse_ids;

    fn fs_parent(id: &str) -> Option<String> {
        if id.is_empty() {
            None
        } else {
            Some(
                id.rsplit_once('/')
                    .map(|(p, _)| p)
                    .unwrap_or("")
                    .to_string(),
            )
        }
    }

    #[test]
    fn collapse_files_to_parent_dirs() {
        let ids = vec![
            "frontend/app.py".into(),
            "frontend/__init__.py".into(),
            "backend/api.py".into(),
        ];
        assert_eq!(
            collapse_ids(&ids, fs_parent),
            vec!["backend".to_string(), "frontend".to_string()]
        );
    }

    #[test]
    fn collapse_dirs_to_root() {
        let ids = vec!["backend".into(), "decoy".into(), "frontend".into()];
        assert_eq!(collapse_ids(&ids, fs_parent), vec!["".to_string()]);
    }

    #[test]
    fn collapse_root_is_identity() {
        let ids = vec!["".to_string()];
        assert_eq!(collapse_ids(&ids, fs_parent), vec!["".to_string()]);
    }
}
