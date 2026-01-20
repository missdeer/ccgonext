//! OpenCode log provider
//!
//! Observed storage layout:
//!   storage/session/<projectID>/ses_*.json
//!   storage/message/<sessionID>/msg_*.json
//!   storage/part/<messageID>/prt_*.json
//!
//! Instead of computing projectID (which requires git and is fragile),
//! we monitor the entire storage directory and find the most recently
//! updated session file.

use super::{HistoryEntry, LockedSession, LogEntry, LogProvider, PathMapper};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct OpenCodeLogProvider {
    storage_root: PathBuf,
    current_offset: Arc<AtomicU64>,
    locked_session: Arc<Mutex<Option<String>>>,
}

impl OpenCodeLogProvider {
    pub fn new(config: Option<&HashMap<String, String>>) -> Self {
        let storage_root = if let Some(cfg) = config {
            if let Some(path) = cfg.get("path_pattern") {
                PathMapper::normalize(path)
            } else {
                Self::default_storage_root()
            }
        } else {
            Self::default_storage_root()
        };

        tracing::info!(
            "[OpenCodeLogProvider] Initialized with storage_root={:?}",
            storage_root
        );

        Self {
            storage_root,
            current_offset: Arc::new(AtomicU64::new(0)),
            locked_session: Arc::new(Mutex::new(None)),
        }
    }

    fn default_storage_root() -> PathBuf {
        if let Ok(root) = std::env::var("OPENCODE_STORAGE_ROOT") {
            if !root.is_empty() {
                return PathMapper::normalize(&root);
            }
        }

        #[cfg(windows)]
        {
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                let path = PathBuf::from(&local_app_data)
                    .join("opencode")
                    .join("storage");
                if path.exists() {
                    return path;
                }
            }
            if let Ok(app_data) = std::env::var("APPDATA") {
                let path = PathBuf::from(&app_data).join("opencode").join("storage");
                if path.exists() {
                    return path;
                }
            }
        }

        PathMapper::normalize("~/.local/share/opencode/storage")
    }

    fn session_dir(&self) -> PathBuf {
        self.storage_root.join("session")
    }

    fn message_dir(&self, session_id: &str) -> PathBuf {
        let nested = self.storage_root.join("message").join(session_id);
        if nested.exists() {
            nested
        } else {
            self.storage_root.join("message")
        }
    }

    fn part_dir(&self, message_id: &str) -> PathBuf {
        let nested = self.storage_root.join("part").join(message_id);
        if nested.exists() {
            nested
        } else {
            self.storage_root.join("part")
        }
    }

    fn load_json(&self, path: &PathBuf) -> Option<serde_json::Value> {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn get_latest_session(&self) -> Option<(PathBuf, serde_json::Value)> {
        let session_dir = self.session_dir();
        tracing::debug!(
            "[OpenCodeLogProvider] Scanning for sessions in {:?}",
            session_dir
        );

        if !session_dir.exists() {
            tracing::debug!(
                "[OpenCodeLogProvider] Session dir does not exist: {:?}",
                session_dir
            );
            return None;
        }

        let mut best: Option<(PathBuf, serde_json::Value, i64)> = None;
        let mut project_count = 0u32;
        let mut session_count = 0u32;

        // Scan all project directories - always pick the most recently updated session
        for project_entry in fs::read_dir(&session_dir).ok()?.filter_map(|e| e.ok()) {
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            project_count += 1;

            // Scan session files in each project directory
            for session_entry in fs::read_dir(&project_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
            {
                let name = session_entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("ses_") || !name.ends_with(".json") {
                    continue;
                }

                session_count += 1;
                let path = session_entry.path();

                if let Some(json) = self.load_json(&path) {
                    let updated = json
                        .get("time")
                        .and_then(|t| t.get("updated"))
                        .and_then(|u| u.as_i64())
                        .unwrap_or(-1);

                    // Always pick the most recently updated session (no time window)
                    if best.is_none() || updated > best.as_ref().unwrap().2 {
                        best = Some((path, json, updated));
                    }
                }
            }
        }

        tracing::debug!(
            "[OpenCodeLogProvider] Scanned {} project dirs, {} total sessions, latest: {:?}",
            project_count,
            session_count,
            best.as_ref().map(|(p, _, _)| p)
        );

        best.map(|(path, json, _)| (path, json))
    }

    fn get_session_by_id(&self, session_id: &str) -> Option<(PathBuf, serde_json::Value)> {
        let session_dir = self.session_dir();
        if !session_dir.exists() {
            return None;
        }

        for project_entry in fs::read_dir(&session_dir).ok()?.filter_map(|e| e.ok()) {
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            for session_entry in fs::read_dir(&project_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
            {
                let name = session_entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("ses_") || !name.ends_with(".json") {
                    continue;
                }

                let path = session_entry.path();
                if let Some(json) = self.load_json(&path) {
                    if json.get("id").and_then(|i| i.as_str()) == Some(session_id) {
                        return Some((path, json));
                    }
                }
            }
        }

        None
    }

    fn read_messages(&self, session_id: &str) -> Vec<serde_json::Value> {
        let message_dir = self.message_dir(session_id);
        if !message_dir.exists() {
            return Vec::new();
        }

        let mut messages = Vec::new();

        let entries: Vec<_> = fs::read_dir(&message_dir)
            .ok()
            .map(|r| r.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();

        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("msg_") || !name.ends_with(".json") {
                continue;
            }

            let path = entry.path();
            if let Some(json) = self.load_json(&path) {
                if json.get("sessionID").and_then(|s| s.as_str()) == Some(session_id) {
                    messages.push(json);
                }
            }
        }

        messages.sort_by(|a, b| {
            let get_created = |m: &serde_json::Value| {
                m.get("time")
                    .and_then(|t| t.get("created"))
                    .and_then(|c| c.as_i64())
                    .unwrap_or(-1)
            };
            get_created(a).cmp(&get_created(b))
        });

        messages
    }

    fn read_parts(&self, message_id: &str) -> Vec<serde_json::Value> {
        let part_dir = self.part_dir(message_id);
        if !part_dir.exists() {
            return Vec::new();
        }

        let mut parts = Vec::new();

        let entries: Vec<_> = fs::read_dir(&part_dir)
            .ok()
            .map(|r| r.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();

        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("prt_") || !name.ends_with(".json") {
                continue;
            }

            let path = entry.path();
            if let Some(json) = self.load_json(&path) {
                if json.get("messageID").and_then(|s| s.as_str()) == Some(message_id) {
                    parts.push(json);
                }
            }
        }

        parts.sort_by(|a, b| {
            let get_start = |p: &serde_json::Value| {
                p.get("time")
                    .and_then(|t| t.get("start"))
                    .and_then(|s| s.as_i64())
                    .unwrap_or(-1)
            };
            get_start(a).cmp(&get_start(b))
        });

        parts
    }

    fn extract_text(&self, parts: &[serde_json::Value], allow_reasoning_fallback: bool) -> String {
        let collect = |types: &[&str]| -> String {
            parts
                .iter()
                .filter(|p| {
                    p.get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| types.contains(&t))
                        .unwrap_or(false)
                })
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string()
        };

        let text = collect(&["text"]);
        if !text.is_empty() {
            return text;
        }

        if allow_reasoning_fallback {
            return collect(&["reasoning"]);
        }

        String::new()
    }

    fn get_assistant_entries(
        &self,
        locked_session_id: Option<&str>,
    ) -> Vec<(String, String, DateTime<Utc>)> {
        let (_, session) = if let Some(sid) = locked_session_id {
            // Use locked session ID to find session
            match self.get_session_by_id(sid) {
                Some(s) => s,
                None => return Vec::new(),
            }
        } else {
            match self.get_latest_session() {
                Some(s) => s,
                None => return Vec::new(),
            }
        };

        let session_id = session
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or_default();
        if session_id.is_empty() {
            return Vec::new();
        }

        let messages = self.read_messages(session_id);
        let mut entries = Vec::new();

        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or_default();
            if role != "assistant" {
                continue;
            }

            let message_id = msg.get("id").and_then(|i| i.as_str()).unwrap_or_default();
            if message_id.is_empty() {
                continue;
            }

            let completed = msg
                .get("time")
                .and_then(|t| t.get("completed"))
                .and_then(|c| c.as_i64());
            if completed.is_none() {
                continue;
            }

            let parts = self.read_parts(message_id);
            let text = self.extract_text(&parts, true);
            if text.is_empty() {
                continue;
            }

            let timestamp = completed
                .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
                .unwrap_or_else(Utc::now);

            entries.push(("assistant".to_string(), text, timestamp));
        }

        entries
    }
}

#[async_trait]
impl LogProvider for OpenCodeLogProvider {
    async fn get_latest_reply(&self, since_offset: u64) -> Option<LogEntry> {
        tracing::debug!(
            "[OpenCodeLogProvider] get_latest_reply called with since_offset={}",
            since_offset
        );

        // Use locked session ID if available
        let locked_session_id = self.locked_session.lock().await.clone();
        let entries = self.get_assistant_entries(locked_session_id.as_deref());
        let total_messages = entries.len() as u64;

        tracing::debug!(
            "[OpenCodeLogProvider] Found {} total assistant entries",
            total_messages
        );

        let new_entries: Vec<_> = entries
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| *idx as u64 >= since_offset)
            .collect();

        tracing::debug!(
            "[OpenCodeLogProvider] {} new entries since offset {}",
            new_entries.len(),
            since_offset
        );

        let result = new_entries
            .into_iter()
            .last()
            .map(|(idx, (_, content, timestamp))| LogEntry {
                content,
                offset: idx as u64 + 1,
                timestamp,
                inode: self.get_inode(),
            });

        if result.is_some() {
            self.current_offset.store(total_messages, Ordering::SeqCst);
            tracing::info!(
                "[OpenCodeLogProvider] Found assistant reply, new offset={}",
                total_messages
            );
        } else {
            tracing::debug!("[OpenCodeLogProvider] No new assistant reply found");
        }

        result
    }

    async fn get_history(&self, _session_id: Option<&str>, count: usize) -> Vec<HistoryEntry> {
        let locked_session_id = self.locked_session.lock().await.clone();
        let entries = self.get_assistant_entries(locked_session_id.as_deref());

        let mut history: Vec<HistoryEntry> = entries
            .into_iter()
            .map(|(role, content, timestamp)| HistoryEntry {
                role,
                content,
                timestamp,
            })
            .collect();

        history.reverse();
        history.truncate(count);
        history.reverse();

        history
    }

    async fn get_current_offset(&self) -> u64 {
        let locked_session_id = self.locked_session.lock().await.clone();
        self.get_assistant_entries(locked_session_id.as_deref())
            .len() as u64
    }

    fn get_inode(&self) -> Option<u64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.get_latest_session()
                .and_then(|(p, _)| fs::metadata(&p).ok())
                .map(|m| m.ino())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn get_watch_path(&self) -> Option<PathBuf> {
        Some(self.storage_root.clone())
    }

    async fn lock_session(&self) -> Option<LockedSession> {
        let (path, session) = self.get_latest_session()?;
        let session_id = session.get("id").and_then(|i| i.as_str())?;
        let baseline_offset = self.get_assistant_entries(Some(session_id)).len() as u64;

        // Store locked session ID
        *self.locked_session.lock().await = Some(session_id.to_string());

        tracing::info!(
            "[OpenCodeLogProvider] Session locked: {:?}, session_id={}, baseline_offset={}",
            path,
            session_id,
            baseline_offset
        );

        Some(LockedSession {
            file_path: path,
            baseline_offset,
        })
    }

    async fn unlock_session(&self) {
        let mut locked = self.locked_session.lock().await;
        if locked.is_some() {
            tracing::debug!("[OpenCodeLogProvider] Session unlocked");
        }
        *locked = None;
    }
}
