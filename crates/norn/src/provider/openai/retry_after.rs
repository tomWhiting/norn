//! `Retry-After` header parsing.
//!
//! RFC 9110 §10.2.3 allows two forms: a non-negative decimal integer of
//! delta-seconds, or an HTTP-date. Both are handled here so a `429`
//! carrying either form imposes the server-requested backoff.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// Parses a `Retry-After` header value into a wait duration.
///
/// Accepts delta-seconds (`"120"`) and HTTP-dates
/// (`"Wed, 21 Oct 2015 07:28:00 GMT"`, parsed as RFC 2822). An
/// HTTP-date in the past yields [`Duration::ZERO`]. Returns `None` for
/// values that match neither form.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = DateTime::parse_from_rfc2822(trimmed).ok()?;
    let delta = date.with_timezone(&Utc) - Utc::now();
    Some(delta.to_std().unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delta_seconds() {
        assert_eq!(parse_retry_after("120"), Some(Duration::from_mins(2)));
        assert_eq!(parse_retry_after(" 5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
    }

    #[test]
    fn parses_future_http_date() {
        let future = Utc::now() + chrono::Duration::seconds(90);
        let header = future.to_rfc2822();
        let parsed = parse_retry_after(&header);
        assert!(
            matches!(
                parsed,
                Some(d) if d > Duration::from_secs(80) && d <= Duration::from_secs(91)
            ),
            "expected ~90s wait from {header:?}, got {parsed:?}"
        );
    }

    #[test]
    fn parses_gmt_http_date_in_past_as_zero() {
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn rejects_garbage_and_negative_values() {
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after("-3"), None);
        assert_eq!(parse_retry_after(""), None);
    }
}
