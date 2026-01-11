//! EU Daylight Saving Time calculation for no_std environments.
//!
//! This crate provides timezone offset calculation with automatic DST handling
//! for European Union timezone rules. DST transitions follow EU regulations:
//! - DST starts: Last Sunday of March at 01:00 UTC
//! - DST ends: Last Sunday of October at 01:00 UTC
//!
//! # Example
//!
//! ```rust
//! use eu_dst::Timezone;
//!
//! // Finland uses EET (UTC+2) / EEST (UTC+3)
//! let tz = Timezone::new(2 * 3600); // base offset 2 hours
//!
//! // Get the offset for a specific Unix timestamp
//! let unix_secs = 1625140800; // 2021-07-01 12:00:00 UTC (summer)
//! let offset = tz.offset_secs(unix_secs);
//! assert_eq!(offset, 3 * 3600); // UTC+3 during DST
//! ```

// https://valtioneuvosto.fi/hanke?tunnus=LVM070:00/2018 :D

#![cfg_attr(not(test), no_std)]

/// DST offset (additional hour in summer)
const DST_OFFSET_SECS: i64 = 3600;

/// Timezone with EU DST rules.
///
/// EU DST rules:
/// - DST starts: Last Sunday of March at 01:00 UTC
/// - DST ends: Last Sunday of October at 01:00 UTC
#[derive(Clone, Copy, Debug)]
pub struct Timezone {
    /// Base timezone offset in seconds (e.g., 2*3600 for EET)
    base_offset_secs: i64,
}

impl Timezone {
    /// Create a new timezone with the given base offset in seconds.
    ///
    /// # Arguments
    ///
    /// * `base_offset_secs` - The base timezone offset from UTC in seconds.
    ///   For example, EET (Eastern European Time) is UTC+2, so pass `2 * 3600`.
    pub const fn new(base_offset_secs: i64) -> Self {
        Self { base_offset_secs }
    }

    /// Get the timezone offset in seconds for a given Unix timestamp,
    /// accounting for DST.
    ///
    /// Returns `base_offset + 3600` during summer time, `base_offset` during winter.
    pub fn offset_secs(&self, unix_secs: u64) -> i64 {
        if is_dst_active(unix_secs) {
            self.base_offset_secs + DST_OFFSET_SECS
        } else {
            self.base_offset_secs
        }
    }

    /// Check if DST is currently active for the given Unix timestamp.
    pub fn is_dst(&self, unix_secs: u64) -> bool {
        is_dst_active(unix_secs)
    }

    /// Convert a Unix timestamp to local time, returning (hours, minutes, seconds).
    pub fn to_local_hms(&self, unix_secs: u64) -> (u32, u32, u32) {
        let offset = self.offset_secs(unix_secs);
        let local_secs = (unix_secs as i64 + offset) as u64;
        let secs_today = local_secs % 86400;
        let hours = (secs_today / 3600) as u32;
        let minutes = ((secs_today % 3600) / 60) as u32;
        let seconds = (secs_today % 60) as u32;
        (hours, minutes, seconds)
    }
}

/// Common timezone presets
impl Timezone {
    /// Western European Time (WET) - UTC+0 / UTC+1 DST
    /// Used by: UK, Ireland, Portugal
    pub const WET: Self = Self::new(0);

    /// Central European Time (CET) - UTC+1 / UTC+2 DST
    /// Used by: Germany, France, Italy, Spain, Poland, etc.
    pub const CET: Self = Self::new(3600);

    /// Eastern European Time (EET) - UTC+2 / UTC+3 DST
    /// Used by: Finland, Estonia, Latvia, Lithuania, Bulgaria, Greece, etc.
    pub const EET: Self = Self::new(2 * 3600);
}

/// Calculate if DST is active according to EU rules.
///
/// DST starts: Last Sunday of March at 01:00 UTC
/// DST ends: Last Sunday of October at 01:00 UTC
///
/// Reference: <https://valtioneuvosto.fi/hanke?tunnus=LVM070:00/2018>
fn is_dst_active(unix_secs: u64) -> bool {
    let (year, month, day, hour) = unix_to_date(unix_secs);

    // Get DST transition dates for this year
    let march_last_sunday = last_sunday_of_month(year, 3);
    let october_last_sunday = last_sunday_of_month(year, 10);

    // DST is active from last Sunday of March 01:00 UTC to last Sunday of October 01:00 UTC
    if month > 3 && month < 10 {
        // April through September: always DST
        true
    } else if month == 3 {
        // March: DST starts on last Sunday at 01:00 UTC
        day > march_last_sunday || (day == march_last_sunday && hour >= 1)
    } else if month == 10 {
        // October: DST ends on last Sunday at 01:00 UTC
        day < october_last_sunday || (day == october_last_sunday && hour < 1)
    } else {
        // November through February: no DST
        false
    }
}

/// Convert Unix timestamp to (year, month, day, hour).
fn unix_to_date(unix_secs: u64) -> (u32, u32, u32, u32) {
    // Days since Unix epoch (1970-01-01)
    let days = (unix_secs / 86400) as u32;
    let hour = ((unix_secs % 86400) / 3600) as u32;

    // Calculate year
    let mut year = 1970u32;
    let mut remaining_days = days;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    // Calculate month and day
    let leap = is_leap_year(year);
    let days_in_months: [u32; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u32;
    for days_in_month in days_in_months {
        if remaining_days < days_in_month {
            break;
        }
        remaining_days -= days_in_month;
        month += 1;
    }

    let day = remaining_days + 1; // Days are 1-indexed

    (year, month, day, hour)
}

/// Check if a year is a leap year.
fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Find the last Sunday of a given month (returns day of month).
fn last_sunday_of_month(year: u32, month: u32) -> u32 {
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    };

    // Calculate day of week for last day of month using Zeller's formula
    // 0 = Saturday, 1 = Sunday, ..., 6 = Friday
    let day = days_in_month;
    let m = if month < 3 { month + 12 } else { month };
    let y = if month < 3 { year - 1 } else { year };
    let k = y % 100;
    let j = y / 100;

    let h = (day + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    // Convert to: 0 = Sunday, 1 = Monday, ..., 6 = Saturday
    let dow = ((h + 6) % 7) as u32;

    // Find last Sunday
    if dow == 0 {
        days_in_month
    } else {
        days_in_month - dow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_winter_time() {
        // 2024-01-15 12:00:00 UTC - clearly winter
        let unix_secs = 1705320000;
        assert!(!is_dst_active(unix_secs));

        let tz = Timezone::EET;
        assert_eq!(tz.offset_secs(unix_secs), 2 * 3600);
    }

    #[test]
    fn test_summer_time() {
        // 2024-07-15 12:00:00 UTC - clearly summer
        let unix_secs = 1721044800;
        assert!(is_dst_active(unix_secs));

        let tz = Timezone::EET;
        assert_eq!(tz.offset_secs(unix_secs), 3 * 3600);
    }

    #[test]
    fn test_dst_transition_march() {
        // Last Sunday of March 2024 is March 31
        // At 00:59 UTC - still winter time
        let before_dst = 1711846740; // 2024-03-31 00:59:00 UTC
        assert!(!is_dst_active(before_dst));

        // At 01:00 UTC - DST starts
        let after_dst = 1711846800; // 2024-03-31 01:00:00 UTC
        assert!(is_dst_active(after_dst));
    }

    #[test]
    fn test_dst_transition_october() {
        // Last Sunday of October 2024 is October 27
        // At 00:59 UTC - still summer time
        let before_end = 1729990740; // 2024-10-27 00:59:00 UTC
        assert!(is_dst_active(before_end));

        // At 01:00 UTC - DST ends
        let after_end = 1729990800; // 2024-10-27 01:00:00 UTC
        assert!(!is_dst_active(after_end));
    }

    #[test]
    fn test_last_sunday_march_2024() {
        // March 2024: Last Sunday is March 31
        assert_eq!(last_sunday_of_month(2024, 3), 31);
    }

    #[test]
    fn test_last_sunday_october_2024() {
        // October 2024: Last Sunday is October 27
        assert_eq!(last_sunday_of_month(2024, 10), 27);
    }

    #[test]
    fn test_leap_year() {
        assert!(is_leap_year(2024)); // Divisible by 4
        assert!(!is_leap_year(2023)); // Not divisible by 4
        assert!(!is_leap_year(1900)); // Divisible by 100 but not 400
        assert!(is_leap_year(2000)); // Divisible by 400
    }

    #[test]
    fn test_to_local_hms() {
        let tz = Timezone::EET;
        // 2024-07-15 12:00:00 UTC -> 15:00:00 EEST (UTC+3)
        let unix_secs = 1721044800;
        assert_eq!(tz.to_local_hms(unix_secs), (15, 0, 0));
    }

    #[test]
    fn test_timezone_presets() {
        let unix_summer = 1721044800; // 2024-07-15 12:00:00 UTC

        assert_eq!(Timezone::WET.offset_secs(unix_summer), 3600); // UTC+1
        assert_eq!(Timezone::CET.offset_secs(unix_summer), 2 * 3600); // UTC+2
        assert_eq!(Timezone::EET.offset_secs(unix_summer), 3 * 3600); // UTC+3
    }
}
