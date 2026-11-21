//! Durations and points in time.
//!
//! * [`parse_duration`] reads human-friendly durations like `30m`, `3h`, `2d`,
//!   `1w` or combinations such as `1h30m` (used by `find --newer/--older`).
//! * [`format_age`] prints a compact age such as `5m` or `3d`.
//! * [`format_timestamp`] prints a modification time as `YYYY-MM-DD HH:MM` in
//!   UTC. UTC keeps output identical on every machine and needs no time-zone
//!   database (see `docs/decisions.md`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::DurationError;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;
/// A "year" is 365 days; durations here are approximate by nature.
const YEAR: u64 = 365 * DAY;

/// Parses a duration made of one or more `<number><unit>` parts.
///
/// Units (case-insensitive): `s`/`sec`, `m`/`min`, `h`/`hour`, `d`/`day`,
/// `w`/`week`, `y`/`year` (365 days), each with optional plural forms.
/// Whitespace between parts is allowed: `1h 30m`.
///
/// ```
/// use std::time::Duration;
/// use fsight::duration::parse_duration;
/// assert_eq!(parse_duration("2d"), Ok(Duration::from_secs(2 * 86_400)));
/// assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5_400)));
/// ```
///
/// # Errors
///
/// Returns a [`DurationError`] for empty input, a missing or unknown unit,
/// a non-integer number, or a total that does not fit in a `u64` of seconds.
pub fn parse_duration(input: &str) -> Result<Duration, DurationError> {
    let mut rest = input.trim();
    if rest.is_empty() {
        return Err(DurationError::Empty);
    }

    let mut total_seconds: u64 = 0;
    while !rest.is_empty() {
        let (seconds, remaining) = parse_part(rest, input)?;
        total_seconds = total_seconds
            .checked_add(seconds)
            .ok_or(DurationError::Overflow)?;
        rest = remaining.trim_start();
    }
    Ok(Duration::from_secs(total_seconds))
}

/// Parses one `<number><unit>` part from the start of `text`.
/// Returns the part's length in seconds and the text that follows it.
fn parse_part<'a>(text: &'a str, whole_input: &str) -> Result<(u64, &'a str), DurationError> {
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if digits_end == 0 {
        return Err(DurationError::InvalidNumber(whole_input.to_string()));
    }
    let (digits, after_digits) = text.split_at(digits_end);

    // The unit runs until the next digit (or the end of the input).
    let unit_end = after_digits
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(after_digits.len());
    let (unit, remaining) = after_digits.split_at(unit_end);
    let unit = unit.trim();

    if unit.is_empty() {
        return Err(DurationError::MissingUnit(whole_input.to_string()));
    }
    if unit.starts_with('.') {
        return Err(DurationError::InvalidNumber(whole_input.to_string()));
    }
    let unit_seconds =
        unit_seconds(unit).ok_or_else(|| DurationError::UnknownUnit(unit.to_string()))?;
    // `digits` holds only ASCII digits, so parsing can only fail by overflowing.
    let value: u64 = digits.parse().map_err(|_| DurationError::Overflow)?;
    let seconds = value
        .checked_mul(unit_seconds)
        .ok_or(DurationError::Overflow)?;
    Ok((seconds, remaining))
}

/// How many seconds one `unit` lasts, or `None` for an unknown unit.
fn unit_seconds(unit: &str) -> Option<u64> {
    let seconds = match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => MINUTE,
        "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
        "d" | "day" | "days" => DAY,
        "w" | "week" | "weeks" => WEEK,
        "y" | "year" | "years" => YEAR,
        _ => return None,
    };
    Some(seconds)
}

/// Formats a duration compactly using the largest fitting unit, rounded down:
/// `42s`, `5m`, `3h`, `6d`, `7w`, `2y`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < 4 * WEEK => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}w", s / WEEK),
        s => format!("{}y", s / YEAR),
    }
}

/// Seconds since the Unix epoch (negative for times before 1970).
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_secs()).map_or(i64::MIN, |s| -s),
    }
}

/// Formats a point in time as `YYYY-MM-DD HH:MM` (UTC).
pub fn format_timestamp(time: SystemTime) -> String {
    let seconds = unix_seconds(time);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        second_of_day / 3600,
        second_of_day % 3600 / 60
    )
}

/// Converts a count of days since 1970-01-01 into a (year, month, day) date
/// in the proleptic Gregorian calendar.
///
/// This is Howard Hinnant's well-known `civil_from_days` algorithm: it shifts
/// the calendar so years start in March (putting the leap day last), then
/// splits days into 400-year "eras", years, months and days.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097); // 146 097 days per 400 years
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153; // 0 = March … 11 = February
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn parses_each_unit() {
        assert_eq!(parse_duration("45s"), Ok(secs(45)));
        assert_eq!(parse_duration("30m"), Ok(secs(30 * MINUTE)));
        assert_eq!(parse_duration("3h"), Ok(secs(3 * HOUR)));
        assert_eq!(parse_duration("2d"), Ok(secs(2 * DAY)));
        assert_eq!(parse_duration("1w"), Ok(secs(WEEK)));
        assert_eq!(parse_duration("1y"), Ok(secs(YEAR)));
    }

    #[test]
    fn accepts_long_unit_names_any_case_and_whitespace() {
        assert_eq!(parse_duration("2 days"), Ok(secs(2 * DAY)));
        assert_eq!(parse_duration("1 Hour"), Ok(secs(HOUR)));
        assert_eq!(parse_duration("10MIN"), Ok(secs(10 * MINUTE)));
        assert_eq!(parse_duration("  3h  "), Ok(secs(3 * HOUR)));
    }

    #[test]
    fn combines_multiple_parts() {
        assert_eq!(parse_duration("1h30m"), Ok(secs(HOUR + 30 * MINUTE)));
        assert_eq!(parse_duration("1d 12h"), Ok(secs(DAY + 12 * HOUR)));
        assert_eq!(
            parse_duration("1w2d3h4m5s"),
            Ok(secs(WEEK + 2 * DAY + 3 * HOUR + 4 * MINUTE + 5))
        );
    }

    #[test]
    fn rejects_invalid_durations() {
        assert_eq!(parse_duration(""), Err(DurationError::Empty));
        assert_eq!(
            parse_duration("10"),
            Err(DurationError::MissingUnit("10".into()))
        );
        assert_eq!(
            parse_duration("1h30"),
            Err(DurationError::MissingUnit("1h30".into()))
        );
        assert_eq!(
            parse_duration("5x"),
            Err(DurationError::UnknownUnit("x".into()))
        );
        assert!(matches!(
            parse_duration("h"),
            Err(DurationError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_duration("-2d"),
            Err(DurationError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_duration("1.5h"),
            Err(DurationError::InvalidNumber(_))
        ));
        assert_eq!(
            parse_duration("99999999999999999999s"),
            Err(DurationError::Overflow)
        );
        assert_eq!(
            parse_duration("999999999999999y"),
            Err(DurationError::Overflow)
        );
    }

    #[test]
    fn formats_ages_with_the_largest_fitting_unit() {
        assert_eq!(format_age(secs(0)), "0s");
        assert_eq!(format_age(secs(59)), "59s");
        assert_eq!(format_age(secs(60)), "1m");
        assert_eq!(format_age(secs(3 * HOUR + 59 * MINUTE)), "3h");
        assert_eq!(format_age(secs(27 * DAY)), "27d");
        assert_eq!(format_age(secs(4 * WEEK)), "4w");
        assert_eq!(format_age(secs(2 * YEAR + 100 * DAY)), "2y");
    }

    #[test]
    fn formats_known_timestamps_in_utc() {
        let at = |s: u64| UNIX_EPOCH + secs(s);
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01 00:00");
        assert_eq!(format_timestamp(at(951_782_400)), "2000-02-29 00:00"); // leap day
        assert_eq!(format_timestamp(at(1_700_000_000)), "2023-11-14 22:13");
        assert_eq!(format_timestamp(at(4_107_542_400)), "2100-03-01 00:00"); // 2100 is not a leap year
    }

    #[test]
    fn handles_times_before_the_epoch() {
        let before = UNIX_EPOCH - secs(DAY + 60);
        assert_eq!(unix_seconds(before), -86_460);
        assert_eq!(format_timestamp(before), "1969-12-30 23:59");
    }

    proptest! {
        /// `format_age` rounds down to its unit, so parsing the text back gives
        /// a value no larger than the original and less than one unit smaller.
        #[test]
        fn format_age_round_trips_through_parse(n in 0u64..(1000 * YEAR)) {
            let text = format_age(secs(n));
            let parsed = parse_duration(&text).expect("formatted ages must parse").as_secs();
            prop_assert!(parsed <= n);
            let unit = unit_seconds(text.trim_start_matches(|c: char| c.is_ascii_digit())).unwrap();
            prop_assert!(n - parsed < unit, "{n} -> {text}");
        }

        #[test]
        fn parsing_arbitrary_text_never_panics(text in "\\PC{0,20}") {
            let _ = parse_duration(&text);
        }

        /// Every day maps to a valid calendar date, and consecutive days never go backwards.
        #[test]
        fn civil_dates_are_valid_and_ordered(days in -1_000_000i64..1_000_000) {
            let (y, m, d) = civil_from_days(days);
            prop_assert!((1..=12).contains(&m));
            prop_assert!((1..=31).contains(&d));
            let next = civil_from_days(days + 1);
            prop_assert!(next > (y, m, d));
        }
    }
}
