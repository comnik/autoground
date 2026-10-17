//! A refinable view over a tree of databases.
//!
//! Files and directories are serializable. A directory has a summary: its
//! listing. A file does not, and is read.
//!
//! Database ids are POSIX relative paths (`frontend/app.py`, root `""`).

pub mod diff;
pub mod fs;
pub mod transcript;

use std::fmt;
use std::io;

pub use diff::Diff;
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
    /// A body longer than `budget` is [`UniverseError::TooLarge`]. `usize::MAX`
    /// reads the whole body. A database that has a summary cannot be read.
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
    /// `read` was asked for a body longer than its budget.
    TooLarge {
        id: String,
        bytes: usize,
    },
}

/// Return `text` when it fits in `budget`. A longer body is not trimmed.
pub(crate) fn within_budget(
    id: &str,
    text: String,
    budget: usize,
) -> Result<String, UniverseError> {
    if text.len() > budget {
        Err(UniverseError::TooLarge {
            id: id.to_string(),
            bytes: text.len(),
        })
    } else {
        Ok(text)
    }
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
            UniverseError::TooLarge { id, bytes } => {
                write!(f, "{id} is {bytes} bytes, over the read budget")
            }
        }
    }
}

impl std::error::Error for UniverseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UniverseError::Io(e) => Some(e),
            UniverseError::Missing(_)
            | UniverseError::Summarizable(_)
            | UniverseError::TooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for UniverseError {
    fn from(e: io::Error) -> Self {
        UniverseError::Io(e)
    }
}
