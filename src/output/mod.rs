//! Rendering of command results.
//!
//! Every command produces a plain data structure (a "report") that derives
//! [`serde::Serialize`]. This module turns a report into either JSON or
//! human-readable text. Keeping computation and presentation apart means the
//! command logic can be tested without parsing text, and `--json` comes for
//! free.

pub mod bars;
pub mod color;
pub mod tree;

use std::io::{self, Write};

use serde::Serialize;

use self::color::Palette;

/// How results are written to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable text (possibly colored).
    Text,
    /// Pretty-printed JSON.
    Json,
    /// JSON on a single line.
    JsonCompact,
}

impl Format {
    /// Picks the format from the `--json` and `--compact` flags
    /// (`--compact` implies `--json`).
    pub fn from_flags(json: bool, compact: bool) -> Self {
        match (json, compact) {
            (_, true) => Format::JsonCompact,
            (true, false) => Format::Json,
            (false, false) => Format::Text,
        }
    }

    /// Is this one of the JSON formats?
    pub fn is_json(self) -> bool {
        self != Format::Text
    }
}

/// Something that can be rendered as human-readable text.
///
/// Implemented by every report type. Taking `&mut dyn Write` (instead of
/// printing directly) lets tests render into a `Vec<u8>`.
pub trait RenderText {
    /// Writes the text rendering of `self` to `out`.
    ///
    /// # Errors
    ///
    /// Returns any error from writing to `out`.
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()>;
}

/// Writes `report` to `out` in the requested format.
///
/// # Errors
///
/// Returns any error from writing to `out` (for example a broken pipe).
pub fn emit<T>(out: &mut dyn Write, report: &T, format: Format, palette: Palette) -> io::Result<()>
where
    T: Serialize + RenderText,
{
    match format {
        Format::Text => report.render_text(out, palette),
        Format::Json => write_json(out, report, true),
        Format::JsonCompact => write_json(out, report, false),
    }
}

/// Serializes `value` as JSON followed by a newline.
///
/// # Errors
///
/// Returns any error from writing to `out`.
pub fn write_json<T: Serialize + ?Sized>(
    out: &mut dyn Write,
    value: &T,
    pretty: bool,
) -> io::Result<()> {
    if pretty {
        serde_json::to_writer_pretty(&mut *out, value)?;
    } else {
        serde_json::to_writer(&mut *out, value)?;
    }
    writeln!(out)
}

/// Formats a count with thousands separators: `1234567` → `1,234,567`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// "1 file" / "2 files": picks the singular or plural form of a noun.
pub fn plural<'a>(count: u64, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_grouped_in_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn compact_implies_json() {
        assert_eq!(Format::from_flags(false, false), Format::Text);
        assert_eq!(Format::from_flags(true, false), Format::Json);
        assert_eq!(Format::from_flags(false, true), Format::JsonCompact);
        assert_eq!(Format::from_flags(true, true), Format::JsonCompact);
    }

    #[test]
    fn json_output_ends_with_a_newline_and_parses_back() {
        let mut out = Vec::new();
        write_json(&mut out, &serde_json::json!({"a": [1, 2]}), false).unwrap();
        assert_eq!(out, b"{\"a\":[1,2]}\n");
        let mut pretty = Vec::new();
        write_json(&mut pretty, &serde_json::json!({"a": 1}), true).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&pretty).unwrap();
        assert_eq!(parsed["a"], 1);
    }
}
