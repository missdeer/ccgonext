//! Agent state machine implementation
//!
//! States: STOPPED -> STARTING -> IDLE/READY_TIMEOUT -> BUSY -> IDLE -> DEAD

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentState {
    /// Agent not started
    #[default]
    Stopped,
    /// Agent starting, waiting for ready detection
    Starting,
    /// Ready detection timed out but process is alive, may be usable
    ReadyTimeout,
    /// Agent idle, ready to accept requests
    Idle,
    /// Agent processing a request
    Busy,
    /// Request timed out but process alive, queue blocked
    Stuck,
    /// Process exited or crashed
    Dead,
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => write!(f, "STOPPED"),
            Self::Starting => write!(f, "STARTING"),
            Self::ReadyTimeout => write!(f, "READY_TIMEOUT"),
            Self::Idle => write!(f, "IDLE"),
            Self::Busy => write!(f, "BUSY"),
            Self::Stuck => write!(f, "STUCK"),
            Self::Dead => write!(f, "DEAD"),
        }
    }
}

impl AgentState {
    pub fn can_accept_request(&self) -> bool {
        matches!(self, Self::Idle | Self::ReadyTimeout)
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::ReadyTimeout | Self::Idle | Self::Busy | Self::Stuck
        )
    }

    pub fn can_start(&self) -> bool {
        matches!(self, Self::Stopped | Self::Dead)
    }

    pub fn can_stop(&self) -> bool {
        self.is_running()
    }

    pub fn can_interrupt(&self) -> bool {
        matches!(self, Self::Busy | Self::Stuck)
    }
}

#[derive(Debug, Clone)]
pub enum StateTransition {
    StartAgent,
    ReadyDetected,
    ReadyTimeout,
    ProcessExit { exit_code: Option<i32> },
    AskAgent { message_id: String },
    ReplyReceived,
    RequestTimeout,
    Interrupted,
    ForceReset,
    AutoRestart,
}

impl fmt::Display for StateTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartAgent => write!(f, "start_agent"),
            Self::ReadyDetected => write!(f, "ready_detected"),
            Self::ReadyTimeout => write!(f, "ready_timeout"),
            Self::ProcessExit { exit_code } => {
                write!(f, "process_exit({:?})", exit_code)
            }
            Self::AskAgent { message_id } => write!(f, "ask_agent({})", message_id),
            Self::ReplyReceived => write!(f, "reply_received"),
            Self::RequestTimeout => write!(f, "request_timeout"),
            Self::Interrupted => write!(f, "interrupted"),
            Self::ForceReset => write!(f, "force_reset"),
            Self::AutoRestart => write!(f, "auto_restart"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransitionResult {
    pub new_state: AgentState,
    pub side_effects: Vec<SideEffect>,
}

#[derive(Debug, Clone)]
pub enum SideEffect {
    CreatePty,
    StartProcess,
    MarkReady,
    LogWarning(String),
    SendMessage { message_id: String },
    ReturnResult,
    ReturnTimeoutError,
    StartBackgroundRecovery,
    KillProcess,
    ClearQueue,
    NotifyWaiters(String),
    TriggerAutoRestart,
}

pub struct StateMachine;

impl StateMachine {
    pub fn transition(
        current: AgentState,
        event: StateTransition,
    ) -> Result<TransitionResult, StateError> {
        match (current, &event) {
            // STOPPED -> STARTING
            (AgentState::Stopped, StateTransition::StartAgent) => Ok(TransitionResult {
                new_state: AgentState::Starting,
                side_effects: vec![SideEffect::CreatePty, SideEffect::StartProcess],
            }),

            // STARTING -> IDLE (ready detected)
            (AgentState::Starting, StateTransition::ReadyDetected) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::MarkReady],
            }),

            // STARTING -> READY_TIMEOUT
            (AgentState::Starting, StateTransition::ReadyTimeout) => Ok(TransitionResult {
                new_state: AgentState::ReadyTimeout,
                side_effects: vec![SideEffect::LogWarning(
                    "Ready detection timed out, agent may still be usable".to_string(),
                )],
            }),

            // READY_TIMEOUT -> IDLE (ready eventually detected)
            (AgentState::ReadyTimeout, StateTransition::ReadyDetected) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::MarkReady],
            }),

            // STARTING -> DEAD (process exit)
            (AgentState::Starting, StateTransition::ProcessExit { .. }) => Ok(TransitionResult {
                new_state: AgentState::Dead,
                side_effects: vec![SideEffect::NotifyWaiters(
                    "Agent failed to start".to_string(),
                )],
            }),

            // IDLE/READY_TIMEOUT -> BUSY
            (
                AgentState::Idle | AgentState::ReadyTimeout,
                StateTransition::AskAgent { message_id },
            ) => Ok(TransitionResult {
                new_state: AgentState::Busy,
                side_effects: vec![SideEffect::SendMessage {
                    message_id: message_id.clone(),
                }],
            }),

            // BUSY -> IDLE (reply received)
            (AgentState::Busy, StateTransition::ReplyReceived) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::ReturnResult],
            }),

            // BUSY -> IDLE (timeout)
            (AgentState::Busy, StateTransition::RequestTimeout) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::ReturnTimeoutError],
            }),

            // BUSY -> IDLE (interrupted)
            (AgentState::Busy, StateTransition::Interrupted) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::ClearQueue],
            }),

            // STUCK -> IDLE (reply received)
            (AgentState::Stuck, StateTransition::ReplyReceived) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::ReturnResult],
            }),

            // STUCK -> DEAD (force reset or process exit) - with auto-restart for process exit
            (AgentState::Stuck, StateTransition::ForceReset) => Ok(TransitionResult {
                new_state: AgentState::Dead,
                side_effects: vec![SideEffect::KillProcess, SideEffect::ClearQueue],
            }),

            (AgentState::Stuck, StateTransition::ProcessExit { .. }) => Ok(TransitionResult {
                new_state: AgentState::Dead,
                side_effects: vec![SideEffect::ClearQueue, SideEffect::TriggerAutoRestart],
            }),

            // STUCK -> IDLE (interrupted)
            (AgentState::Stuck, StateTransition::Interrupted) => Ok(TransitionResult {
                new_state: AgentState::Idle,
                side_effects: vec![SideEffect::ClearQueue],
            }),

            // ANY running -> DEAD (unexpected process exit)
            (state, StateTransition::ProcessExit { .. }) if state.is_running() => {
                Ok(TransitionResult {
                    new_state: AgentState::Dead,
                    side_effects: vec![
                        SideEffect::NotifyWaiters("Agent process exited unexpectedly".to_string()),
                        SideEffect::TriggerAutoRestart,
                    ],
                })
            }

            // DEAD -> STARTING (restart)
            (AgentState::Dead, StateTransition::AutoRestart | StateTransition::StartAgent) => {
                Ok(TransitionResult {
                    new_state: AgentState::Starting,
                    side_effects: vec![SideEffect::CreatePty, SideEffect::StartProcess],
                })
            }

            // Invalid transition
            (state, event) => Err(StateError::InvalidTransition {
                from: state,
                event: event.clone(),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("Invalid state transition: {from} + {event}")]
    InvalidTransition {
        from: AgentState,
        event: StateTransition,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stopped_to_starting() {
        let result = StateMachine::transition(AgentState::Stopped, StateTransition::StartAgent);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().new_state, AgentState::Starting);
    }

    #[test]
    fn test_starting_to_idle() {
        let result = StateMachine::transition(AgentState::Starting, StateTransition::ReadyDetected);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().new_state, AgentState::Idle);
    }

    #[test]
    fn test_idle_to_busy() {
        let result = StateMachine::transition(
            AgentState::Idle,
            StateTransition::AskAgent {
                message_id: "test-123".to_string(),
            },
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().new_state, AgentState::Busy);
    }

    #[test]
    fn test_busy_to_idle_on_timeout() {
        let result = StateMachine::transition(AgentState::Busy, StateTransition::RequestTimeout);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().new_state, AgentState::Idle);
    }

    #[test]
    fn test_invalid_transition() {
        let result = StateMachine::transition(AgentState::Stopped, StateTransition::ReplyReceived);
        assert!(result.is_err());
    }
}
