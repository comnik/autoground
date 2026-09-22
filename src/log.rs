//! Append-only session log.
//!
//! One JSON object per line, in a file under `/tmp/autoground`. [`Log`] is
//! cheap to clone and safe to share across threads: each append takes the
//! mutex, writes one line, and flushes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

pub const DEFAULT_TRACE_DIR: &str = "/tmp/autoground";

/// Create a new uniquely named file under `dir` and return its path.
pub fn allocate_unique_file(dir: &Path, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ext = ext.trim_start_matches('.');
    for n in 0u32..10_000 {
        let path = dir.join(format!("trace-{nanos}-{pid}-{n}.{ext}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique trace filename",
    ))
}

/// Handle for one session log. Clone shares the same file.
#[derive(Clone)]
pub struct Log {
    path: PathBuf,
    file: Arc<Mutex<File>>,
}

impl Log {
    /// `trace-{nanos}-{pid}-{n}.jsonl` under [`DEFAULT_TRACE_DIR`].
    pub fn create() -> std::io::Result<Self> {
        let path = allocate_unique_file(Path::new(DEFAULT_TRACE_DIR), "jsonl")?;
        Self::open(path)
    }

    /// Append to a new or existing file.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one JSON value as a single line.
    pub fn append(&self, event: &impl Serialize) -> std::io::Result<()> {
        let mut buf = serde_json::to_vec(event).map_err(json_io)?;
        buf.push(b'\n');
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        file.write_all(&buf)?;
        file.flush()
    }

    /// Append `body` as one JSON object with a `"kind"` field.
    ///
    /// `body` must serialize to an object. An existing `"kind"` is replaced.
    pub fn append_event(&self, kind: &str, body: &impl Serialize) -> std::io::Result<()> {
        let mut value = serde_json::to_value(body).map_err(json_io)?;
        let Some(obj) = value.as_object_mut() else {
            return Err(json_io("log event must be a JSON object"));
        };
        obj.insert("kind".to_string(), Value::String(kind.to_string()));
        self.append(&value)
    }
}

fn json_io(err: impl ToString) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
}

/// Parse JSONL. `Ok(None)` when the text is not line-delimited JSON, so a
/// pretty-printed document is left for the caller to read as one value.
pub fn parse_jsonl(text: &str) -> Result<Option<Vec<Value>>, String> {
    let mut events = Vec::new();
    let mut saw = false;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                saw = true;
                events.push(value);
            }
            Err(err) if saw => return Err(format!("line {}: {err}", i + 1)),
            Err(_) => return Ok(None),
        }
    }
    if events.is_empty() {
        Ok(None)
    } else {
        Ok(Some(events))
    }
}

pub fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    match parse_jsonl(&text)? {
        Some(events) => Ok(events),
        None => Err("not a jsonl log".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn appends_from_two_threads_stay_one_object_per_line() {
        let path = std::env::temp_dir().join(format!(
            "autoground-log-{}-{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = Log::open(&path).unwrap();
        let other = log.clone();
        let left = thread::spawn(move || {
            for n in 0..20 {
                log.append(&serde_json::json!({"n": n, "side": "a"}))
                    .unwrap();
            }
        });
        let right = thread::spawn(move || {
            for n in 0..20 {
                other
                    .append(&serde_json::json!({"n": n, "side": "b"}))
                    .unwrap();
            }
        });
        left.join().unwrap();
        right.join().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let events = read_events(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(events.len(), 40, "{text}");
        assert_eq!(text.lines().filter(|line| !line.is_empty()).count(), 40);
        assert_eq!(
            events.iter().filter(|event| event["side"] == "a").count(),
            20
        );
    }

    #[test]
    fn append_event_stamps_kind() {
        let path = std::env::temp_dir().join(format!(
            "autoground-log-kind-{}-{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = Log::open(&path).unwrap();
        log.append_event("listing", &serde_json::json!({"dir": "ui"}))
            .unwrap();
        let events = read_events(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(events[0]["kind"], "listing");
        assert_eq!(events[0]["dir"], "ui");
    }
}
