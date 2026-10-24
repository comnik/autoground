//! Where a witness lives.
//!
//! [`Universe::stat`] reports a file's length. [`Universe::read`] returns its
//! bytes. `Ok(None)` from `stat` means this id is not a file witness.
//!
//! [`fs::Filesystem`] is rooted at a directory and resolves a relative id
//! against it. [`fs::GivenFile`] is the one file a caller supplied.

pub mod fs;

use std::fmt;
use std::io;

pub use fs::{Filesystem, GivenFile};

/// Length of a file witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    pub len: u64,
}

pub trait Universe {
    fn stat(&self, id: &str) -> Result<Option<FileInfo>, UniverseError>;
    fn read(&self, id: &str) -> Result<Vec<u8>, UniverseError>;
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
