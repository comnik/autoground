//! The local filesystem as a universe of file witnesses.
//!
//! [`Filesystem`] joins a relative id onto its root. An absolute id is not a
//! file. [`GivenFile`] is one path opened as given, including an absolute path.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{FileInfo, Universe, UniverseError};

/// Witnesses that live under `root`.
#[derive(Clone)]
pub struct Filesystem {
    root: PathBuf,
}

impl Filesystem {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, id: &str) -> Option<PathBuf> {
        let path = Path::new(id);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::Prefix(_)))
        {
            return None;
        }
        Some(self.root.join(path))
    }
}

impl Universe for Filesystem {
    fn stat(&self, id: &str) -> Result<Option<FileInfo>, UniverseError> {
        let Some(path) = self.resolve(id) else {
            return Ok(None);
        };
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(Some(FileInfo { len: meta.len() })),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn read(&self, id: &str) -> Result<Vec<u8>, UniverseError> {
        let Some(path) = self.resolve(id) else {
            return Err(UniverseError::Missing(id.to_string()));
        };
        match fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Err(UniverseError::Missing(id.to_string()))
            }
            Err(err) => Err(err.into()),
        }
    }
}

/// The one file a caller supplied. `stat` and `read` open `path` and ignore the id.
#[derive(Clone)]
pub struct GivenFile {
    path: PathBuf,
}

impl GivenFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Universe for GivenFile {
    fn stat(&self, _id: &str) -> Result<Option<FileInfo>, UniverseError> {
        match fs::metadata(&self.path) {
            Ok(meta) if meta.is_file() => Ok(Some(FileInfo { len: meta.len() })),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn read(&self, id: &str) -> Result<Vec<u8>, UniverseError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Err(UniverseError::Missing(id.to_string()))
            }
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "autoground-universe-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_absolute_id_is_not_a_file() {
        let root = scratch("absolute");
        let file = root.join("note.txt");
        fs::write(&file, "alpha\n").unwrap();
        let fs = Filesystem::at(&root);
        assert_eq!(fs.stat(file.to_str().unwrap()).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let root = scratch("dir");
        fs::create_dir(root.join("src")).unwrap();
        let fs = Filesystem::at(&root);
        assert_eq!(fs.stat("src").unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_dir_reads_the_file() {
        let root = scratch("parent");
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(root.join("note.txt"), "alpha\n").unwrap();
        let fs = Filesystem::at(&nested);
        let info = fs.stat("../note.txt").unwrap().unwrap();
        assert_eq!(info.len, 6);
        assert_eq!(fs.read("../note.txt").unwrap(), b"alpha\n");
        let _ = fs::remove_dir_all(&root);
    }
}
