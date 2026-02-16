#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStage {
    Queued,
    InFlight,
    Acked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    BecameInFlight { attempt: u32 },
    BecameAcked { attempts: u32 },
    RetryQueued { next_attempt: u32 },
    Ignored { state: PublishStage },
}

pub struct PublishStateMachine {
    stage: PublishStage,
    attempts: u32,
}

impl Default for PublishStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl PublishStateMachine {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stage: PublishStage::Queued,
            attempts: 0,
        }
    }

    #[must_use]
    pub const fn stage(&self) -> PublishStage {
        self.stage
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn start_publish(&mut self) -> PublishOutcome {
        match self.stage {
            PublishStage::Queued => {
                self.attempts += 1;
                self.stage = PublishStage::InFlight;
                PublishOutcome::BecameInFlight {
                    attempt: self.attempts,
                }
            }
            _ => PublishOutcome::Ignored { state: self.stage },
        }
    }

    pub fn ack(&mut self) -> PublishOutcome {
        match self.stage {
            PublishStage::InFlight => {
                self.stage = PublishStage::Acked;
                PublishOutcome::BecameAcked {
                    attempts: self.attempts,
                }
            }
            _ => PublishOutcome::Ignored { state: self.stage },
        }
    }

    pub fn retry(&mut self) -> PublishOutcome {
        match self.stage {
            PublishStage::InFlight => {
                self.stage = PublishStage::Queued;
                PublishOutcome::RetryQueued {
                    next_attempt: self.attempts + 1,
                }
            }
            _ => PublishOutcome::Ignored { state: self.stage },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishOutcome, PublishStage, PublishStateMachine};

    #[test]
    fn queued_record_transitions_to_inflight_then_acked() {
        let mut state = PublishStateMachine::new();

        assert_eq!(state.stage(), PublishStage::Queued);
        assert_eq!(
            state.start_publish(),
            PublishOutcome::BecameInFlight { attempt: 1 }
        );
        assert_eq!(state.stage(), PublishStage::InFlight);
        assert_eq!(state.ack(), PublishOutcome::BecameAcked { attempts: 1 });
        assert_eq!(state.stage(), PublishStage::Acked);
    }

    #[test]
    fn retry_moves_inflight_back_to_queue_for_next_attempt() {
        let mut state = PublishStateMachine::new();

        assert_eq!(
            state.start_publish(),
            PublishOutcome::BecameInFlight { attempt: 1 }
        );
        assert_eq!(
            state.retry(),
            PublishOutcome::RetryQueued { next_attempt: 2 }
        );
        assert_eq!(state.stage(), PublishStage::Queued);
        assert_eq!(
            state.start_publish(),
            PublishOutcome::BecameInFlight { attempt: 2 }
        );
        assert_eq!(state.attempts(), 2);
    }
}
