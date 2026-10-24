//! Write a directory of files and return its path.
//!
//! The directory stays after this returns. A failure before the path is
//! returned removes it.

#[path = "diff.rs"]
mod diff_files;
#[path = "transcript.rs"]
mod transcript_files;

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One file per tool call in `path`. `path` is `chat_history.jsonl`, or a
/// directory that contains it. Zero exchanges still return a directory.
pub fn transcript(path: &Path) -> Result<PathBuf, String> {
    let exchanges = transcript_files::exchanges(path)?;
    let dir = create_dir()?;
    let bodies: Vec<(&str, &str)> = exchanges
        .iter()
        .map(|exchange| (exchange.id.as_str(), exchange.body.as_str()))
        .collect();
    let written = write_bodies(&dir, &bodies);
    finish(dir, written)
}

/// One file per change in `git diff`, run from `root`. Arguments are appended
/// to `git diff`. An empty diff still returns a directory.
pub fn diff(root: &Path, git_args: &[String]) -> Result<PathBuf, String> {
    let patches = diff_files::changes(root, git_args, None)?;
    let dir = create_dir()?;
    let bodies: Vec<(&str, &str)> = patches
        .iter()
        .map(|patch| (patch.id.as_str(), patch.body.as_str()))
        .collect();
    let written = write_bodies(&dir, &bodies);
    finish(dir, written)
}

fn finish(dir: PathBuf, result: Result<(), String>) -> Result<PathBuf, String> {
    match result {
        Ok(()) => Ok(dir),
        Err(err) => {
            let _ = fs::remove_dir_all(&dir);
            Err(err)
        }
    }
}

fn create_dir() -> Result<PathBuf, String> {
    let parent = PathBuf::from("/tmp/autoground");
    fs::create_dir_all(&parent).map_err(|err| format!("creating {}: {err}", parent.display()))?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for n in 0u32..10_000 {
        let path = parent.join(format!("prepare-{nanos}-{pid}-{n}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("creating {}: {err}", path.display())),
        }
    }
    Err("could not allocate a prepare directory".into())
}

fn write_bodies(dir: &Path, bodies: &[(&str, &str)]) -> Result<(), String> {
    for (id, body) in bodies {
        let rel = Path::new(id);
        if rel.is_absolute()
            || rel
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!("`{id}` is not a relative path"));
        }
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("creating {}: {err}", parent.display()))?;
        }
        fs::write(&path, body).map_err(|err| format!("writing {}: {err}", path.display()))?;
    }
    Ok(())
}
