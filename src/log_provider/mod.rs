//! Log provider abstraction layer

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

mod codex;
mod gemini;
mod opencode;
mod path_mapper;

pub use codex::CodexLogProvider;
pub use gemini::GeminiLogProvider;
pub use opencode::OpenCodeLogProvider;
pub use path_mapper::PathMapper;

/// Handle for a file watcher subscription
pub struct WatchHandle {
    _cancel_tx: tokio::sync::oneshot::Sender<()>,
}

impl WatchHandle {
    pub fn new(cancel_tx: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            _cancel_tx: cancel_tx,
        }
    }
}

/// File change event
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub timestamp: std::time::Instant,
}

/// Watcher subscription with debouncing support
pub struct WatchSubscription {
    pub handle: WatchHandle,
    pub receiver: broadcast::Receiver<FileChangeEvent>,
}

/// Create a debounced file watcher for a path
pub fn create_debounced_watcher(
    path: std::path::PathBuf,
    debounce_ms: u64,
) -> Option<(Arc<broadcast::Sender<FileChangeEvent>>, WatchHandle)> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let (event_tx, _) = broadcast::channel::<FileChangeEvent>(16);
    let event_tx = Arc::new(event_tx);
    let event_tx_clone = event_tx.clone();

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    std::thread::spawn(move || {
        let (notify_tx, notify_rx) = std::sync::mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(notify_tx, Config::default()) {
            Ok(w) => w,
            Err(_) => return,
        };

        if watcher.watch(&path, RecursiveMode::Recursive).is_err() {
            return;
        }

        let debounce_duration = Duration::from_millis(debounce_ms);
        let mut last_event_time: Option<Instant> = None;

        // Check for cancellation in a separate thread
        let running_for_cancel = running_clone.clone();
        std::thread::spawn(move || {
            let _ = cancel_rx.blocking_recv();
            running_for_cancel.store(false, Ordering::SeqCst);
        });

        while running_clone.load(Ordering::SeqCst) {
            match notify_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(_event) => {
                    let now = Instant::now();
                    let should_emit = match last_event_time {
                        Some(last) => now.duration_since(last) >= debounce_duration,
                        None => true,
                    };

                    if should_emit {
                        last_event_time = Some(now);
                        let _ = event_tx_clone.send(FileChangeEvent { timestamp: now });
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Some((event_tx, WatchHandle::new(cancel_tx)))
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub content: String,
    pub offset: u64,
    pub timestamp: DateTime<Utc>,
    pub inode: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub role: String, // "user" or "assistant"
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// Locked session info returned by lock_session
#[derive(Debug, Clone)]
pub struct LockedSession {
    pub file_path: std::path::PathBuf,
    pub baseline_offset: u64,
}

#[async_trait]
pub trait LogProvider: Send + Sync {
    async fn get_latest_reply(&self, since_offset: u64) -> Option<LogEntry>;

    async fn get_history(&self, session_id: Option<&str>, count: usize) -> Vec<HistoryEntry>;

    async fn get_current_offset(&self) -> u64;

    fn get_inode(&self) -> Option<u64>;

    fn get_watch_path(&self) -> Option<std::path::PathBuf>;

    /// Lock the current session file and return baseline offset.
    /// This ensures that subsequent get_latest_reply calls use the same session file.
    /// Call this before sending a message to the agent.
    async fn lock_session(&self) -> Option<LockedSession>;

    /// Unlock the session, allowing the provider to track new session files.
    /// Call this after reply detection completes (success or timeout).
    async fn unlock_session(&self);

    fn subscribe_changes(&self, debounce_ms: u64) -> Option<WatchSubscription> {
        let path = self.get_watch_path()?;
        let (sender, handle) = create_debounced_watcher(path, debounce_ms)?;
        Some(WatchSubscription {
            handle,
            receiver: sender.subscribe(),
        })
    }
}

pub struct NullLogProvider;

#[async_trait]
impl LogProvider for NullLogProvider {
    async fn get_latest_reply(&self, _since_offset: u64) -> Option<LogEntry> {
        None
    }

    async fn get_history(&self, _session_id: Option<&str>, _count: usize) -> Vec<HistoryEntry> {
        Vec::new()
    }

    async fn get_current_offset(&self) -> u64 {
        0
    }

    fn get_inode(&self) -> Option<u64> {
        None
    }

    fn get_watch_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    async fn lock_session(&self) -> Option<LockedSession> {
        None
    }

    async fn unlock_session(&self) {}
}

pub fn create_log_provider(
    provider_type: &str,
    config: Option<&HashMap<String, String>>,
) -> Box<dyn LogProvider> {
    tracing::debug!(
        "[LogProvider] Creating provider type='{}' with config={:?}",
        provider_type,
        config
    );

    match provider_type.to_lowercase().as_str() {
        "codex" | "codexlogprovider" => Box::new(CodexLogProvider::new(config)),
        "gemini" | "geminilogprovider" => Box::new(GeminiLogProvider::new(config)),
        "opencode" | "opencodelogprovider" => Box::new(OpenCodeLogProvider::new(config)),
        "pty" | "null" => Box::new(NullLogProvider),
        _ => {
            tracing::warn!(
                "[LogProvider] Unknown provider type '{}', using NullLogProvider",
                provider_type
            );
            Box::new(NullLogProvider)
        }
    }
}
