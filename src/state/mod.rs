use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessState {
    #[default]
    Stopped,
    Starting,
    Running,
    Dead,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => write!(f, "STOPPED"),
            Self::Starting => write!(f, "STARTING"),
            Self::Running => write!(f, "RUNNING"),
            Self::Dead => write!(f, "DEAD"),
        }
    }
}

impl ProcessState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    pub fn can_start(&self) -> bool {
        matches!(self, Self::Stopped | Self::Dead)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnState {
    #[default]
    Idle,
    Prompting,
    Cancelling,
}

impl fmt::Display for TurnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "IDLE"),
            Self::Prompting => write!(f, "PROMPTING"),
            Self::Cancelling => write!(f, "CANCELLING"),
        }
    }
}

impl TurnState {
    pub fn can_prompt(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn can_cancel(&self) -> bool {
        matches!(self, Self::Prompting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_state_default() {
        assert_eq!(ProcessState::default(), ProcessState::Stopped);
    }

    #[test]
    fn test_process_state_can_start() {
        assert!(ProcessState::Stopped.can_start());
        assert!(ProcessState::Dead.can_start());
        assert!(!ProcessState::Starting.can_start());
        assert!(!ProcessState::Running.can_start());
    }

    #[test]
    fn test_process_state_is_running() {
        assert!(ProcessState::Starting.is_running());
        assert!(ProcessState::Running.is_running());
        assert!(!ProcessState::Stopped.is_running());
        assert!(!ProcessState::Dead.is_running());
    }

    #[test]
    fn test_turn_state_default() {
        assert_eq!(TurnState::default(), TurnState::Idle);
    }

    #[test]
    fn test_turn_state_can_prompt() {
        assert!(TurnState::Idle.can_prompt());
        assert!(!TurnState::Prompting.can_prompt());
        assert!(!TurnState::Cancelling.can_prompt());
    }

    #[test]
    fn test_turn_state_can_cancel() {
        assert!(TurnState::Prompting.can_cancel());
        assert!(!TurnState::Idle.can_cancel());
        assert!(!TurnState::Cancelling.can_cancel());
    }

    #[test]
    fn test_display() {
        assert_eq!(ProcessState::Stopped.to_string(), "STOPPED");
        assert_eq!(ProcessState::Running.to_string(), "RUNNING");
        assert_eq!(TurnState::Idle.to_string(), "IDLE");
        assert_eq!(TurnState::Prompting.to_string(), "PROMPTING");
    }
}
