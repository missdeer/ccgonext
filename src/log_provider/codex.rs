//! Codex log provider

use super::{HistoryEntry, LockedSession, LogEntry, LogProvider, PathMapper};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct CodexLogProvider {
    log_path: PathBuf,
    current_offset: Arc<AtomicU64>,
    #[allow(dead_code)]
    file_handle: Arc<Mutex<Option<File>>>,
    locked_session: Arc<Mutex<Option<PathBuf>>>,
}

impl CodexLogProvider {
    pub fn new(config: Option<&HashMap<String, String>>) -> Self {
        let log_path = if let Some(cfg) = config {
            if let Some(path) = cfg.get("path_pattern") {
                PathMapper::normalize(path)
            } else {
                Self::default_log_path()
            }
        } else {
            Self::default_log_path()
        };

        Self {
            log_path,
            current_offset: Arc::new(AtomicU64::new(0)),
            file_handle: Arc::new(Mutex::new(None)),
            locked_session: Arc::new(Mutex::new(None)),
        }
    }

    fn default_log_path() -> PathBuf {
        PathMapper::normalize("~/.codex/sessions")
    }

    fn find_latest_session_file(&self) -> Option<PathBuf> {
        tracing::debug!(
            "[CodexLogProvider] Looking for session files in: {:?}",
            self.log_path
        );

        if !self.log_path.exists() {
            tracing::warn!(
                "[CodexLogProvider] Log path does not exist: {:?}",
                self.log_path
            );
            return None;
        }

        // Recursively find all .jsonl files
        fn find_jsonl_files(dir: &PathBuf, files: &mut Vec<PathBuf>) {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        find_jsonl_files(&path, files);
                    } else if path.extension().map(|ext| ext == "jsonl").unwrap_or(false) {
                        files.push(path);
                    }
                }
            }
        }

        let mut files = Vec::new();
        find_jsonl_files(&self.log_path, &mut files);

        tracing::debug!("[CodexLogProvider] Found {} .jsonl files", files.len());

        let latest = files
            .into_iter()
            .max_by_key(|p| fs::metadata(p).ok().and_then(|m| m.modified().ok()));

        if let Some(ref path) = latest {
            tracing::debug!("[CodexLogProvider] Latest session file: {:?}", path);
        } else {
            tracing::warn!("[CodexLogProvider] No session files found");
        }

        latest
    }

    fn parse_jsonl_entry(&self, line: &str) -> Option<(String, String, DateTime<Utc>)> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;

        // Parse timestamp from top level
        let timestamp = json
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Try new format first (type/payload structure)
        if let Some(entry_type) = json.get("type").and_then(|t| t.as_str()) {
            if let Some(payload) = json.get("payload") {
                // Handle response_item type
                if entry_type == "response_item" {
                    let payload_type = payload.get("type")?.as_str()?;
                    if payload_type != "message" {
                        return None;
                    }

                    // Skip user messages
                    if payload.get("role").and_then(|r| r.as_str()) == Some("user") {
                        return None;
                    }

                    let role = payload.get("role")?.as_str()?.to_string();

                    // content can be array or string
                    if let Some(content_array) = payload.get("content").and_then(|c| c.as_array()) {
                        let content = content_array
                            .iter()
                            .filter_map(|item| {
                                let item_type = item.get("type")?.as_str()?;
                                if item_type == "output_text" || item_type == "text" {
                                    item.get("text")?.as_str().map(|s| s.trim().to_string())
                                } else {
                                    None
                                }
                            })
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");

                        if !content.is_empty() {
                            return Some((role, content, timestamp));
                        }
                    } else if let Some(content_str) =
                        payload.get("content").and_then(|c| c.as_str())
                    {
                        let content = content_str.trim().to_string();
                        if !content.is_empty() {
                            return Some((role, content, timestamp));
                        }
                    }

                    // Fallback: check message field
                    if let Some(message) = payload.get("message").and_then(|m| m.as_str()) {
                        let message = message.trim().to_string();
                        if !message.is_empty() {
                            return Some((role, message, timestamp));
                        }
                    }

                    return None;
                }

                // Handle event_msg type
                if entry_type == "event_msg" {
                    let payload_type = payload.get("type")?.as_str()?;
                    let valid_types = [
                        "agent_message",
                        "assistant_message",
                        "assistant",
                        "assistant_response",
                        "message",
                    ];
                    if !valid_types.contains(&payload_type) {
                        return None;
                    }

                    // Skip user messages
                    if payload.get("role").and_then(|r| r.as_str()) == Some("user") {
                        return None;
                    }

                    let role = "assistant".to_string();
                    let content = payload
                        .get("message")
                        .or_else(|| payload.get("content"))
                        .or_else(|| payload.get("text"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())?;

                    return Some((role, content, timestamp));
                }

                // Fallback: check for role == "assistant" in payload
                if payload.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    let content = payload
                        .get("message")
                        .or_else(|| payload.get("content"))
                        .or_else(|| payload.get("text"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())?;

                    return Some(("assistant".to_string(), content, timestamp));
                }
            }
        }

        // Legacy format fallback: top-level role/content fields
        let role = json.get("role")?.as_str()?.to_string();
        if role == "user" {
            return None;
        }
        let content = json.get("content")?.as_str()?.trim().to_string();
        if content.is_empty() {
            return None;
        }
        Some((role, content, timestamp))
    }
}

#[async_trait]
impl LogProvider for CodexLogProvider {
    async fn get_latest_reply(&self, since_offset: u64) -> Option<LogEntry> {
        tracing::debug!(
            "[CodexLogProvider] get_latest_reply called with since_offset={}",
            since_offset
        );

        // Use locked session file if available, otherwise find latest
        let session_file = {
            let locked = self.locked_session.lock().await;
            if let Some(ref path) = *locked {
                tracing::debug!("[CodexLogProvider] Using locked session file: {:?}", path);
                path.clone()
            } else {
                drop(locked);
                self.find_latest_session_file()?
            }
        };

        let file = match File::open(&session_file) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "[CodexLogProvider] Failed to open session file {:?}: {}",
                    session_file,
                    e
                );
                return None;
            }
        };
        let mut reader = BufReader::new(file);

        if let Err(e) = reader.seek(SeekFrom::Start(since_offset)) {
            tracing::warn!(
                "[CodexLogProvider] Failed to seek to offset {}: {}",
                since_offset,
                e
            );
            return None;
        }

        let mut last_assistant_entry: Option<LogEntry> = None;
        let mut last_end_offset: u64 = since_offset;
        let mut lines_read = 0u32;
        let mut entries_parsed = 0u32;

        loop {
            let mut line = String::new();

            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    lines_read += 1;
                    // Get position after reading line (end of line)
                    let end_pos = reader.stream_position().ok()?;

                    if let Some((role, content, timestamp)) = self.parse_jsonl_entry(&line) {
                        entries_parsed += 1;
                        tracing::debug!(
                            "[CodexLogProvider] Parsed entry: role={}, content_len={}, offset={}",
                            role,
                            content.len(),
                            end_pos
                        );

                        if role == "assistant" {
                            last_assistant_entry = Some(LogEntry {
                                content,
                                offset: end_pos, // Use end position to avoid re-reading
                                timestamp,
                                inode: self.get_inode(),
                            });
                            last_end_offset = end_pos;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("[CodexLogProvider] Error reading line: {}", e);
                    break;
                }
            }
        }

        tracing::debug!(
            "[CodexLogProvider] Read {} lines, parsed {} entries, found assistant reply: {}",
            lines_read,
            entries_parsed,
            last_assistant_entry.is_some()
        );

        if last_assistant_entry.is_some() {
            self.current_offset.store(last_end_offset, Ordering::SeqCst);
        }

        last_assistant_entry
    }

    async fn get_history(&self, _session_id: Option<&str>, count: usize) -> Vec<HistoryEntry> {
        let Some(session_file) = self.find_latest_session_file() else {
            return Vec::new();
        };

        let Ok(content) = fs::read_to_string(&session_file) else {
            return Vec::new();
        };

        let mut entries: Vec<HistoryEntry> = content
            .lines()
            .filter_map(|line| {
                let (role, content, timestamp) = self.parse_jsonl_entry(line)?;
                Some(HistoryEntry {
                    role,
                    content,
                    timestamp,
                })
            })
            .collect();

        entries.reverse();
        entries.truncate(count);
        entries.reverse();

        entries
    }

    async fn get_current_offset(&self) -> u64 {
        if let Some(session_file) = self.find_latest_session_file() {
            if let Ok(metadata) = fs::metadata(&session_file) {
                return metadata.len();
            }
        }
        0
    }

    fn get_inode(&self) -> Option<u64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.find_latest_session_file()
                .and_then(|p| fs::metadata(&p).ok())
                .map(|m| m.ino())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn get_watch_path(&self) -> Option<PathBuf> {
        Some(self.log_path.clone())
    }

    async fn lock_session(&self) -> Option<LockedSession> {
        let session_file = self.find_latest_session_file()?;
        let metadata = fs::metadata(&session_file).ok()?;
        let baseline_offset = metadata.len();

        // Store locked session
        *self.locked_session.lock().await = Some(session_file.clone());

        tracing::info!(
            "[CodexLogProvider] Session locked: {:?}, baseline_offset={}",
            session_file,
            baseline_offset
        );

        Some(LockedSession {
            file_path: session_file,
            baseline_offset,
        })
    }

    async fn unlock_session(&self) {
        let mut locked = self.locked_session.lock().await;
        if locked.is_some() {
            tracing::debug!("[CodexLogProvider] Session unlocked");
        }
        *locked = None;
    }
}
