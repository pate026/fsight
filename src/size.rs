//! Parsing and formatting of byte sizes such as `200B`, `10K`, `1.5G` or `12.4 GiB`.
//!
//! All units are *binary*: `1K` = 1024 bytes, `1M` = 1024 × 1024 bytes, and so on.
//! The arithmetic is done with integers (`u128` for headroom), which keeps the
//! results exact and avoids floating-point rounding surprises.

use crate::error::SizeError;

/// Unit suffixes used when formatting, indexed by the power of 1024.
const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// The largest power of 1024 that fits in a `u64` (1 EiB = 1024⁶ bytes).
const MAX_EXPONENT: usize = 6;

/// More fractional digits than this cannot change the result in a meaningful way.
const MAX_FRACTION_DIGITS: usize = 9;

/// Parses a human-readable size into a number of bytes.
///
/// Accepted forms: a number (integer or decimal) followed by an optional unit,
/// with optional whitespace in between. Units are case-insensitive, and `K`,
/// `KB` and `KiB` all mean 1024 bytes. A bare number is a byte count.
/// Values larger than `u64::MAX` saturate to `u64::MAX`.
///
/// ```
/// use fsight::size::parse_size;
/// assert_eq!(parse_size("200B"), Ok(200));
/// assert_eq!(parse_size("10K"), Ok(10 * 1024));
/// assert_eq!(parse_size("1.5G"), Ok(1_610_612_736));
/// assert_eq!(parse_size("12.4 GiB"), Ok(13_314_398_618));
/// ```
///
/// # Errors
///
/// Returns a [`SizeError`] if the input is empty, the number is malformed
/// (e.g. `-5M` or `1.2.3K`) or the unit is unknown.
pub fn parse_size(input: &str) -> Result<u64, SizeError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(SizeError::Empty);
    }

    // Split "1.5G" into the number part "1.5" and the unit part "G".
    let unit_start = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(unit_start);

    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if (whole.is_empty() && fraction.is_empty()) || fraction.contains('.') {
        return Err(SizeError::InvalidNumber(input.to_string()));
    }
    let multiplier = unit_multiplier(unit.trim_start())?;

    // Both parts contain only ASCII digits here, so byte slicing is safe.
    let whole_value = digits_to_u128(whole);
    let fraction = &fraction[..fraction.len().min(MAX_FRACTION_DIGITS)];
    let (fraction_value, scale) = fraction
        .bytes()
        .fold((0u128, 1u128), |(value, scale), digit| {
            (value * 10 + u128::from(digit - b'0'), scale * 10)
        });

    // bytes = whole * multiplier + round(fraction * multiplier)
    let fraction_bytes = (fraction_value * multiplier + scale / 2) / scale;
    let bytes = whole_value
        .saturating_mul(multiplier)
        .saturating_add(fraction_bytes);
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
}

/// Converts a string of ASCII digits to a number, saturating instead of overflowing.
fn digits_to_u128(digits: &str) -> u128 {
    digits.bytes().fold(0u128, |acc, digit| {
        acc.saturating_mul(10)
            .saturating_add(u128::from(digit - b'0'))
    })
}

/// Maps a unit suffix to its multiplier in bytes.
fn unit_multiplier(unit: &str) -> Result<u128, SizeError> {
    let exponent = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 1,
        "M" | "MB" | "MIB" => 2,
        "G" | "GB" | "GIB" => 3,
        "T" | "TB" | "TIB" => 4,
        "P" | "PB" | "PIB" => 5,
        "E" | "EB" | "EIB" => 6,
        _ => return Err(SizeError::UnknownUnit(unit.to_string())),
    };
    Ok(1u128 << (10 * exponent))
}

/// Formats a byte count for humans, e.g. `512 B`, `1.0 KiB`, `12.4 GiB`.
///
/// Values below 1 KiB are shown exactly; larger values use one decimal in the
/// largest unit that keeps the number at least 1.
///
/// ```
/// use fsight::size::format_size;
/// assert_eq!(format_size(512), "512 B");
/// assert_eq!(format_size(1536), "1.5 KiB");
/// assert_eq!(format_size(u64::MAX), "16.0 EiB");
/// ```
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    // Find the largest unit that is <= bytes.
    let mut exponent = 1;
    while exponent < MAX_EXPONENT && bytes >= 1u64 << (10 * (exponent + 1)) {
        exponent += 1;
    }

    let mut tenths = rounded_tenths(bytes, exponent);
    // Rounding can turn 1023.96 KiB into "1024.0 KiB"; show "1.0 MiB" instead.
    if tenths >= 10_240 && exponent < MAX_EXPONENT {
        exponent += 1;
        tenths = rounded_tenths(bytes, exponent);
    }
    format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[exponent])
}

/// `bytes / 1024^exponent`, expressed in tenths and rounded to the nearest tenth.
fn rounded_tenths(bytes: u64, exponent: usize) -> u128 {
    let unit = 1u128 << (10 * exponent);
    (u128::from(bytes) * 10 + unit / 2) / unit
}

/// A size comparison used by `find --size`: `+1M`, `-10K` or `4K`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeFilter {
    /// `+SIZE`: at least this many bytes.
    AtLeast(u64),
    /// `-SIZE`: at most this many bytes.
    AtMost(u64),
    /// `SIZE` without a sign: exactly this many bytes.
    Exactly(u64),
}

impl SizeFilter {
    /// Returns `true` if a file of `size` bytes passes this filter.
    pub fn matches(self, size: u64) -> bool {
        match self {
            SizeFilter::AtLeast(limit) => size >= limit,
            SizeFilter::AtMost(limit) => size <= limit,
            SizeFilter::Exactly(limit) => size == limit,
        }
    }
}

/// Parses `+SIZE`, `-SIZE` or `SIZE` into a [`SizeFilter`].
///
/// # Errors
///
/// Returns a [`SizeError`] if the size after the optional sign is invalid.
pub fn parse_size_filter(input: &str) -> Result<SizeFilter, SizeError> {
    let text = input.trim();
    if let Some(rest) = text.strip_prefix('+') {
        parse_size(rest).map(SizeFilter::AtLeast)
    } else if let Some(rest) = text.strip_prefix('-') {
        parse_size(rest).map(SizeFilter::AtMost)
    } else {
        parse_size(text).map(SizeFilter::Exactly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    #[test]
    fn parses_plain_numbers_and_every_unit() {
        assert_eq!(parse_size("0"), Ok(0));
        assert_eq!(parse_size("200"), Ok(200));
        assert_eq!(parse_size("200B"), Ok(200));
        assert_eq!(parse_size("10K"), Ok(10 * KIB));
        assert_eq!(parse_size("5M"), Ok(5 * MIB));
        assert_eq!(parse_size("2G"), Ok(2 * GIB));
        assert_eq!(parse_size("1T"), Ok(1 << 40));
        assert_eq!(parse_size("1P"), Ok(1 << 50));
        assert_eq!(parse_size("1E"), Ok(1 << 60));
    }

    #[test]
    fn units_are_case_insensitive_and_accept_suffix_variants() {
        for text in [
            "10k", "10K", "10kb", "10KB", "10KiB", "10kib", "10 K", " 10K ",
        ] {
            assert_eq!(parse_size(text), Ok(10 * KIB), "input {text:?}");
        }
    }

    #[test]
    fn parses_decimal_values_with_rounding() {
        assert_eq!(parse_size("1.5G"), Ok(GIB + GIB / 2));
        assert_eq!(parse_size(".5K"), Ok(512));
        assert_eq!(parse_size("0.1K"), Ok(102)); // 102.4 rounds down
        assert_eq!(parse_size("0.3K"), Ok(307)); // 307.2 rounds down
        assert_eq!(parse_size("1.0005K"), Ok(1025)); // 1024.512 rounds up
        assert_eq!(parse_size("12.4 GiB"), Ok(13_314_398_618));
    }

    #[test]
    fn rejects_malformed_sizes() {
        assert_eq!(parse_size(""), Err(SizeError::Empty));
        assert_eq!(parse_size("   "), Err(SizeError::Empty));
        assert!(matches!(
            parse_size("-5M"),
            Err(SizeError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_size("1.2.3K"),
            Err(SizeError::InvalidNumber(_))
        ));
        assert!(matches!(parse_size("."), Err(SizeError::InvalidNumber(_))));
        assert!(matches!(parse_size("K"), Err(SizeError::InvalidNumber(_))));
        assert_eq!(parse_size("10X"), Err(SizeError::UnknownUnit("X".into())));
        assert_eq!(
            parse_size("10 KK"),
            Err(SizeError::UnknownUnit("KK".into()))
        );
    }

    #[test]
    fn huge_values_saturate_instead_of_overflowing() {
        assert_eq!(parse_size("16E"), Ok(u64::MAX));
        assert_eq!(
            parse_size("99999999999999999999999999999999999999999"),
            Ok(u64::MAX)
        );
    }

    #[test]
    fn formats_small_values_exactly() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn formats_larger_values_with_one_decimal() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(10 * MIB), "10.0 MiB");
        assert_eq!(format_size(13_314_398_618), "12.4 GiB");
        assert_eq!(format_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn rounding_up_moves_to_the_next_unit() {
        // 1023.96 KiB would print as "1024.0 KiB" without the unit bump.
        assert_eq!(format_size(MIB - 40), "1.0 MiB");
        assert_eq!(format_size(MIB - 52), "1023.9 KiB");
    }

    #[test]
    fn size_filters_compare_inclusively() {
        assert_eq!(parse_size_filter("+1K"), Ok(SizeFilter::AtLeast(KIB)));
        assert_eq!(parse_size_filter("-1K"), Ok(SizeFilter::AtMost(KIB)));
        assert_eq!(parse_size_filter("1K"), Ok(SizeFilter::Exactly(KIB)));
        assert!(SizeFilter::AtLeast(KIB).matches(KIB));
        assert!(!SizeFilter::AtLeast(KIB).matches(KIB - 1));
        assert!(SizeFilter::AtMost(KIB).matches(KIB));
        assert!(!SizeFilter::AtMost(KIB).matches(KIB + 1));
        assert!(SizeFilter::Exactly(KIB).matches(KIB));
        assert!(!SizeFilter::Exactly(KIB).matches(KIB + 1));
        assert!(parse_size_filter("+").is_err());
    }

    proptest! {
        /// Formatting loses precision (one decimal), but parsing the formatted
        /// text must land within half a display step of the original value.
        #[test]
        fn parse_of_format_round_trips_within_rounding(n in any::<u64>()) {
            let text = format_size(n);
            let parsed = parse_size(&text).expect("formatted sizes must parse");
            let tolerance = n / 20 + 1; // one decimal place in a unit <= n is at most 5% of n
            prop_assert!(parsed.abs_diff(n) <= tolerance, "{n} -> {text} -> {parsed}");
        }

        #[test]
        fn small_values_round_trip_exactly(n in 0u64..1024) {
            prop_assert_eq!(parse_size(&format_size(n)), Ok(n));
        }

        #[test]
        fn formatting_never_panics(n in any::<u64>()) {
            let text = format_size(n);
            prop_assert!(!text.is_empty());
        }

        #[test]
        fn parsing_arbitrary_text_never_panics(text in "\\PC{0,20}") {
            let _ = parse_size(&text);
        }
    }
}
