// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! ISO 8601 datetime parsing utilities.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// Parse an ISO 8601 datetime string into `DateTime<Utc>`.
///
/// Handles common variants found in CGMES, IEC 62325, and other power system data:
/// - `2024-01-15T00:00:00Z`
/// - `2024-01-15T00:00Z`
/// - `2024-01-15T00:00:00+00:00`
/// - `2024-01-15T00:00:00.123Z`
/// - `2024-01-15` (date only → midnight UTC)
pub fn parse_iso8601(s: &str) -> Option<DateTime<Utc>> {
    // Try RFC 3339 / full datetime with timezone
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.to_utc());
    }
    // Try datetime with Z but missing seconds (e.g. "2024-01-15T00:00Z")
    if s.ends_with('Z')
        && let Ok(dt) = DateTime::parse_from_rfc3339(&format!("{}:00Z", &s[..s.len() - 1]))
    {
        return Some(dt.to_utc());
    }
    // Try NaiveDateTime (no timezone) → assume UTC
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(ndt.and_utc());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return Some(ndt.and_utc());
    }
    // Try date-only → midnight UTC
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return nd.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc3339_z() {
        let dt = parse_iso8601("2024-01-15T08:30:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T08:30:00+00:00");
    }

    #[test]
    fn test_rfc3339_offset() {
        let dt = parse_iso8601("2024-01-15T08:30:00+05:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T03:30:00+00:00");
    }

    #[test]
    fn test_short_z() {
        let dt = parse_iso8601("2024-01-15T00:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T00:00:00+00:00");
    }

    #[test]
    fn test_date_only() {
        let dt = parse_iso8601("2024-03-15").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-03-15T00:00:00+00:00");
    }

    #[test]
    fn test_naive_datetime() {
        let dt = parse_iso8601("2024-01-15T14:30:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T14:30:00+00:00");
    }

    #[test]
    fn test_invalid() {
        assert!(parse_iso8601("not-a-date").is_none());
        assert!(parse_iso8601("").is_none());
    }
}
