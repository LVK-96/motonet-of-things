#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStage {
    Queued,
    InFlight,
    Acked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishTransition {
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

    pub fn start_publish(&mut self) -> PublishTransition {
        match self.stage {
            PublishStage::Queued => {
                self.attempts += 1;
                self.stage = PublishStage::InFlight;
                PublishTransition::BecameInFlight {
                    attempt: self.attempts,
                }
            }
            _ => PublishTransition::Ignored { state: self.stage },
        }
    }

    pub fn ack(&mut self) -> PublishTransition {
        match self.stage {
            PublishStage::InFlight => {
                self.stage = PublishStage::Acked;
                PublishTransition::BecameAcked {
                    attempts: self.attempts,
                }
            }
            _ => PublishTransition::Ignored { state: self.stage },
        }
    }

    pub fn retry(&mut self) -> PublishTransition {
        match self.stage {
            PublishStage::InFlight => {
                self.stage = PublishStage::Queued;
                PublishTransition::RetryQueued {
                    next_attempt: self.attempts + 1,
                }
            }
            _ => PublishTransition::Ignored { state: self.stage },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Published,
    RetryLater,
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginPublishError {
    Busy,
}

pub struct PublishPipelineState<T: Copy> {
    pending_retry: Option<T>,
    in_flight: Option<T>,
    reconnect_required: bool,
}

impl<T: Copy> Default for PublishPipelineState<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> PublishPipelineState<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending_retry: None,
            in_flight: None,
            reconnect_required: false,
        }
    }

    #[must_use]
    pub const fn pending_retry(&self) -> Option<T> {
        self.pending_retry
    }

    #[must_use]
    pub const fn in_flight(&self) -> Option<T> {
        self.in_flight
    }

    #[must_use]
    pub const fn reconnect_required(&self) -> bool {
        self.reconnect_required
    }

    #[must_use]
    pub const fn has_pending_retry(&self) -> bool {
        self.pending_retry.is_some()
    }

    #[must_use]
    pub const fn pending_retry_count(&self) -> usize {
        if self.pending_retry.is_some() { 1 } else { 0 }
    }

    pub fn clear_reconnect_required(&mut self) {
        self.reconnect_required = false;
    }

    pub fn begin_retry(&mut self) -> Option<T> {
        if self.in_flight.is_some() {
            return None;
        }

        let retry_item = self.pending_retry.take();
        if let Some(item) = retry_item {
            self.in_flight = Some(item);
        }

        retry_item
    }

    pub fn begin_new(&mut self, item: T) -> Result<(), BeginPublishError> {
        if self.in_flight.is_some() || self.pending_retry.is_some() {
            return Err(BeginPublishError::Busy);
        }

        self.in_flight = Some(item);
        Ok(())
    }

    pub fn complete_in_flight(&mut self, outcome: PublishOutcome) {
        if let Some(item) = self.in_flight.take() {
            match outcome {
                PublishOutcome::Published | PublishOutcome::Dropped => {}
                PublishOutcome::RetryLater => {
                    self.pending_retry = Some(item);
                    self.reconnect_required = true;
                }
            }
        }
    }

    pub fn mark_ping_failed(&mut self) {
        self.reconnect_required = true;
        if let Some(item) = self.in_flight.take() {
            self.pending_retry = Some(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BeginPublishError, PublishOutcome, PublishPipelineState, PublishStage, PublishStateMachine,
        PublishTransition,
    };
    use crate::TelemetryRecord;
    use crate::queue::{DropPolicy, TelemetryQueue};

    #[test]
    fn queued_record_transitions_to_inflight_then_acked() {
        let mut state = PublishStateMachine::new();

        assert_eq!(state.stage(), PublishStage::Queued);
        assert_eq!(
            state.start_publish(),
            PublishTransition::BecameInFlight { attempt: 1 }
        );
        assert_eq!(state.stage(), PublishStage::InFlight);
        assert_eq!(state.ack(), PublishTransition::BecameAcked { attempts: 1 });
        assert_eq!(state.stage(), PublishStage::Acked);
    }

    #[test]
    fn retry_moves_inflight_back_to_queue_for_next_attempt() {
        let mut state = PublishStateMachine::new();

        assert_eq!(
            state.start_publish(),
            PublishTransition::BecameInFlight { attempt: 1 }
        );
        assert_eq!(
            state.retry(),
            PublishTransition::RetryQueued { next_attempt: 2 }
        );
        assert_eq!(state.stage(), PublishStage::Queued);
        assert_eq!(
            state.start_publish(),
            PublishTransition::BecameInFlight { attempt: 2 }
        );
        assert_eq!(state.attempts(), 2);
    }

    const fn record(
        sensor_id: u8,
        channel: u8,
        temperature_deci_c: i16,
        battery_ok: bool,
    ) -> TelemetryRecord {
        TelemetryRecord {
            sensor_id,
            channel,
            temperature_deci_c,
            battery_ok,
        }
    }

    #[test]
    fn publish_failure_retains_exactly_one_in_flight_retry_item() {
        let failed = record(1, 1, 215, true);
        let mut publish_state = PublishPipelineState::new();

        assert!(publish_state.begin_new(failed).is_ok());
        publish_state.complete_in_flight(PublishOutcome::RetryLater);

        assert_eq!(publish_state.pending_retry(), Some(failed));
        assert_eq!(publish_state.pending_retry_count(), 1);
        assert_eq!(
            publish_state.begin_new(record(9, 1, 230, true)),
            Err(BeginPublishError::Busy)
        );
    }

    #[test]
    fn reconnect_publishes_pending_item_before_draining_new_queue() {
        let pending = record(2, 1, 180, true);
        let queued = record(3, 1, 181, true);
        let mut publish_state = PublishPipelineState::new();
        let mut queue = TelemetryQueue::<4>::new(DropPolicy::DropOldest);

        assert!(publish_state.begin_new(pending).is_ok());
        publish_state.complete_in_flight(PublishOutcome::RetryLater);
        let _ = queue.enqueue(queued);

        assert_eq!(publish_state.begin_retry(), Some(pending));
        assert_eq!(queue.len(), 1);
        publish_state.complete_in_flight(PublishOutcome::Published);

        assert_eq!(queue.dequeue(), Some(queued));
    }

    #[test]
    fn ping_failure_transitions_to_reconnect_without_dropping_in_flight_data() {
        let in_flight = record(7, 2, -55, false);
        let mut publish_state = PublishPipelineState::new();

        assert!(publish_state.begin_new(in_flight).is_ok());
        publish_state.mark_ping_failed();

        assert!(publish_state.reconnect_required());
        assert_eq!(publish_state.in_flight(), None);
        assert_eq!(publish_state.pending_retry(), Some(in_flight));
        assert_eq!(publish_state.pending_retry_count(), 1);
    }
}
