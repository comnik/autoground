//! A tree of databases a caller can list.
//!
//! A directory has a summary: its listing. A file does not. Ids are POSIX
//! relative paths (`frontend/app.py`, root `""`).

pub mod fs;

use std::fmt;
use std::io;

pub use fs::Filesystem;

pub trait Universe {
    /// Ids of the databases in this universe.
    fn databases(&self) -> &[String];

    fn children(&self, id: &str) -> Result<Vec<String>, UniverseError>;

    /// Listing text. `Ok(None)` means this database has no summary.
    fn summary(&self, id: &str) -> Result<Option<String>, UniverseError>;
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
                    write!(f, "no such database: /")
                } else {
                    write!(f, "no such database: {id}")
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
