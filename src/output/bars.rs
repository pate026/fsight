//! Text bars: the `██████░░░░` meters in the scan tree and the smooth
//! horizontal bars of the `stats` histograms.
//!
//! All arithmetic is done in integers (`u128` so `u64` values can be
//! multiplied without overflow), which keeps the output exact and identical
//! on every machine.

/// Characters for 1/8 … 8/8 of a cell, used for smooth histogram bars.
const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// `part / whole` as a whole percentage, rounded down (0 when `whole` is 0).
pub fn percent(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let value = u128::from(part) * 100 / u128::from(whole);
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// A fixed-width meter such as `██████░░░░` showing `part / whole`.
///
/// Filled cells are rounded down, so a bar is only full at 100%.
pub fn meter(part: u64, whole: u64, width: usize) -> String {
    let filled = if whole == 0 {
        0
    } else {
        let cells = u128::from(part.min(whole)) * width as u128 / u128::from(whole);
        usize::try_from(cells).unwrap_or(width)
    };
    let mut bar = "█".repeat(filled);
    bar.push_str(&"░".repeat(width - filled));
    bar
}

/// A smooth bar for histograms, scaled so that `max` fills `width` cells.
///
/// Uses eighth-blocks for sub-cell precision, and always shows at least a
/// thin sliver for non-zero values so small buckets remain visible.
pub fn histogram_bar(value: u64, max: u64, width: usize) -> String {
    if value == 0 || max == 0 || width == 0 {
        return String::new();
    }
    let eighths_total = u128::from(value.min(max)) * (width as u128) * 8 / u128::from(max);
    let eighths_total = usize::try_from(eighths_total).unwrap_or(width * 8).max(1);
    let full = eighths_total / 8;
    let remainder = eighths_total % 8;

    let mut bar = EIGHTHS[7].to_string().repeat(full);
    if remainder > 0 {
        bar.push(EIGHTHS[remainder - 1]);
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_round_down_and_handle_zero() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(5, 0), 0);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 66);
        assert_eq!(percent(3, 3), 100);
        assert_eq!(percent(u64::MAX, u64::MAX), 100);
    }

    #[test]
    fn meter_matches_the_documented_examples() {
        assert_eq!(meter(100, 100, 10), "██████████");
        assert_eq!(meter(65, 100, 10), "██████░░░░");
        assert_eq!(meter(24, 100, 10), "██░░░░░░░░");
        assert_eq!(meter(0, 100, 10), "░░░░░░░░░░");
        assert_eq!(meter(5, 0, 4), "░░░░");
        assert_eq!(meter(u64::MAX, u64::MAX, 3), "███");
    }

    #[test]
    fn histogram_bars_use_eighths_and_never_hide_small_values() {
        assert_eq!(histogram_bar(8, 8, 2), "██");
        assert_eq!(histogram_bar(4, 8, 2), "█");
        assert_eq!(histogram_bar(5, 8, 2), "█▎"); // 1.25 cells
        assert_eq!(histogram_bar(1, 1000, 10), "▏"); // tiny but visible
        assert_eq!(histogram_bar(0, 8, 2), "");
        assert_eq!(histogram_bar(3, 0, 2), "");
    }
}
