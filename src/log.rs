//! Append-only session log.
//!
//! One JSON object per line. [`Log::create_under`] writes `trace.jsonl`
//! inside a fresh `{nanos}-{pid}-{n}` directory.
//! [`Log`] is cheap to clone and safe to share across threads: each append
//! takes the mutex, writes one line, and flushes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

pub const DEFAULT_TRACE_DIR: &str = "/tmp/autoground";

/// Create `{parent}/{nanos}-{pid}-{n}` and return that directory.
pub fn allocate_unique_dir(parent: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(parent)?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for n in 0u32..10_000 {
        let path = parent.join(format!("{nanos}-{pid}-{n}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique session directory",
    ))
}

struct Shared {
    file: Mutex<File>,
    /// Called after each appended line, without the file lock held.
    hook: Mutex<Option<Arc<dyn Fn(&Value) + Send + Sync>>>,
}

/// Handle for one session log. Clone shares the same file.
#[derive(Clone)]
pub struct Log {
    path: PathBuf,
    shared: Arc<Shared>,
}

impl Log {
    /// `{dir}/{nanos}-{pid}-{n}/trace.jsonl`.
    pub fn create_under(dir: &Path) -> std::io::Result<Self> {
        let session = allocate_unique_dir(dir)?;
        Self::open(session.join("trace.jsonl"))
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
            shared: Arc::new(Shared {
                file: Mutex::new(file),
                hook: Mutex::new(None),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Invoke `hook` after every later append on this log, including clones.
    ///
    /// The hook runs without the file lock. It must not append to this same
    /// log: that calls the hook again.
    pub fn set_hook<F>(&self, hook: F)
    where
        F: Fn(&Value) + Send + Sync + 'static,
    {
        *self.hook_slot() = Some(Arc::new(hook));
    }

    pub fn clear_hook(&self) {
        *self.hook_slot() = None;
    }

    fn hook_slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<dyn Fn(&Value) + Send + Sync>>> {
        self.shared
            .hook
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Append one JSON value as a single line.
    pub fn append(&self, event: &impl Serialize) -> std::io::Result<()> {
        let value = serde_json::to_value(event).map_err(json_io)?;
        let mut buf = serde_json::to_vec(&value).map_err(json_io)?;
        buf.push(b'\n');
        {
            let mut file = self
                .shared
                .file
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            file.write_all(&buf)?;
            file.flush()?;
        }
        self.emit(&value);
        Ok(())
    }

    fn emit(&self, value: &Value) {
        let hook = self.hook_slot().clone();
        if let Some(hook) = hook {
            hook(value);
        }
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

/// Parse a JSONL log. Each non-empty line is one JSON value.
pub fn parse_jsonl(text: &str) -> Result<Vec<Value>, String> {
    let mut events = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_str(line).map_err(|err| format!("line {}: {err}", i + 1))?;
        events.push(value);
    }
    if events.is_empty() {
        return Err("empty log".into());
    }
    Ok(events)
}

pub fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse_jsonl(&text)
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

    #[test]
    fn hook_sees_each_appended_event() {
        let path = std::env::temp_dir().join(format!(
            "autoground-log-hook-{}-{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = Log::open(&path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let got = Arc::clone(&seen);
        log.set_hook(move |event| {
            got.lock()
                .unwrap()
                .push(event["kind"].as_str().unwrap().to_string());
        });
        log.append_event("search_step", &serde_json::json!({"id": "frontend"}))
            .unwrap();
        let other = log.clone();
        other
            .append_event("read_step", &serde_json::json!({"path": "a.py"}))
            .unwrap();
        log.clear_hook();
        log.append_event("run", &serde_json::json!({})).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["search_step", "read_step"]
        );
    }

    #[test]
    fn create_under_puts_trace_jsonl_in_a_fresh_directory() {
        let parent =
            std::env::temp_dir().join(format!("autoground-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let a = Log::create_under(&parent).unwrap();
        let b = Log::create_under(&parent).unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(
            a.path().file_name().and_then(|name| name.to_str()),
            Some("trace.jsonl")
        );
        assert_eq!(
            b.path().file_name().and_then(|name| name.to_str()),
            Some("trace.jsonl")
        );
        assert_eq!(
            a.path().parent().and_then(|dir| dir.parent()),
            Some(parent.as_path())
        );
        assert_ne!(a.path().parent(), b.path().parent());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
