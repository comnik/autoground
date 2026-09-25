//! A refinable view over a tree of databases.
//!
//! Files, directories, tool call results, and other databases are all serializable.
//! Only some (e.g. directories) have true summaries. A database with a summary
//! is classified from that text. A database without one is read.
//!
//! Database ids are path-like strings, e.g. POSIX relative paths for a filesystem
//! (`frontend/app.py`, root `""`), or synthetic ids like `p0/c0/00-call-read_file` for a
//! tool call in an agent transcript.

pub mod fs;
pub mod transcript;

use std::fmt;
use std::io;

pub use fs::Filesystem;
pub use transcript::Transcript;

pub trait Universe {
    /// IDs of all databases in the current view.
    fn databases(&self) -> &[String];

    fn parent(&self, id: &str) -> Option<String>;
    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError>;

    /// Text search can classify. `Ok(None)` means this database has no summary.
    fn summary(&self, id: &str) -> Result<Option<String>, UniverseError>;

    /// Byte length of the current view.
    ///
    /// A summarizable database contributes its summary's length. Any other database
    /// contributes its content length. Bodies are not read.
    fn size(&self) -> Result<usize, UniverseError>;

    /// Full text of a database that has no summary.
    ///
    /// A longer body is head-and-tail trimmed to `budget`. `usize::MAX` reads the
    /// whole body. A database that has a summary cannot be read.
    fn read(&self, id: &str, budget: usize) -> Result<String, UniverseError>;

    /// The view can be refined by setting, inserting, and removing the active databases.
    fn insert(&mut self, id: String) -> Result<(), UniverseError>;
    fn remove(&mut self, id: &str);
    fn set(&mut self, ids: Vec<String>) -> Result<(), UniverseError>;
}

#[derive(Debug)]
pub enum UniverseError {
    Io(io::Error),
    Missing(String),
    /// `read` was asked for a database that has a summary.
    Summarizable(String),
}

impl fmt::Display for UniverseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UniverseError::Io(e) => write!(f, "{e}"),
            UniverseError::Missing(id) => {
                if id.is_empty() {
                    write!(f, "no such database: /")
                } else {
                    write!(f, "no such database: {id}")
                }
            }
            UniverseError::Summarizable(id) => {
                if id.is_empty() {
                    write!(f, "cannot read /: it has a summary")
                } else {
                    write!(f, "cannot read {id}: it has a summary")
                }
            }
        }
    }
}

impl std::error::Error for UniverseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UniverseError::Io(e) => Some(e),
            UniverseError::Missing(_) | UniverseError::Summarizable(_) => None,
        }
    }
}

impl From<io::Error> for UniverseError {
    fn from(e: io::Error) -> Self {
        UniverseError::Io(e)
    }
}
