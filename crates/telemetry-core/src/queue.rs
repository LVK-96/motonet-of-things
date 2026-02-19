use heapless::Deque;

use crate::TelemetryRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPolicy {
    DropOldest,
    DropNewest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Enqueued,
    DroppedOldest,
    DroppedNewest,
}

pub struct TelemetryQueue<const N: usize> {
    records: Deque<TelemetryRecord, N>,
    drop_policy: DropPolicy,
    dropped_total: u64,
}

impl<const N: usize> TelemetryQueue<N> {
    #[must_use]
    pub const fn new(drop_policy: DropPolicy) -> Self {
        Self {
            records: Deque::new(),
            drop_policy,
            dropped_total: 0,
        }
    }

    pub fn enqueue(&mut self, record: TelemetryRecord) -> EnqueueOutcome {
        match self.records.push_back(record) {
            Ok(()) => EnqueueOutcome::Enqueued,
            Err(rejected_record) => {
                self.dropped_total += 1;
                match self.drop_policy {
                    DropPolicy::DropOldest => {
                        let _ = self.records.pop_front();
                        let _ = self.records.push_back(rejected_record);
                        EnqueueOutcome::DroppedOldest
                    }
                    DropPolicy::DropNewest => EnqueueOutcome::DroppedNewest,
                }
            }
        }
    }

    pub fn dequeue(&mut self) -> Option<TelemetryRecord> {
        self.records.pop_front()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    #[must_use]
    pub const fn drop_policy(&self) -> DropPolicy {
        self.drop_policy
    }
}

#[cfg(test)]
mod tests {
    use super::{DropPolicy, EnqueueOutcome, TelemetryQueue};
    use crate::TelemetryRecord;

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
            timestamp: 0,
        }
    }

    #[test]
    fn drop_oldest_evicts_head_and_keeps_latest_reading() {
        let mut queue = TelemetryQueue::<2>::new(DropPolicy::DropOldest);

        assert_eq!(
            queue.enqueue(record(1, 1, 215, true)),
            EnqueueOutcome::Enqueued
        );
        assert_eq!(
            queue.enqueue(record(2, 1, 220, true)),
            EnqueueOutcome::Enqueued
        );
        assert_eq!(
            queue.enqueue(record(3, 1, 225, true)),
            EnqueueOutcome::DroppedOldest
        );

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped_total(), 1);
        assert_eq!(queue.dequeue(), Some(record(2, 1, 220, true)));
        assert_eq!(queue.dequeue(), Some(record(3, 1, 225, true)));
        assert_eq!(queue.dequeue(), None);
    }

    #[test]
    fn drop_newest_rejects_incoming_reading_when_full() {
        let mut queue = TelemetryQueue::<2>::new(DropPolicy::DropNewest);

        assert_eq!(
            queue.enqueue(record(1, 1, 215, true)),
            EnqueueOutcome::Enqueued
        );
        assert_eq!(
            queue.enqueue(record(2, 1, 220, true)),
            EnqueueOutcome::Enqueued
        );
        assert_eq!(
            queue.enqueue(record(3, 1, 225, true)),
            EnqueueOutcome::DroppedNewest
        );

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped_total(), 1);
        assert_eq!(queue.dequeue(), Some(record(1, 1, 215, true)));
        assert_eq!(queue.dequeue(), Some(record(2, 1, 220, true)));
        assert_eq!(queue.dequeue(), None);
    }
}
