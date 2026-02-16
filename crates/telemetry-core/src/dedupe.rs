use heapless::Vec;

use crate::TelemetryRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedupeOutcome {
    New,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DedupeKey {
    sensor_id: u8,
    channel: u8,
    temperature_deci_c: i16,
    battery_ok: bool,
}

pub struct DedupeCache<const N: usize> {
    ttl_ms: u64,
    entries: Vec<CacheEntry, N>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheEntry {
    key: DedupeKey,
    expires_at: u64,
}

impl<const N: usize> DedupeCache<N> {
    #[must_use]
    pub const fn new(ttl_ms: u64) -> Self {
        Self {
            ttl_ms,
            entries: Vec::new(),
        }
    }

    pub fn observe(&mut self, record: TelemetryRecord, now_ms: u64) -> DedupeOutcome {
        self.prune_expired(now_ms);

        let key = DedupeKey {
            sensor_id: record.sensor_id,
            channel: record.channel,
            temperature_deci_c: record.temperature_deci_c,
            battery_ok: record.battery_ok,
        };

        // TODO: Should this be a hash map?
        if self.entries.iter().any(|entry| entry.key == key) {
            return DedupeOutcome::Duplicate;
        }

        let new_entry = CacheEntry {
            key,
            expires_at: now_ms.saturating_add(self.ttl_ms),
        };

        if self.entries.push(new_entry).is_ok() {
            return DedupeOutcome::New;
        }

        self.evict_oldest();
        let _ = self.entries.push(new_entry);
        DedupeOutcome::New
    }

    fn prune_expired(&mut self, now_ms: u64) {
        self.entries.retain(|entry| now_ms < entry.expires_at);
    }

    fn evict_oldest(&mut self) {
        if let Some((idx, _)) = self.entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.expires_at)
        {
            let _ = self.entries.remove(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DedupeCache, DedupeOutcome};
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
        }
    }

    #[test]
    fn duplicate_is_blocked_within_ttl_for_same_identity_tuple() {
        let mut dedupe = DedupeCache::<8>::new(30_000);
        let sample = record(7, 2, 185, true);

        assert_eq!(dedupe.observe(sample, 1_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(sample, 20_000), DedupeOutcome::Duplicate);
    }

    #[test]
    fn dedupe_key_is_sensor_channel_temperature_and_battery() {
        let mut dedupe = DedupeCache::<8>::new(30_000);

        let base = record(7, 2, 185, true);
        let changed_sensor = record(8, 2, 185, true);
        let changed_channel = record(7, 3, 185, true);
        let changed_battery = record(7, 2, 185, false);
        let changed_temp = record(7, 2, 190, true);

        assert_eq!(dedupe.observe(base, 1_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(changed_sensor, 2_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(changed_channel, 3_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(changed_battery, 4_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(changed_temp, 5_000), DedupeOutcome::New);
    }

    #[test]
    fn duplicate_expires_after_ttl() {
        let mut dedupe = DedupeCache::<8>::new(30_000);
        let sample = record(7, 2, 185, true);

        assert_eq!(dedupe.observe(sample, 1_000), DedupeOutcome::New);
        assert_eq!(dedupe.observe(sample, 20_000), DedupeOutcome::Duplicate);
        assert_eq!(dedupe.observe(sample, 31_001), DedupeOutcome::New);
    }
}
