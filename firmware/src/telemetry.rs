use crate::messages::RadioReading;
use telemetry_core::TelemetryRecord;
use telemetry_core::dedupe::{DedupeCache, DedupeOutcome};

#[cfg(not(test))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(not(test))]
use embassy_sync::channel::Sender as ChannelSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryEnqueueOutcome {
    Queued,
    DroppedByPolicy,
    RejectedAsDuplicate,
}

pub struct TelemetryPipelineAdapter<const DEDUPE_CAPACITY: usize> {
    dedupe: DedupeCache<DEDUPE_CAPACITY>,
    dropped_total: u64,
    rejected_duplicates_total: u64,
    drop_log_interval_ms: u64,
    next_drop_log_ms: u64,
}

impl<const DEDUPE_CAPACITY: usize> Default for TelemetryPipelineAdapter<DEDUPE_CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const DEDUPE_CAPACITY: usize> TelemetryPipelineAdapter<DEDUPE_CAPACITY> {
    pub const DEFAULT_DEDUPE_TTL_MS: u64 = 30_000;
    pub const DEFAULT_DROP_LOG_INTERVAL_MS: u64 = 60_000;

    #[must_use]
    pub const fn new() -> Self {
        Self::with_windows(
            Self::DEFAULT_DEDUPE_TTL_MS,
            Self::DEFAULT_DROP_LOG_INTERVAL_MS,
        )
    }

    #[must_use]
    pub const fn with_windows(dedupe_ttl_ms: u64, drop_log_interval_ms: u64) -> Self {
        Self {
            dedupe: DedupeCache::new(dedupe_ttl_ms),
            dropped_total: 0,
            rejected_duplicates_total: 0,
            drop_log_interval_ms,
            next_drop_log_ms: 0,
        }
    }

    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    #[must_use]
    pub const fn rejected_duplicates_total(&self) -> u64 {
        self.rejected_duplicates_total
    }

    #[cfg(not(test))]
    pub fn enqueue_for_channel<const N: usize>(
        &mut self,
        reading: RadioReading,
        now_ms: u64,
        sender: &ChannelSender<'static, CriticalSectionRawMutex, RadioReading, N>,
    ) -> TelemetryEnqueueOutcome {
        self.enqueue_with(reading, now_ms, |queued_reading| {
            sender.try_send(queued_reading).is_ok()
        })
    }

    pub fn enqueue_with<F>(
        &mut self,
        reading: RadioReading,
        now_ms: u64,
        mut enqueue: F,
    ) -> TelemetryEnqueueOutcome
    where
        F: FnMut(RadioReading) -> bool,
    {
        if matches!(
            self.dedupe
                .observe(map_radio_reading_to_record(reading), now_ms),
            DedupeOutcome::Duplicate
        ) {
            self.rejected_duplicates_total += 1;
            return TelemetryEnqueueOutcome::RejectedAsDuplicate;
        }

        if enqueue(reading) {
            TelemetryEnqueueOutcome::Queued
        } else {
            self.dropped_total += 1;
            self.maybe_log_drop_metrics(now_ms);
            TelemetryEnqueueOutcome::DroppedByPolicy
        }
    }

    fn maybe_log_drop_metrics(&mut self, now_ms: u64) {
        if now_ms < self.next_drop_log_ms {
            return;
        }

        log_drop_metrics(self.dropped_total, self.rejected_duplicates_total);
        self.next_drop_log_ms = now_ms.saturating_add(self.drop_log_interval_ms);
    }
}

#[must_use]
pub fn map_radio_reading_to_record(reading: RadioReading) -> TelemetryRecord {
    reading.to_telemetry_record()
}

#[cfg(not(test))]
fn log_drop_metrics(dropped_total: u64, rejected_duplicates_total: u64) {
    defmt::warn!(
        "Telemetry queue pressure: dropped_total={} rejected_duplicates_total={}",
        dropped_total,
        rejected_duplicates_total
    );
}

#[cfg(test)]
fn log_drop_metrics(_dropped_total: u64, _rejected_duplicates_total: u64) {}

#[cfg(not(test))]
#[must_use]
pub fn now_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}

#[cfg(test)]
mod tests {
    use rubicson::RubicsonReading;
    use telemetry_core::TelemetryRecord;

    use super::{TelemetryEnqueueOutcome, TelemetryPipelineAdapter, map_radio_reading_to_record};
    use crate::messages::RadioReading;

    fn sample_reading(temp_c: f32) -> RadioReading {
        RadioReading {
            inner: RubicsonReading {
                id: 0x42,
                channel: 3,
                battery_ok: true,
                temperature_c: temp_c,
                crc_ok: true,
            },
            rssi: -78,
            detection_threshold: 16,
            received_at: embassy_time::Instant::now(),
        }
    }

    #[test]
    fn maps_radio_reading_to_telemetry_record() {
        let reading = RadioReading {
            inner: RubicsonReading {
                id: 0x12,
                channel: 2,
                battery_ok: false,
                temperature_c: -10.5,
                crc_ok: true,
            },
            rssi: -81,
            detection_threshold: 12,
            received_at: embassy_time::Instant::now(),
        };

        assert_eq!(
            map_radio_reading_to_record(reading),
            TelemetryRecord {
                sensor_id: 0x12,
                channel: 2,
                temperature_deci_c: -105,
                battery_ok: false,
            }
        );
    }

    #[test]
    fn drop_counter_increments_on_queue_pressure() {
        let mut adapter = TelemetryPipelineAdapter::<16>::new();

        assert_eq!(
            adapter.enqueue_with(sample_reading(21.1), 1_000, |_| true),
            TelemetryEnqueueOutcome::Queued
        );
        assert_eq!(adapter.dropped_total(), 0);

        assert_eq!(
            adapter.enqueue_with(sample_reading(21.6), 2_000, |_| false),
            TelemetryEnqueueOutcome::DroppedByPolicy
        );
        assert_eq!(adapter.dropped_total(), 1);
    }

    #[test]
    fn duplicate_readings_are_rejected_before_queue_pressure() {
        let mut adapter = TelemetryPipelineAdapter::<16>::with_windows(30_000, 60_000);
        let reading = sample_reading(21.1);

        assert_eq!(
            adapter.enqueue_with(reading, 1_000, |_| true),
            TelemetryEnqueueOutcome::Queued
        );
        assert_eq!(
            adapter.enqueue_with(reading, 2_000, |_| false),
            TelemetryEnqueueOutcome::RejectedAsDuplicate
        );
        assert_eq!(adapter.dropped_total(), 0);
        assert_eq!(adapter.rejected_duplicates_total(), 1);
    }
}
