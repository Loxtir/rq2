//! Sent requests on disk: `<id>.json` metadata plus `<id>.body`, pruned
//! oldest-first once bodies exceed a size limit. Metadata outlives its body,
//! so a list entry can say "response no longer kept".

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_LIMIT: u64 = 250 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: String,
    /// Missing in older history; an empty name means no environment.
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub deployment: Option<collection::deployment::Target>,
    pub timestamp: u64,
    pub name: String,
    #[serde(default)]
    pub collection: String,
    #[serde(default)]
    pub path: Vec<usize>,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub content_type: Option<String>,
    pub insecure: bool,
    pub missing: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub response_available: bool,
}

#[derive(Serialize)]
pub(crate) struct Listing {
    pub entries: Vec<Entry>,
    pub bytes: u64,
    pub limit: u64,
}

pub(crate) struct History {
    dir: PathBuf,
}

impl History {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn limit(&self) -> u64 {
        std::fs::read(self.dir.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<u64>(&bytes).ok())
            .unwrap_or(DEFAULT_LIMIT)
    }

    pub fn set_limit(&self, limit: u64) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        std::fs::write(self.dir.join("settings.json"), limit.to_string())
            .map_err(|error| error.to_string())?;
        self.prune(limit);
        Ok(())
    }

    /// Newest first, optionally narrowed to one collection or request.
    pub fn list(&self, collection: Option<&str>, path: Option<&[usize]>) -> Listing {
        let entries: Vec<Entry> = self
            .entries()
            .into_iter()
            .filter(|entry| collection.is_none_or(|slug| entry.collection == slug))
            .filter(|entry| path.is_none_or(|path| entry.path == path))
            .collect();
        let bytes = entries.iter().fold(0u64, |sum, entry| {
            sum.saturating_add(self.body_bytes(entry))
        });
        Listing {
            entries,
            bytes,
            limit: self.limit(),
        }
    }

    /// Stores `entry` with `body`, assigning its id and time, then prunes.
    /// Returns the id, or `None` if nothing could be written: history is a
    /// convenience and never fails a send.
    pub fn record(&self, mut entry: Entry, body: &[u8]) -> Option<String> {
        std::fs::create_dir_all(&self.dir).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        let id = format!("{}-{}", now.as_nanos(), std::process::id());
        entry.id = id.clone();
        entry.timestamp = now.as_secs();
        entry.response_available = entry.error.is_none();
        let body_path = self.body_path(&id);
        if entry.response_available {
            std::fs::write(&body_path, body).ok()?;
        }
        let metadata = serde_json::to_vec(&entry).ok()?;
        if std::fs::write(self.metadata_path(&id), metadata).is_err() {
            let _ = std::fs::remove_file(body_path);
            return None;
        }
        self.prune(self.limit());
        Some(id)
    }

    /// The entry and its body. A failed request has no body to lose.
    pub fn open(&self, id: &str) -> Result<(Entry, Vec<u8>), String> {
        let metadata = std::fs::read(self.metadata_path(id))
            .map_err(|_| "history entry was removed".to_string())?;
        let entry: Entry = serde_json::from_slice(&metadata).map_err(|error| error.to_string())?;
        let body = match std::fs::read(self.body_path(id)) {
            Ok(body) => body,
            Err(_) if entry.error.is_some() => Vec::new(),
            Err(_) => return Err("response body was removed by history retention".to_string()),
        };
        Ok((entry, body))
    }

    /// Removes every entry but keeps the size limit.
    pub fn clear(&self) -> Result<(), String> {
        let Ok(items) = std::fs::read_dir(&self.dir) else {
            return Ok(());
        };
        for item in items.flatten() {
            if item.file_name() != "settings.json" {
                std::fs::remove_file(item.path()).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn metadata_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn body_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.body"))
    }

    fn body_bytes(&self, entry: &Entry) -> u64 {
        std::fs::metadata(self.body_path(&entry.id))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    fn entries(&self) -> Vec<Entry> {
        let Ok(files) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut entries: Vec<Entry> = files
            .flatten()
            .filter(|item| is_extension(&item.path(), "json"))
            .filter(|item| item.file_name() != "settings.json")
            .filter_map(|item| std::fs::read(item.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
            .collect();
        for entry in &mut entries {
            entry.response_available = self.body_path(&entry.id).is_file();
        }
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
        entries
    }

    /// Drops bodies oldest-first until the total fits, keeping their metadata,
    /// then removes bodies no entry refers to. Best effort: a file that cannot
    /// be removed now is retried on the next prune.
    fn prune(&self, limit: u64) {
        let mut entries = self.entries();
        entries.reverse();
        let mut total = entries.iter().fold(0u64, |sum, entry| {
            sum.saturating_add(self.body_bytes(entry))
        });
        let mut known = HashSet::new();
        for mut entry in entries {
            known.insert(entry.id.clone());
            let bytes = self.body_bytes(&entry);
            if total <= limit || bytes == 0 {
                continue;
            }
            if std::fs::remove_file(self.body_path(&entry.id)).is_err() {
                continue;
            }
            entry.response_available = false;
            if let Ok(metadata) = serde_json::to_vec(&entry) {
                let _ = std::fs::write(self.metadata_path(&entry.id), metadata);
            }
            total = total.saturating_sub(bytes);
        }
        let Ok(files) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for file in files.flatten() {
            let path = file.path();
            let orphan = is_extension(&path, "body")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|id| !known.contains(id));
            if orphan {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn is_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|found| found == extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(tag: &str) -> History {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        History::new(
            std::env::temp_dir().join(format!("rq-history-{tag}-{}-{nanos}", std::process::id())),
        )
    }

    fn entry(name: &str, collection: &str) -> Entry {
        Entry {
            name: name.into(),
            collection: collection.into(),
            path: vec![0],
            method: "GET".into(),
            status: 200,
            ..Default::default()
        }
    }

    #[test]
    fn a_recorded_response_lists_and_opens() {
        let history = history("open");
        let id = history.record(entry("Get", "demo"), b"{}").unwrap();
        let listing = history.list(Some("demo"), Some(&[0]));
        assert_eq!(listing.entries.len(), 1);
        assert!(listing.entries[0].response_available);
        assert_eq!(listing.bytes, 2);
        assert!(history.list(Some("other"), None).entries.is_empty());
        assert_eq!(history.open(&id).unwrap().1, b"{}");
        std::fs::remove_dir_all(&history.dir).unwrap();
    }

    #[test]
    fn a_failure_is_listed_without_a_response() {
        let history = history("failure");
        let mut failed = entry("Get", "demo");
        failed.error = Some("connection refused".into());
        let id = history.record(failed, &[]).unwrap();
        assert!(!history.list(None, None).entries[0].response_available);
        let (opened, body) = history.open(&id).unwrap();
        assert_eq!(opened.error.as_deref(), Some("connection refused"));
        assert!(body.is_empty());
        std::fs::remove_dir_all(&history.dir).unwrap();
    }

    #[test]
    fn the_limit_drops_the_oldest_bodies_but_keeps_their_entries() {
        let history = history("prune");
        let old = history.record(entry("Old", "demo"), &[b'a'; 10]).unwrap();
        let new = history.record(entry("New", "demo"), &[b'b'; 10]).unwrap();
        history.set_limit(15).unwrap();
        assert_eq!(history.limit(), 15);
        assert!(history.open(&old).unwrap_err().contains("retention"));
        assert_eq!(history.open(&new).unwrap().1, [b'b'; 10]);
        assert_eq!(history.list(None, None).entries.len(), 2);

        history.clear().unwrap();
        assert!(history.list(None, None).entries.is_empty());
        assert_eq!(history.limit(), 15, "clearing keeps the setting");
        std::fs::remove_dir_all(&history.dir).unwrap();
    }
}
