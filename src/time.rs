//! Reading times typed by a person: `2w`, `3 days ago`, `yesterday`, `2025-12-01`.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};

/// Parse a human-friendly time into a point in time.
///
/// - `today`, `yesterday`: start of that day, local time
/// - `30m`, `12h`, `3d`, `2w`, `6mo`, `1y`: that long ago
/// - `3 days ago`, `1 wk ago`, `2 months ago`
/// - `2025-12-01` (start of that day, local time), or RFC 3339
pub fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim().to_lowercase();

    if s == "today" {
        return Ok(start_of_local_day(Local::now().date_naive()));
    }
    if s == "yesterday" {
        return Ok(start_of_local_day(Local::now().date_naive() - Duration::days(1)));
    }

    // "2w", "3d", "6mo"
    if let Some(duration) = parse_compact_duration(&s) {
        return Ok(Utc::now() - duration);
    }

    // "N unit ago"
    if s.ends_with(" ago") {
        let parts: Vec<&str> = s.trim_end_matches(" ago").split_whitespace().collect();
        if parts.len() == 2 {
            let n: i64 = parts[0].parse().map_err(|_| {
                anyhow::anyhow!("Invalid time format: {}. Try '2w', '1 week ago' or '2025-12-01'", s)
            })?;
            let unit = parts[1].trim_end_matches('s'); // "weeks" -> "week"
            let duration = unit_duration(unit, n).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown time unit: {}. Use minutes, hours, days, weeks, months, years",
                    unit
                )
            })?;
            return Ok(Utc::now() - duration);
        }
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Ok(dt.with_timezone(&Utc));
    }

    if let Ok(date) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return Ok(start_of_local_day(date));
    }

    Err(anyhow::anyhow!(
        "Invalid time format: {}. Try '2w', '1 week ago', 'yesterday', or '2025-12-01'",
        s
    ))
}

/// A search box query split into its search text and its date words
#[derive(Debug, Default, PartialEq)]
pub struct QueryDates {
    /// The query without the date words
    pub text: String,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

const SINCE_PREFIXES: &[&str] = &["since:", "after:"];
const UNTIL_PREFIXES: &[&str] = &["until:", "before:"];

/// Take `since:2w`, `after:2025-12-01`, `until:yesterday`, `before:3d` out of a
/// search query. A date word with a value that can't be read is dropped from
/// the text and ignored.
pub fn split_query_dates(query: &str) -> QueryDates {
    let mut dates = QueryDates::default();
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        let lower = word.to_lowercase();
        let since = SINCE_PREFIXES.iter().find(|p| lower.starts_with(*p));
        let until = UNTIL_PREFIXES.iter().find(|p| lower.starts_with(*p));
        match (since, until) {
            (Some(prefix), _) => dates.since = parse_time(&word[prefix.len()..]).ok().or(dates.since),
            (_, Some(prefix)) => dates.until = parse_time(&word[prefix.len()..]).ok().or(dates.until),
            _ => words.push(word),
        }
    }
    dates.text = words.join(" ");
    dates
}

/// `<number><unit>` with no space, e.g. `2w`
fn parse_compact_duration(s: &str) -> Option<Duration> {
    let digits_end = s.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let n: i64 = s[..digits_end].parse().ok()?;
    unit_duration(&s[digits_end..], n)
}

fn unit_duration(unit: &str, n: i64) -> Option<Duration> {
    Some(match unit {
        "m" | "min" | "minute" => Duration::minutes(n),
        "h" | "hr" | "hour" => Duration::hours(n),
        "d" | "day" => Duration::days(n),
        "w" | "wk" | "week" => Duration::weeks(n),
        "mo" | "month" => Duration::days(n * 30), // Approximate
        "y" | "yr" | "year" => Duration::days(n * 365),
        _ => return None,
    })
}

fn start_of_local_day(date: NaiveDate) -> DateTime<Utc> {
    let midnight = date.and_hms_opt(0, 0, 0).unwrap();
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn test_parse_time_today_is_start_of_local_day() {
        let result = parse_time("today").unwrap().with_timezone(&Local);
        assert_eq!(result.date_naive(), Local::now().date_naive());
        assert_eq!((result.hour(), result.minute()), (0, 0));
    }

    #[test]
    fn test_parse_time_yesterday_is_start_of_previous_day() {
        let result = parse_time("yesterday").unwrap().with_timezone(&Local);
        assert_eq!(result.date_naive(), Local::now().date_naive() - Duration::days(1));
        assert_eq!((result.hour(), result.minute()), (0, 0));
    }

    #[test]
    fn test_parse_time_compact() {
        let close = |text: &str, expected: DateTime<Utc>| {
            let result = parse_time(text).unwrap();
            assert!((result - expected).num_seconds().abs() < 2, "{}", text);
        };
        close("30m", Utc::now() - Duration::minutes(30));
        close("12h", Utc::now() - Duration::hours(12));
        close("3d", Utc::now() - Duration::days(3));
        close("2w", Utc::now() - Duration::weeks(2));
        close("6mo", Utc::now() - Duration::days(180));
        close("1y", Utc::now() - Duration::days(365));
        assert!(parse_time("2x").is_err());
        assert!(parse_time("w").is_err());
    }

    #[test]
    fn test_parse_time_relative_days() {
        let result = parse_time("3 days ago").unwrap();
        let expected = Utc::now() - Duration::days(3);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_time_relative_weeks() {
        let result = parse_time("2 weeks ago").unwrap();
        let expected = Utc::now() - Duration::weeks(2);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_time_relative_hours() {
        let result = parse_time("5 hours ago").unwrap();
        let expected = Utc::now() - Duration::hours(5);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_time_relative_minutes() {
        let result = parse_time("30 minutes ago").unwrap();
        let expected = Utc::now() - Duration::minutes(30);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_time_relative_months() {
        let result = parse_time("2 months ago").unwrap();
        let expected = Utc::now() - Duration::days(60); // 2 * 30
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_time_short_units() {
        // Test abbreviated units
        assert!(parse_time("1 hr ago").is_ok());
        assert!(parse_time("5 min ago").is_ok());
        assert!(parse_time("1 wk ago").is_ok());
        assert!(parse_time("1 mo ago").is_ok());
    }

    #[test]
    fn test_parse_time_date() {
        let result = parse_time("2025-12-01").unwrap().with_timezone(&Local);
        assert_eq!(result.year(), 2025);
        assert_eq!(result.month(), 12);
        assert_eq!(result.day(), 1);
    }

    #[test]
    fn test_parse_time_iso8601() {
        let result = parse_time("2025-12-01T14:30:00Z").unwrap();
        assert_eq!(result.year(), 2025);
        assert_eq!(result.month(), 12);
        assert_eq!(result.day(), 1);
        assert_eq!(result.hour(), 14);
        assert_eq!(result.minute(), 30);
    }

    #[test]
    fn test_parse_time_case_insensitive() {
        assert!(parse_time("YESTERDAY").is_ok());
        assert!(parse_time("Today").is_ok());
        assert!(parse_time("3 DAYS AGO").is_ok());
    }

    #[test]
    fn test_parse_time_whitespace() {
        assert!(parse_time("  yesterday  ").is_ok());
        assert!(parse_time("\tyesterday\n").is_ok());
    }

    #[test]
    fn test_parse_time_invalid() {
        assert!(parse_time("invalid").is_err());
        assert!(parse_time("a week ago").is_err()); // "a" is not a number
        assert!(parse_time("5 fortnights ago").is_err()); // unknown unit
    }

    #[test]
    fn test_split_query_dates() {
        let dates = split_query_dates("deploy since:2w before:yesterday bug");
        assert_eq!(dates.text, "deploy bug");
        assert!(dates.since.is_some());
        assert_eq!(dates.until, Some(parse_time("yesterday").unwrap()));
    }

    #[test]
    fn test_split_query_dates_ignores_unreadable_value() {
        let dates = split_query_dates("deploy since:");
        assert_eq!(dates.text, "deploy");
        assert_eq!(dates.since, None);
    }

    #[test]
    fn test_split_query_dates_leaves_other_colons() {
        let dates = split_query_dates("https://example.com after:2025-12-01");
        assert_eq!(dates.text, "https://example.com");
        assert!(dates.since.is_some());
    }
}
