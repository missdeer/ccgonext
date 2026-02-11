//! Codex log provider

use super::{HistoryEntry, LockedSession, LogEntry, LogProvider, PathMapper};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
struct LockedCodexSession {
    path: PathBuf,
    baseline_timestamp: Option<DateTime<Utc>>,
    lock_time: DateTime<Utc>,
    override_offset: Option<u64>,
}

pub struct CodexLogProvider {
    log_path: PathBuf,
    current_offset: Arc<AtomicU64>,
    #[allow(dead_code)]
    file_handle: Arc<Mutex<Option<File>>>,
    locked_session: Arc<Mutex<Option<LockedCodexSession>>>,
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

    fn find_all_session_files(&self) -> Vec<PathBuf> {
        if !self.log_path.exists() {
            tracing::warn!(
                "[CodexLogProvider] Log path does not exist: {:?}",
                self.log_path
            );
            return Vec::new();
        }

        fn collect_jsonl(dir: &Path, files: &mut Vec<PathBuf>) {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        collect_jsonl(&path, files);
                    } else if path.extension().map(|ext| ext == "jsonl").unwrap_or(false) {
                        files.push(path);
                    }
                }
            }
        }

        let mut files = Vec::new();
        collect_jsonl(&self.log_path, &mut files);
        files.sort_by(|a, b| {
            let ma = fs::metadata(a).ok().and_then(|m| m.modified().ok());
            let mb = fs::metadata(b).ok().and_then(|m| m.modified().ok());
            mb.cmp(&ma)
        });
        files
    }

    fn find_latest_session_file(&self) -> Option<PathBuf> {
        tracing::debug!(
            "[CodexLogProvider] Looking for session files in: {:?}",
            self.log_path
        );

        let files = self.find_all_session_files();
        tracing::debug!("[CodexLogProvider] Found {} .jsonl files", files.len());

        let latest = files.into_iter().next();

        if let Some(ref path) = latest {
            tracing::debug!("[CodexLogProvider] Latest session file: {:?}", path);
        } else {
            tracing::warn!("[CodexLogProvider] No session files found");
        }

        latest
    }

    fn find_latest_timestamp(&self, path: &Path) -> Option<DateTime<Utc>> {
        let file = File::open(path).ok()?;
        let reader = BufReader::new(file);
        let mut latest: Option<DateTime<Utc>> = None;

        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Some(ts) = Self::parse_raw_timestamp(&line) {
                latest = Some(match latest {
                    Some(prev) if ts > prev => ts,
                    Some(prev) => prev,
                    None => ts,
                });
            }
        }
        latest
    }

    fn parse_raw_timestamp(line: &str) -> Option<DateTime<Utc>> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;
        json.get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
    }

    fn scan_newer_sessions(
        &self,
        locked_path: &Path,
        baseline: DateTime<Utc>,
    ) -> Option<(LogEntry, PathBuf)> {
        let candidates: Vec<PathBuf> = self
            .find_all_session_files()
            .into_iter()
            .filter(|p| p != locked_path)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        tracing::info!(
            "[CodexLogProvider] Checking {} candidate session file(s) for reply",
            candidates.len()
        );

        for candidate in &candidates {
            let file = match File::open(candidate) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(file);
            let mut last_assistant_entry: Option<LogEntry> = None;

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let end_pos = match reader.stream_position() {
                            Ok(p) => p,
                            Err(_) => break,
                        };
                        if let Some((role, content, timestamp)) = self.parse_jsonl_entry(&line) {
                            if role == "assistant" && timestamp >= baseline {
                                last_assistant_entry = Some(LogEntry {
                                    content: content.clone(),
                                    offset: end_pos,
                                    timestamp,
                                    inode: self.get_inode(),
                                    done_seen: super::might_have_done_marker(&content),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[CodexLogProvider] Error reading session {:?}: {}",
                            candidate,
                            e
                        );
                        break;
                    }
                }
            }

            if let Some(entry) = last_assistant_entry {
                tracing::info!(
                    "[CodexLogProvider] Found assistant reply in session file: {:?}",
                    candidate
                );
                return Some((entry, candidate.clone()));
            }
        }

        None
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
        let (session_file, should_check_newer, baseline_timestamp, override_offset, lock_time) = {
            let locked = self.locked_session.lock().await;
            if let Some(ref session) = *locked {
                tracing::debug!(
                    "[CodexLogProvider] Using locked session file: {:?}",
                    session.path
                );
                (
                    session.path.clone(),
                    true,
                    session.baseline_timestamp,
                    session.override_offset,
                    Some(session.lock_time),
                )
            } else {
                drop(locked);
                match self.find_latest_session_file() {
                    Some(f) => (f, false, None, None, None),
                    None => return None,
                }
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

        // Determine effective read offset:
        // 1. After re-lock, override_offset points to where we left off in the new file
        // 2. If since_offset exceeds file size, the file was replaced — reset to 0
        // 3. Otherwise use the caller's offset
        let file_len = file.metadata().ok().map(|m| m.len()).unwrap_or(u64::MAX);
        let effective_offset = if let Some(ov) = override_offset {
            if ov <= file_len {
                ov
            } else {
                0
            }
        } else if since_offset > file_len {
            0
        } else {
            since_offset
        };

        let mut reader = BufReader::new(file);

        if let Err(e) = reader.seek(SeekFrom::Start(effective_offset)) {
            tracing::warn!(
                "[CodexLogProvider] Failed to seek to offset {}: {}",
                effective_offset,
                e
            );
            return None;
        }

        let mut last_assistant_entry: Option<LogEntry> = None;
        let mut last_end_offset: u64 = effective_offset;
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
                                content: content.clone(),
                                offset: end_pos, // Use end position to avoid re-reading
                                timestamp,
                                inode: self.get_inode(),
                                done_seen: super::might_have_done_marker(&content),
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
            if override_offset.is_some() {
                let mut locked = self.locked_session.lock().await;
                if let Some(ref mut session) = *locked {
                    session.override_offset = Some(last_end_offset);
                }
            }
            return last_assistant_entry;
        }

        if should_check_newer {
            let effective_baseline = baseline_timestamp.or(lock_time).unwrap_or_else(Utc::now);
            tracing::debug!(
                "[CodexLogProvider] No reply in locked session, checking for newer files (baseline={:?})",
                effective_baseline
            );
            if let Some((entry, new_path)) =
                self.scan_newer_sessions(&session_file, effective_baseline)
            {
                let new_baseline = self.find_latest_timestamp(&new_path);
                *self.locked_session.lock().await = Some(LockedCodexSession {
                    path: new_path,
                    baseline_timestamp: new_baseline,
                    lock_time: Utc::now(),
                    override_offset: Some(entry.offset),
                });
                self.current_offset.store(entry.offset, Ordering::SeqCst);
                tracing::info!(
                    "[CodexLogProvider] Re-locked to newer session, offset={}",
                    entry.offset
                );
                return Some(entry);
            }
        }

        None
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

        let baseline_timestamp = self.find_latest_timestamp(&session_file);
        let lock_time = Utc::now();

        *self.locked_session.lock().await = Some(LockedCodexSession {
            path: session_file.clone(),
            baseline_timestamp,
            lock_time,
            override_offset: None,
        });

        tracing::info!(
            "[CodexLogProvider] Session locked: {:?}, baseline_offset={}, baseline_timestamp={:?}",
            session_file,
            baseline_offset,
            baseline_timestamp
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn make_provider(dir: &std::path::Path) -> CodexLogProvider {
        let mut config_map = HashMap::new();
        config_map.insert(
            "path_pattern".to_string(),
            dir.to_str().unwrap().to_string(),
        );
        CodexLogProvider::new(Some(&config_map))
    }

    fn write_entry(file: &mut File, role: &str, content: &str, ts: DateTime<Utc>) {
        let json = serde_json::json!({
            "timestamp": ts.to_rfc3339(),
            "role": role,
            "content": content
        });
        writeln!(file, "{}", json).unwrap();
    }

    #[tokio::test]
    async fn test_offset_reset_on_truncation() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let session_path = dir_path.join("session-2023-01-01.jsonl");
        let mut file = File::create(&session_path).unwrap();

        write_entry(&mut file, "assistant", "Hello world", Utc::now());

        let metadata = fs::metadata(&session_path).unwrap();
        let file_len = metadata.len();

        let provider = make_provider(&dir_path);
        let result = provider.get_latest_reply(file_len + 100).await;

        assert!(
            result.is_some(),
            "Should return a reply even if offset is larger than file size (reset to 0)"
        );
        assert_eq!(result.unwrap().content, "Hello world");
    }

    #[tokio::test]
    async fn test_session_rollover_with_baseline_timestamp() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let base_ts = Utc::now();

        let old_path = dir_path.join("session-old.jsonl");
        let mut old_file = File::create(&old_path).unwrap();
        write_entry(&mut old_file, "user", "hello", base_ts);
        drop(old_file);

        let provider = make_provider(&dir_path);
        let locked = provider.lock_session().await.unwrap();
        let baseline_offset = locked.baseline_offset;

        // Simulate: codex creates a new session file with a reply
        let new_ts = base_ts + chrono::Duration::seconds(5);
        // Touch new file with a later mtime
        std::thread::sleep(std::time::Duration::from_millis(50));
        let new_path = dir_path.join("session-new.jsonl");
        let mut new_file = File::create(&new_path).unwrap();
        write_entry(&mut new_file, "assistant", "new session reply", new_ts);
        drop(new_file);

        let result = provider.get_latest_reply(baseline_offset).await;

        assert!(result.is_some(), "Should find reply in newer session file");
        assert_eq!(result.unwrap().content, "new session reply");
    }

    #[tokio::test]
    async fn test_session_rollover_with_none_baseline() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // Create an empty session file (no timestamps → baseline_timestamp = None)
        let old_path = dir_path.join("session-old.jsonl");
        File::create(&old_path).unwrap();

        let provider = make_provider(&dir_path);
        let locked = provider.lock_session().await.unwrap();
        let baseline_offset = locked.baseline_offset;

        // Verify baseline_timestamp is None
        {
            let session = provider.locked_session.lock().await;
            assert!(
                session.as_ref().unwrap().baseline_timestamp.is_none(),
                "Empty file should produce None baseline_timestamp"
            );
        }

        // Simulate: codex creates a new session with a reply
        std::thread::sleep(std::time::Duration::from_millis(50));
        let new_path = dir_path.join("session-new.jsonl");
        let mut new_file = File::create(&new_path).unwrap();
        write_entry(
            &mut new_file,
            "assistant",
            "reply from new session",
            Utc::now(),
        );
        drop(new_file);

        let result = provider.get_latest_reply(baseline_offset).await;

        assert!(
            result.is_some(),
            "Should find reply even when baseline_timestamp is None"
        );
        assert_eq!(result.unwrap().content, "reply from new session");
    }

    #[tokio::test]
    async fn test_equal_timestamp_not_missed() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let ts = Utc::now();

        let old_path = dir_path.join("session-old.jsonl");
        let mut old_file = File::create(&old_path).unwrap();
        write_entry(&mut old_file, "user", "query", ts);
        drop(old_file);

        let provider = make_provider(&dir_path);
        let locked = provider.lock_session().await.unwrap();
        let baseline_offset = locked.baseline_offset;

        // New session with SAME timestamp as baseline
        std::thread::sleep(std::time::Duration::from_millis(50));
        let new_path = dir_path.join("session-new.jsonl");
        let mut new_file = File::create(&new_path).unwrap();
        write_entry(&mut new_file, "assistant", "same-ts reply", ts);
        drop(new_file);

        let result = provider.get_latest_reply(baseline_offset).await;

        assert!(
            result.is_some(),
            "Should find reply with equal timestamp (>= not >)"
        );
        assert_eq!(result.unwrap().content, "same-ts reply");
    }

    #[tokio::test]
    async fn test_override_offset_after_relock() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let base_ts = Utc::now();

        let old_path = dir_path.join("session-old.jsonl");
        let mut old_file = File::create(&old_path).unwrap();
        write_entry(&mut old_file, "user", "msg", base_ts);
        drop(old_file);

        let provider = make_provider(&dir_path);
        let locked = provider.lock_session().await.unwrap();
        let baseline_offset = locked.baseline_offset;

        // New session file with two assistant replies
        let ts1 = base_ts + chrono::Duration::seconds(1);
        let ts2 = base_ts + chrono::Duration::seconds(2);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let new_path = dir_path.join("session-new.jsonl");
        let mut new_file = File::create(&new_path).unwrap();
        write_entry(&mut new_file, "assistant", "first reply", ts1);
        write_entry(&mut new_file, "assistant", "second reply", ts2);
        drop(new_file);

        // First poll triggers re-lock, returns the last reply
        let result = provider.get_latest_reply(baseline_offset).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().content, "second reply");

        // Verify override_offset was set
        {
            let session = provider.locked_session.lock().await;
            assert!(session.as_ref().unwrap().override_offset.is_some());
        }

        // Second poll with same stale baseline_offset should use override_offset
        // and not re-read old content (no new entries → None is correct)
        let result2 = provider.get_latest_reply(baseline_offset).await;
        assert!(result2.is_none(), "No new content after override_offset");
    }

    #[tokio::test]
    async fn test_none_baseline_rejects_historical_replies() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // Create a "historical" session with an old timestamp FIRST
        let historical_ts = Utc::now() - chrono::Duration::hours(1);
        let hist_path = dir_path.join("session-hist.jsonl");
        let mut hist_file = File::create(&hist_path).unwrap();
        write_entry(
            &mut hist_file,
            "assistant",
            "stale historical reply",
            historical_ts,
        );
        drop(hist_file);

        // Sleep so the empty file gets a later mtime and becomes the "latest"
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create an empty session file (baseline_timestamp = None) — this is the latest by mtime
        let old_path = dir_path.join("session-old.jsonl");
        File::create(&old_path).unwrap();

        let provider = make_provider(&dir_path);
        let locked = provider.lock_session().await.unwrap();
        let baseline_offset = locked.baseline_offset;

        // Verify the lock is on the empty file with None baseline
        {
            let session = provider.locked_session.lock().await;
            let s = session.as_ref().unwrap();
            assert!(s.baseline_timestamp.is_none());
            assert!(s.lock_time > historical_ts);
        }

        let result = provider.get_latest_reply(baseline_offset).await;

        assert!(
            result.is_none(),
            "Should NOT match historical replies older than lock_time"
        );
    }
}
