//! A refinable view over a tree of databases.
//!
//! Files and directories are the same kind of object: a serializable database.
//! Leaves render as path plus contents; non-leaves render as a listing.

pub mod fs;

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

    /// A leaf renders as path plus contents. A non-leaf renders as a listing.
    fn is_leaf(&self, id: &str) -> Result<bool, UniverseError>;

    /// Leaf byte length, or a directory listing. No file body.
    fn summary(&self, id: &str) -> Result<ObjectSummary, UniverseError>;

    /// Cheap size of the current view (objects plus samples). File bodies are
    /// not read: files use metadata length plus header overhead.
    fn estimate(&self) -> Result<usize, UniverseError>;
    /// Exact size: `render()?.len()`.
    fn size(&self) -> Result<usize, UniverseError>;
    fn render(&self) -> Result<String, UniverseError>;
    /// Render one object. A leaf larger than `budget` is head-and-tail trimmed.
    fn render_capped(&self, id: &str, budget: usize) -> Result<String, UniverseError>;

    /// Fill leftover budget with children of current objects (one level).
    ///
    /// Samples are extra render context. A sampled leaf is already in the
    /// window, so search may judge it without a second render.
    fn sample(&mut self, extra_budget: usize) -> Result<(), UniverseError>;

    fn samples(&self) -> &[String];

    fn insert(&mut self, id: String) -> Result<(), UniverseError>;
    fn remove(&mut self, id: &str);
    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError>;
}

/// What a trace can show for one database without its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSummary {
    pub leaf: bool,
    /// File length, or the listing's byte length.
    pub bytes: u64,
    /// Directory listing. Empty for a leaf.
    pub listing: String,
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
