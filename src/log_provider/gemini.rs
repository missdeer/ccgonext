//! Gemini log provider
//!
//! Observed storage layout:
//!   ~/.gemini/tmp/<project_hash>/chats/session-*.json
//!
//! Instead of computing project_hash (which is fragile due to path normalization
//! differences across platforms), we monitor the entire log directory and find
//! the most recently modified session file.

use super::{HistoryEntry, LockedSession, LogEntry, LogProvider, PathMapper};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct GeminiLogProvider {
    log_path: PathBuf,
    current_offset: Arc<AtomicU64>,
    locked_session: Arc<Mutex<Option<PathBuf>>>,
}

impl GeminiLogProvider {
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

        tracing::info!(
            "[GeminiLogProvider] Initialized with log_path={:?}",
            log_path
        );

        Self {
            log_path,
            current_offset: Arc::new(AtomicU64::new(0)),
            locked_session: Arc::new(Mutex::new(None)),
        }
    }

    fn default_log_path() -> PathBuf {
        if let Ok(root) = std::env::var("GEMINI_ROOT") {
            if !root.is_empty() {
                return PathMapper::normalize(&root);
            }
        }
        PathMapper::normalize("~/.gemini/tmp")
    }

    fn find_latest_chat_file(&self) -> Option<PathBuf> {
        tracing::debug!(
            "[GeminiLogProvider] Scanning for latest chat file in {:?}",
            self.log_path
        );

        if !self.log_path.exists() {
            tracing::warn!(
                "[GeminiLogProvider] Log path does not exist: {:?}",
                self.log_path
            );
            return None;
        }

        let result = Self::scan_latest_session(&self.log_path);
        if let Some(ref file) = result {
            tracing::debug!("[GeminiLogProvider] Found latest chat file: {:?}", file);
        } else {
            tracing::warn!("[GeminiLogProvider] No chat files found");
        }
        result
    }

    fn scan_latest_session(root: &PathBuf) -> Option<PathBuf> {
        let mut latest_file: Option<PathBuf> = None;
        let mut latest_time = std::time::SystemTime::UNIX_EPOCH;
        let mut project_count = 0u32;
        let mut total_files = 0u32;

        for entry in fs::read_dir(root).ok()?.filter_map(|e| e.ok()) {
            let hash_dir = entry.path();
            let chats_dir = hash_dir.join("chats");

            if !chats_dir.is_dir() {
                continue;
            }

            project_count += 1;

            for chat_entry in fs::read_dir(&chats_dir).ok()?.filter_map(|e| e.ok()) {
                let path = chat_entry.path();
                let name = path.file_name().map(|n| n.to_string_lossy().to_string());

                if let Some(ref n) = name {
                    if !n.starts_with("session-") || !n.ends_with(".json") {
                        continue;
                    }
                }

                total_files += 1;

                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if modified > latest_time {
                            latest_time = modified;
                            latest_file = Some(path);
                        }
                    }
                }
            }
        }

        tracing::debug!(
            "[GeminiLogProvider] Scanned {} project dirs, {} total files, latest: {:?}",
            project_count,
            total_files,
            latest_file
        );

        latest_file
    }

    fn parse_chat_json(&self, content: &str) -> Vec<(String, String, DateTime<Utc>)> {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
            return Vec::new();
        };

        let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
            return Vec::new();
        };

        messages
            .iter()
            .filter_map(|msg| {
                // Gemini uses "type" instead of "role"
                let role = msg.get("type")?.as_str()?.to_string();
                let content = msg.get("content")?.as_str()?.to_string();
                let timestamp = msg
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                Some((role, content, timestamp))
            })
            .collect()
    }
}

#[async_trait]
impl LogProvider for GeminiLogProvider {
    async fn get_latest_reply(&self, since_offset: u64) -> Option<LogEntry> {
        tracing::debug!(
            "[GeminiLogProvider] get_latest_reply called with since_offset={}",
            since_offset
        );

        // Use locked session file if available, otherwise find latest
        let chat_file = {
            let locked = self.locked_session.lock().await;
            if let Some(ref path) = *locked {
                tracing::debug!("[GeminiLogProvider] Using locked session file: {:?}", path);
                path.clone()
            } else {
                drop(locked);
                match self.find_latest_chat_file() {
                    Some(f) => f,
                    None => {
                        tracing::debug!("[GeminiLogProvider] No chat file found");
                        return None;
                    }
                }
            }
        };

        let file = match File::open(&chat_file) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "[GeminiLogProvider] Failed to open chat file {:?}: {}",
                    chat_file,
                    e
                );
                return None;
            }
        };

        let mut reader = BufReader::new(file);
        let mut content = String::new();
        if let Err(e) = reader.read_to_string(&mut content) {
            tracing::warn!(
                "[GeminiLogProvider] Failed to read chat file {:?}: {}",
                chat_file,
                e
            );
            return None;
        }

        let entries = self.parse_chat_json(&content);
        let total_messages = entries.len() as u64;

        tracing::debug!(
            "[GeminiLogProvider] Parsed {} messages from {:?}, since_offset={}",
            total_messages,
            chat_file,
            since_offset
        );

        // Only consider messages after since_offset (message index)
        let new_entries: Vec<_> = entries
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| *idx as u64 >= since_offset)
            .collect();

        tracing::debug!(
            "[GeminiLogProvider] {} new messages since offset {}",
            new_entries.len(),
            since_offset
        );

        // Find last assistant message in new entries
        // Gemini uses "gemini" as the role/type for assistant messages
        let result = new_entries
            .into_iter()
            .rfind(|(_, (role, _, _))| role == "assistant" || role == "model" || role == "gemini")
            .map(|(idx, (_, content, timestamp))| LogEntry {
                content,
                offset: idx as u64 + 1, // Next message index
                timestamp,
                inode: self.get_inode(),
            });

        // Update current offset to total message count
        if result.is_some() {
            self.current_offset.store(total_messages, Ordering::SeqCst);
            tracing::info!(
                "[GeminiLogProvider] Found assistant reply, new offset={}",
                total_messages
            );
        } else {
            tracing::debug!("[GeminiLogProvider] No new assistant reply found");
        }

        result
    }

    async fn get_history(&self, _session_id: Option<&str>, count: usize) -> Vec<HistoryEntry> {
        let Some(chat_file) = self.find_latest_chat_file() else {
            return Vec::new();
        };

        let Ok(content) = fs::read_to_string(&chat_file) else {
            return Vec::new();
        };

        let mut entries: Vec<HistoryEntry> = self
            .parse_chat_json(&content)
            .into_iter()
            .map(|(role, content, timestamp)| {
                // Normalize role: "gemini" and "model" -> "assistant"
                let normalized_role = if role == "model" || role == "gemini" {
                    "assistant".to_string()
                } else {
                    role
                };
                HistoryEntry {
                    role: normalized_role,
                    content,
                    timestamp,
                }
            })
            .collect();

        entries.reverse();
        entries.truncate(count);
        entries.reverse();

        entries
    }

    async fn get_current_offset(&self) -> u64 {
        // Return current message count as offset
        if let Some(chat_file) = self.find_latest_chat_file() {
            if let Ok(content) = fs::read_to_string(&chat_file) {
                return self.parse_chat_json(&content).len() as u64;
            }
        }
        self.current_offset.load(Ordering::SeqCst)
    }

    fn get_inode(&self) -> Option<u64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.find_latest_chat_file()
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
        let chat_file = self.find_latest_chat_file()?;
        let content = fs::read_to_string(&chat_file).ok()?;
        let baseline_offset = self.parse_chat_json(&content).len() as u64;

        // Store locked session
        *self.locked_session.lock().await = Some(chat_file.clone());

        tracing::info!(
            "[GeminiLogProvider] Session locked: {:?}, baseline_offset={}",
            chat_file,
            baseline_offset
        );

        Some(LockedSession {
            file_path: chat_file,
            baseline_offset,
        })
    }

    async fn unlock_session(&self) {
        let mut locked = self.locked_session.lock().await;
        if locked.is_some() {
            tracing::debug!("[GeminiLogProvider] Session unlocked");
        }
        *locked = None;
    }
}
