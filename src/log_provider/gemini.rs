//! Gemini log provider
//!
//! Observed storage layout:
//!   ~/.gemini/tmp/<project_hash>/chats/session-*.json
//!
//! We prefer using a deterministic project_hash derived from the agent working
//! directory when available, to avoid locking the wrong project's session file
//! when multiple projects exist under ~/.gemini/tmp.

use super::{HistoryEntry, LockedSession, LogEntry, LogProvider, PathMapper};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

#[derive(Clone)]
struct LockedGeminiSession {
    path: PathBuf,
    baseline_timestamp: DateTime<Utc>,
    // Cache fields to avoid re-parsing unchanged files
    last_modified: SystemTime,
    last_size: u64,
    cached_entries: Arc<Vec<(String, String, DateTime<Utc>)>>,
}

pub struct GeminiLogProvider {
    log_root: PathBuf,
    project_dir: Option<PathBuf>,
    current_offset: Arc<AtomicU64>,
    locked_session: Arc<Mutex<Option<LockedGeminiSession>>>,
}

impl GeminiLogProvider {
    pub fn new(config: Option<&HashMap<String, String>>) -> Self {
        let log_root = if let Some(cfg) = config {
            if let Some(path) = cfg.get("path_pattern") {
                PathMapper::normalize(path)
            } else {
                Self::default_log_path()
            }
        } else {
            Self::default_log_path()
        };

        let project_dir = config
            .and_then(|cfg| cfg.get("working_dir"))
            .map(|wd| Self::project_dir_for_working_dir(&log_root, wd));

        tracing::info!(
            "[GeminiLogProvider] Initialized with log_root={:?}, project_dir={:?}",
            log_root,
            project_dir
        );

        Self {
            log_root,
            project_dir,
            current_offset: Arc::new(AtomicU64::new(0)),
            locked_session: Arc::new(Mutex::new(None)),
        }
    }

    fn default_timestamp() -> DateTime<Utc> {
        DateTime::<Utc>::from(SystemTime::UNIX_EPOCH)
    }

    fn default_log_path() -> PathBuf {
        if let Ok(root) = std::env::var("GEMINI_ROOT") {
            if !root.is_empty() {
                return PathMapper::normalize(&root);
            }
        }
        PathMapper::normalize("~/.gemini/tmp")
    }

    fn project_dir_for_working_dir(log_root: &Path, working_dir: &str) -> PathBuf {
        // Gemini CLI uses sha256(process.cwd()) as the project hash directory name.
        //
        // Path normalization can vary (drive letter casing, separators, canonicalization),
        // so try a small set of candidates and prefer an existing directory if found.
        let normalized = PathMapper::normalize(working_dir);
        let normalized_str = normalized.to_string_lossy().to_string();

        let mut candidates: Vec<String> = Vec::new();
        candidates.push(normalized_str.clone());

        // Try alternate separator normalization (some callers may provide mixed separators).
        let win_sep = normalized_str.replace('/', "\\");
        if win_sep != normalized_str {
            candidates.push(win_sep);
        }
        let unix_sep = normalized_str.replace('\\', "/");
        if unix_sep != normalized_str {
            candidates.push(unix_sep);
        }

        if let Ok(canon) = normalized.canonicalize() {
            candidates.push(canon.to_string_lossy().to_string());
        }

        // Trim trailing separators (if any)
        let trimmed = normalized_str.trim_end_matches(['/', '\\']).to_string();
        if trimmed != normalized_str {
            candidates.push(trimmed);
        }

        // Windows: try toggling drive letter case (C: vs c:)
        if normalized_str.len() >= 2 && normalized_str.as_bytes()[1] == b':' {
            let drive = normalized_str.chars().next().unwrap_or('C');
            let toggled = if drive.is_ascii_uppercase() {
                drive.to_ascii_lowercase()
            } else {
                drive.to_ascii_uppercase()
            };
            candidates.push(format!("{}{}", toggled, &normalized_str[1..]));
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut fallback_dir: Option<PathBuf> = None;

        for candidate in candidates.into_iter() {
            if !seen.insert(candidate.clone()) {
                continue;
            }

            let digest = Sha256::digest(candidate.as_bytes());
            let mut hash = String::with_capacity(64);
            for b in digest {
                let _ = write!(&mut hash, "{:02x}", b);
            }

            let dir = log_root.join(hash);
            if fallback_dir.is_none() {
                fallback_dir = Some(dir.clone());
            }

            if dir.is_dir() {
                return dir;
            }
        }

        fallback_dir.unwrap_or_else(|| log_root.to_path_buf())
    }

    fn find_latest_chat_file(&self) -> Option<PathBuf> {
        tracing::debug!(
            "[GeminiLogProvider] Scanning for latest chat file in {:?}",
            self.log_root
        );

        if let Some(ref project_dir) = self.project_dir {
            let chats_dir = project_dir.join("chats");
            if !chats_dir.is_dir() {
                tracing::debug!(
                    "[GeminiLogProvider] Project chats dir not available: {:?}",
                    chats_dir
                );
                return None;
            }

            let result = Self::scan_latest_session_file_in_chats_dir(&chats_dir);
            if let Some(ref file) = result {
                tracing::debug!(
                    "[GeminiLogProvider] Found latest chat file in project dir: {:?}",
                    file
                );
            } else {
                tracing::debug!(
                    "[GeminiLogProvider] No chat files found in project dir: {:?}",
                    chats_dir
                );
            }
            return result;
        }

        if !self.log_root.exists() {
            tracing::warn!(
                "[GeminiLogProvider] Log path does not exist: {:?}",
                self.log_root
            );
            return None;
        }

        let result = Self::scan_latest_session(&self.log_root);
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

            let Ok(chat_entries) = fs::read_dir(&chats_dir) else {
                continue;
            };
            for chat_entry in chat_entries.filter_map(|e| e.ok()) {
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

    fn scan_latest_session_file_in_chats_dir(chats_dir: &Path) -> Option<PathBuf> {
        let mut latest_file: Option<PathBuf> = None;
        let mut latest_time = std::time::SystemTime::UNIX_EPOCH;

        let entries = fs::read_dir(chats_dir).ok()?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string());

            if let Some(ref n) = name {
                if !n.starts_with("session-") || !n.ends_with(".json") {
                    continue;
                }
            }

            if let Ok(metadata) = path.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if modified > latest_time {
                        latest_time = modified;
                        latest_file = Some(path);
                    }
                }
            }
        }

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
                    .unwrap_or_else(Self::default_timestamp);
                Some((role, content, timestamp))
            })
            .collect()
    }

    fn is_assistant_role(role: &str) -> bool {
        role == "assistant" || role == "model" || role == "gemini"
    }

    fn read_file_to_string(path: &PathBuf) -> std::io::Result<String> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut content = String::new();
        reader.read_to_string(&mut content)?;
        Ok(content)
    }

    fn read_file_to_string_with_metadata(
        path: &Path,
    ) -> std::io::Result<(String, Option<(SystemTime, u64)>)> {
        let file = File::open(path)?;
        let metadata = file.metadata().ok().and_then(|m| {
            let modified = m.modified().ok()?;
            Some((modified, m.len()))
        });

        let mut reader = BufReader::new(file);
        let mut content = String::new();
        reader.read_to_string(&mut content)?;
        Ok((content, metadata))
    }

    /// Get file metadata (mtime, size) for cache invalidation check.
    /// Returns None if file doesn't exist or metadata unavailable.
    fn get_file_metadata(path: &Path) -> Option<(SystemTime, u64)> {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let size = metadata.len();
        Some((modified, size))
    }

    /// Check if cached entries are still valid by comparing file metadata.
    /// Returns true if cache is valid (file unchanged), false if re-parse needed.
    fn is_cache_valid(
        cached_mtime: SystemTime,
        cached_size: u64,
        current_mtime: SystemTime,
        current_size: u64,
    ) -> bool {
        cached_mtime == current_mtime && cached_size == current_size
    }

    /// Parse file and update the locked session cache.
    /// Returns the parsed entries.
    async fn parse_and_update_cache(
        &self,
        path: &PathBuf,
    ) -> Arc<Vec<(String, String, DateTime<Utc>)>> {
        let (content, metadata) = match Self::read_file_to_string_with_metadata(path) {
            Ok((c, m)) => (c, m),
            Err(e) => {
                tracing::warn!("[GeminiLogProvider] Failed to read file {:?}: {}", path, e);
                return Arc::new(Vec::new());
            }
        };

        let entries = Arc::new(self.parse_chat_json(&content));

        // Update cache in locked session if path matches
        if let Some((mtime, size)) = metadata {
            let mut locked = self.locked_session.lock().await;
            if let Some(ref mut session) = *locked {
                if session.path == *path {
                    session.last_modified = mtime;
                    session.last_size = size;
                    session.cached_entries = entries.clone();
                    tracing::debug!(
                        "[GeminiLogProvider] Cache updated: {} entries, size={}",
                        entries.len(),
                        size
                    );
                }
            }
        }

        entries
    }

    fn find_assistant_reply(
        &self,
        entries: &[(String, String, DateTime<Utc>)],
        since_offset: u64,
    ) -> Option<LogEntry> {
        // On 32-bit systems, u64 may overflow usize. Using MAX ensures .min() clamps
        // to array length, resulting in an empty slice (no matches) - correct behavior.
        let start = usize::try_from(since_offset)
            .unwrap_or(usize::MAX)
            .min(entries.len());

        // Use slice for O(1) offset instead of O(N) skip
        entries[start..]
            .iter()
            .enumerate()
            .rfind(|(_, (role, content, _))| {
                Self::is_assistant_role(role) && !content.trim().is_empty()
            })
            .map(|(local_idx, (_, content, timestamp))| LogEntry {
                content: content.clone(),
                offset: (start + local_idx) as u64 + 1,
                timestamp: *timestamp,
                inode: self.get_inode(),
            })
    }

    fn find_assistant_reply_by_timestamp(
        &self,
        entries: &[(String, String, DateTime<Utc>)],
        baseline_timestamp: DateTime<Utc>,
    ) -> Option<LogEntry> {
        entries
            .iter()
            .enumerate()
            .rfind(|(_, (role, content, timestamp))| {
                Self::is_assistant_role(role)
                    && *timestamp > baseline_timestamp
                    && !content.trim().is_empty()
            })
            .map(|(idx, (_, content, timestamp))| LogEntry {
                content: content.clone(),
                offset: idx as u64 + 1,
                timestamp: *timestamp,
                inode: self.get_inode(),
            })
    }

    fn scan_newer_session(
        &self,
        locked_file: &PathBuf,
        since_offset: u64,
        baseline_timestamp: DateTime<Utc>,
        default_timestamp: DateTime<Utc>,
    ) -> Option<(LogEntry, u64)> {
        let chats_dir = locked_file.parent()?;
        let latest_file = Self::scan_latest_session_file_in_chats_dir(chats_dir)?;

        if latest_file == *locked_file {
            return None;
        }

        tracing::info!(
            "[GeminiLogProvider] Found newer session file: {:?}, switching to it",
            latest_file
        );

        let content = match Self::read_file_to_string(&latest_file) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(
                    "[GeminiLogProvider] Failed to read newer chat file: {:?}",
                    latest_file
                );
                return None;
            }
        };

        let entries = self.parse_chat_json(&content);
        let total_messages = entries.len() as u64;

        tracing::debug!(
            "[GeminiLogProvider] Parsed {} messages from newer file",
            total_messages
        );

        // First try offset-based search
        if let Some(entry) = self.find_assistant_reply(&entries, since_offset) {
            return Some((entry, total_messages));
        }

        // Fall back to timestamp-based search if baseline is valid
        if baseline_timestamp != default_timestamp {
            if let Some(entry) =
                self.find_assistant_reply_by_timestamp(&entries, baseline_timestamp)
            {
                return Some((entry, total_messages));
            }
        }

        None
    }
}

#[async_trait]
impl LogProvider for GeminiLogProvider {
    async fn get_latest_reply(&self, since_offset: u64) -> Option<LogEntry> {
        tracing::debug!(
            "[GeminiLogProvider] get_latest_reply called with since_offset={}",
            since_offset
        );

        let default_timestamp = Self::default_timestamp();

        // Use locked session file if available, otherwise find latest
        let (chat_file, baseline_timestamp, should_check_newer, cached_data) = {
            let locked = self.locked_session.lock().await.clone();
            if let Some(ref locked) = locked {
                tracing::debug!(
                    "[GeminiLogProvider] Using locked session file: {:?}",
                    locked.path
                );
                (
                    locked.path.clone(),
                    locked.baseline_timestamp,
                    true,
                    Some((
                        locked.last_modified,
                        locked.last_size,
                        locked.cached_entries.clone(),
                    )),
                )
            } else {
                match self.find_latest_chat_file() {
                    Some(f) => {
                        tracing::debug!("[GeminiLogProvider] Found latest chat file: {:?}", f);
                        (f, default_timestamp, false, None)
                    }
                    None => {
                        tracing::debug!("[GeminiLogProvider] No chat file found");
                        return None;
                    }
                }
            }
        };

        // Get current file metadata for cache validation
        let current_metadata = Self::get_file_metadata(&chat_file);

        // Try to use cached entries if file unchanged (O(1) syscall instead of full parse)
        let entries = if let (
            Some((cached_mtime, cached_size, ref cached_entries)),
            Some((current_mtime, current_size)),
        ) = (&cached_data, current_metadata)
        {
            if Self::is_cache_valid(*cached_mtime, *cached_size, current_mtime, current_size) {
                tracing::debug!(
                    "[GeminiLogProvider] Cache hit - file unchanged, using {} cached entries",
                    cached_entries.len()
                );
                cached_entries.clone()
            } else {
                tracing::debug!("[GeminiLogProvider] Cache miss - file changed, re-parsing");
                self.parse_and_update_cache(&chat_file).await
            }
        } else {
            // No cache available, parse file
            tracing::debug!("[GeminiLogProvider] No cache available, parsing file");
            self.parse_and_update_cache(&chat_file).await
        };

        let total_messages = entries.len() as u64;

        tracing::debug!(
            "[GeminiLogProvider] Using {} messages from {:?}, since_offset={}",
            total_messages,
            chat_file,
            since_offset
        );

        if let Some(result) = self.find_assistant_reply(entries.as_slice(), since_offset) {
            self.current_offset.store(total_messages, Ordering::SeqCst);
            tracing::info!(
                "[GeminiLogProvider] Found assistant reply, new offset={}",
                total_messages
            );
            return Some(result);
        }

        // Fallback: check newer session file if we were using a locked session
        if should_check_newer {
            tracing::debug!(
                "[GeminiLogProvider] No reply in locked session, checking for newer files"
            );

            if let Some((result, total_messages)) = self.scan_newer_session(
                &chat_file,
                since_offset,
                baseline_timestamp,
                default_timestamp,
            ) {
                self.current_offset.store(total_messages, Ordering::SeqCst);
                tracing::info!(
                    "[GeminiLogProvider] Found assistant reply in newer file, offset={}",
                    total_messages
                );
                return Some(result);
            }
        }

        tracing::debug!("[GeminiLogProvider] No new assistant reply found");
        None
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
        if let Some(ref project_dir) = self.project_dir {
            let chats_dir = project_dir.join("chats");
            return Some(if chats_dir.is_dir() {
                chats_dir
            } else {
                project_dir.clone()
            });
        }
        Some(self.log_root.clone())
    }

    async fn lock_session(&self) -> Option<LockedSession> {
        let chat_file = self.find_latest_chat_file()?;
        let (content, metadata) = Self::read_file_to_string_with_metadata(&chat_file).ok()?;
        let entries = self.parse_chat_json(&content);
        let baseline_offset = entries.len() as u64;
        let default_timestamp = Self::default_timestamp();
        let baseline_timestamp = entries
            .iter()
            .rev()
            .map(|(_, _, ts)| *ts)
            .find(|ts| *ts != default_timestamp)
            .unwrap_or(default_timestamp);

        let cached_entries = Arc::new(entries);
        let (last_modified, last_size) = metadata.unwrap_or((SystemTime::UNIX_EPOCH, 0));

        // Store locked session with cache
        *self.locked_session.lock().await = Some(LockedGeminiSession {
            path: chat_file.clone(),
            baseline_timestamp,
            last_modified,
            last_size,
            cached_entries,
        });

        tracing::info!(
            "[GeminiLogProvider] Session locked: {:?}, baseline_offset={}, cached_size={}",
            chat_file,
            baseline_offset,
            last_size
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_session_file(path: &Path, last_updated: &str) {
        let content = format!(
            r#"{{
  "sessionId": "test",
  "projectHash": "deadbeef",
  "startTime": "2026-01-01T00:00:00.000Z",
  "lastUpdated": "{last_updated}",
  "messages": [
    {{
      "id": "1",
      "timestamp": "2026-01-01T00:00:00.000Z",
      "type": "gemini",
      "content": "hello"
    }}
  ]
}}"#
        );
        fs::write(path, content).unwrap();
    }

    #[test]
    fn prefers_project_hash_dir_over_global_latest() {
        let root = tempdir().unwrap();

        // Simulate two projects under GEMINI_ROOT/tmp
        let working_dir = r"D:\Shareware\ccgo";
        let project_dir = GeminiLogProvider::project_dir_for_working_dir(root.path(), working_dir);
        let other_dir = root
            .path()
            .join("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");

        let project_chats = project_dir.join("chats");
        let other_chats = other_dir.join("chats");
        fs::create_dir_all(&project_chats).unwrap();
        fs::create_dir_all(&other_chats).unwrap();

        let project_session = project_chats.join("session-2026-01-01T00-00-00test.json");
        let other_session = other_chats.join("session-2026-01-02T00-00-00test.json");
        write_session_file(&project_session, "2026-01-01T00:00:00.000Z");
        write_session_file(&other_session, "2026-01-02T00:00:00.000Z");

        let mut cfg = HashMap::new();
        cfg.insert(
            "path_pattern".to_string(),
            root.path().to_string_lossy().to_string(),
        );
        cfg.insert("working_dir".to_string(), working_dir.to_string());

        let provider = GeminiLogProvider::new(Some(&cfg));
        let chosen = provider.find_latest_chat_file().unwrap();

        assert_eq!(chosen, project_session);
    }
}
