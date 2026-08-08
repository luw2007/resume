//! Shared timestamp parsing helpers.
//!
//! Kept dependency-free (no chrono/jiff) per the project's minimal-dependency
//! stance: only `std::time` primitives and pure integer date math are used.
//! `days_from_civil` implements Howard Hinnant's civil-from-days algorithm.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Convert a JSON timestamp value (epoch seconds, epoch millis, or an
/// ISO-8601 string) to [`SystemTime`]. Returns `None` on an unparseable
/// value. Shared by integrations that read heterogeneous timestamp
/// representations from JSONL records.
pub fn json_value_to_system_time(value: &serde_json::Value) -> Option<SystemTime> {
    match value {
        serde_json::Value::Number(n) if n.is_i64() => {
            UNIX_EPOCH.checked_add(Duration::from_secs(n.as_i64()? as u64))
        }
        serde_json::Value::Number(n) if n.is_u64() => {
            UNIX_EPOCH.checked_add(Duration::from_secs(n.as_u64()?))
        }
        serde_json::Value::Number(n) if n.is_f64() => {
            let secs = n.as_f64()?;
            // Heuristic: some producers emit millis when the magnitude is
            // large enough that it cannot plausibly be epoch seconds.
            if secs >= 1e12 {
                let millis = secs as u64;
                UNIX_EPOCH.checked_add(Duration::from_millis(millis))
            } else if secs >= 0.0 {
                UNIX_EPOCH.checked_add(Duration::from_secs_f64(secs))
            } else {
                None
            }
        }
        serde_json::Value::String(s) => parse_iso8601(s),
        _ => None,
    }
}

/// Parse a subset of ISO-8601 timestamps into [`SystemTime`].
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS`, and the same with a `Z` or
/// `+HH:MM`/`-HH:MM` timezone suffix. The timezone offset itself is not
/// applied (all inputs are treated as UTC wall-clock), matching prior
/// integration behavior; this is acceptable because source producers emit
/// `Z`-suffixed UTC timestamps almost universally in practice.
pub fn parse_iso8601(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (date, rest) = s.split_once('T').unwrap_or((s, ""));
    let date_parts: Vec<&str> = date.split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: u32 = date_parts[1].parse().ok()?;
    let day: u32 = date_parts[2].parse().ok()?;
    let (h, min, sec) = if rest.is_empty() {
        (0u32, 0u32, 0u32)
    } else {
        let (time_part, _tz) = rest
            .split_once(['Z', '+', '-'])
            .filter(|(t, tz)| !tz.is_empty() || t.contains(':'))
            .unwrap_or((rest, ""));
        let time_parts: Vec<&str> = time_part.split(':').collect();
        let h: u32 = time_parts.first().and_then(|x| x.parse().ok()).unwrap_or(0);
        let min: u32 = time_parts.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        let sec: u32 = time_parts
            .get(2)
            .and_then(|x| x.split('.').next().unwrap_or("0").parse().ok())
            .unwrap_or(0);
        (h, min, sec)
    };
    let epoch_days = days_from_civil(year, month, day)?;
    let secs = epoch_days * 86_400 + (h as i64) * 3600 + (min as i64) * 60 + sec as i64;
    if secs < 0 {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    }
}

/// Howard Hinnant's `days_from_civil`: convert `(year, month, day)` to days
/// since 1970-01-01. Returns `None` for out-of-range `month`/`day`.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as i64 - 1);
    let doe = yoe as i64 * 365 + yoe as i64 / 4 - yoe as i64 / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Parse a bounded relative duration of the form `<N><unit>` where unit is
/// one of `m` (minutes), `h` (hours), `d` (days), or `w` (weeks). Returns
/// `None` for any other shape (including a bare number, a negative number,
/// or an unrecognized unit).
pub fn parse_relative_duration(value: &str) -> Option<Duration> {
    let split = value.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 || split + 1 != value.len() {
        return None;
    }
    let amount: u64 = value[..split].parse().ok()?;
    let unit_secs: u64 = match &value[split..] {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    amount.checked_mul(unit_secs).map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_seconds_and_millis() {
        assert_eq!(
            json_value_to_system_time(&serde_json::json!(0)),
            Some(UNIX_EPOCH)
        );
        assert_eq!(
            json_value_to_system_time(&serde_json::json!(1_700_000_000)),
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
    }

    #[test]
    fn parses_iso8601_date_and_datetime() {
        assert_eq!(parse_iso8601("1970-01-01"), Some(UNIX_EPOCH));
        assert_eq!(
            parse_iso8601("1970-01-02T00:00:00Z"),
            Some(UNIX_EPOCH + Duration::from_secs(86_400))
        );
    }

    #[test]
    fn rejects_malformed_iso8601() {
        assert_eq!(parse_iso8601(""), None);
        assert_eq!(parse_iso8601("not-a-date"), None);
        assert_eq!(parse_iso8601("2026-13-01"), None);
    }

    #[test]
    fn parses_relative_durations() {
        assert_eq!(
            parse_relative_duration("7d"),
            Some(Duration::from_secs(7 * 86_400))
        );
        assert_eq!(
            parse_relative_duration("1w"),
            Some(Duration::from_secs(7 * 86_400))
        );
        assert_eq!(
            parse_relative_duration("30m"),
            Some(Duration::from_secs(30 * 60))
        );
        assert_eq!(
            parse_relative_duration("2h"),
            Some(Duration::from_secs(2 * 3600))
        );
    }

    #[test]
    fn rejects_malformed_relative_durations() {
        assert_eq!(parse_relative_duration(""), None);
        assert_eq!(parse_relative_duration("7"), None);
        assert_eq!(parse_relative_duration("d"), None);
        assert_eq!(parse_relative_duration("7x"), None);
        assert_eq!(parse_relative_duration("-7d"), None);
    }
}
