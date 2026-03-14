use super::AppState;
use crate::events::ReplayResult;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Notify;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum ClientMessage {
    Subscribe {
        #[serde(default)]
        from_seq: u64,
    },
    Prompt {
        session_id: String,
        text: String,
    },
    Cancel {
        session_id: String,
    },
    PermissionResponse {
        session_id: String,
        id: String,
        granted: bool,
    },
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (ws_sender, mut receiver) = socket.split();
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let event_log = state.session_manager.event_log().clone();
    let mut event_rx = event_log.subscribe();
    let replay_ready = Arc::new(Notify::new());
    let replay_started = Arc::new(AtomicBool::new(false));
    let replay_seq = Arc::new(AtomicU64::new(0));

    let (action_tx, mut action_rx) = mpsc::channel::<ClientMessage>(32);

    // Send task: forward events to client
    let sender_clone = ws_sender.clone();
    let replay_ready_send = replay_ready.clone();
    let replay_started_send = replay_started.clone();
    let replay_seq_send = replay_seq.clone();
    let send_task = tokio::spawn(async move {
        loop {
            let notified = replay_ready_send.notified();
            if replay_started_send.load(Ordering::SeqCst) {
                break;
            }
            notified.await;
        }

        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    let last_replayed_seq = replay_seq_send.load(Ordering::SeqCst);
                    if event.seq <= last_replayed_seq {
                        continue;
                    }
                    replay_seq_send.store(event.seq, Ordering::SeqCst);
                    if let Ok(json) = serde_json::to_string(&event) {
                        let mut sender = sender_clone.lock().await;
                        if sender.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WebSocket lagged by {} events", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Recv task: parse client messages
    let action_tx_clone = action_tx.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<ClientMessage>(&text) {
                        if action_tx_clone.send(cmd).await.is_err() {
                            break;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Action task: handle client commands
    let sm = state.session_manager.clone();
    let sender_clone2 = ws_sender.clone();
    let replay_ready_action = replay_ready.clone();
    let replay_started_action = replay_started.clone();
    let replay_seq_action = replay_seq.clone();
    let action_task = tokio::spawn(async move {
        while let Some(cmd) = action_rx.recv().await {
            match cmd {
                ClientMessage::Subscribe { from_seq } => {
                    let replay = event_log.replay_from(from_seq);
                    let (events, gap) = match replay {
                        ReplayResult::Complete(evts) => (evts, None),
                        ReplayResult::Partial {
                            events,
                            oldest_available_seq,
                        } => (events, Some(oldest_available_seq)),
                    };

                    let mut sender = sender_clone2.lock().await;
                    if let Some(oldest) = gap {
                        let gap_msg = serde_json::json!({
                            "type": "replay_gap",
                            "oldest_available_seq": oldest,
                            "requested_seq": from_seq,
                        });
                        let _ = sender
                            .send(Message::Text(serde_json::to_string(&gap_msg).unwrap()))
                            .await;
                    }
                    let max_replayed_seq = events.last().map(|event| event.seq).unwrap_or(0);
                    for event in events {
                        if let Ok(json) = serde_json::to_string(&event) {
                            if sender.send(Message::Text(json)).await.is_err() {
                                return;
                            }
                        }
                    }

                    replay_seq_action.store(max_replayed_seq, Ordering::SeqCst);
                    replay_started_action.store(true, Ordering::SeqCst);
                    replay_ready_action.notify_waiters();
                }
                ClientMessage::PermissionResponse {
                    session_id,
                    id,
                    granted,
                } => {
                    if let Some(session) = sm.get_by_id(&session_id).await {
                        session.respond_to_permission(&id, granted).await;
                    }
                }
                ClientMessage::Prompt { .. } | ClientMessage::Cancel { .. } => {
                    // Prompt/cancel via WebSocket is not supported; use REST API
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
        _ = action_task => {},
    }
}
