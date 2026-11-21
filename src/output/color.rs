//! Color policy and the palette used by all text renderers.
//!
//! Whether colors are used is decided once, at startup, from (highest
//! priority first) `--color`/`--no-color`, `FSIGHT_COLOR`, `NO_COLOR`, the
//! config file and finally the default `auto`, which colors only when stdout
//! is a terminal. Renderers never decide this themselves: they receive a
//! [`Palette`] that is either enabled or a no-op.

use std::fmt;

use clap::ValueEnum;
use owo_colors::Style;
use serde::{Deserialize, Serialize};

/// When to use colors (`--color <WHEN>` and `color = "..."` in the config).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ColorChoice {
    /// Color only when writing to a terminal.
    #[default]
    Auto,
    /// Always color, even when piping (useful with `less -R`).
    Always,
    /// Never color.
    Never,
}

impl ColorChoice {
    /// Resolves the choice into a yes/no answer for the current stdout.
    pub fn should_color(self, stdout_is_terminal: bool) -> bool {
        match self {
            ColorChoice::Auto => stdout_is_terminal,
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        }
    }
}

/// The semantic role of a piece of output text; each role has one style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Directory names.
    Dir,
    /// Symbolic link names.
    Symlink,
    /// Sizes such as `12.4 GiB`.
    Size,
    /// Bars and histogram blocks.
    Bar,
    /// The part of a name or line that matched a pattern.
    Match,
    /// File names used as headings (grep).
    Path,
    /// Line numbers (grep).
    LineNumber,
    /// Less important text: separators, collapsed rows, hints.
    Dim,
    /// Section headings.
    Header,
    /// Warnings printed to stderr.
    Warning,
    /// The `error:` prefix on stderr.
    Error,
}

impl Role {
    fn style(self) -> Style {
        match self {
            Role::Dir => Style::new().blue().bold(),
            Role::Symlink => Style::new().cyan(),
            Role::Size | Role::Header => Style::new().bold(),
            Role::Bar | Role::LineNumber => Style::new().green(),
            Role::Match | Role::Error => Style::new().red().bold(),
            Role::Path => Style::new().magenta(),
            Role::Dim => Style::new().dimmed(),
            Role::Warning => Style::new().yellow().bold(),
        }
    }
}

/// Paints text according to its [`Role`] — or leaves it alone when colors are off.
///
/// `Palette` is `Copy` (it is just one `bool`), so it can be passed by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    /// A palette that colors when `enabled` is true.
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// A palette that never colors (used by tests and JSON output).
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// Is this palette producing ANSI escape codes?
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Wraps `value` so that displaying it applies the style for `role`.
    ///
    /// Nothing is allocated: [`Painted`] borrows `value` and writes the escape
    /// codes around it when it is formatted.
    pub fn paint<T: fmt::Display + ?Sized>(self, role: Role, value: &T) -> Painted<'_, T> {
        Painted {
            value,
            style: self.enabled.then(|| role.style()),
        }
    }
}

/// A value plus an optional style, produced by [`Palette::paint`].
pub struct Painted<'a, T: ?Sized> {
    value: &'a T,
    style: Option<Style>,
}

impl<T: fmt::Display + ?Sized> fmt::Display for Painted<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.style {
            // Pass the formatter through so width/alignment flags still apply.
            Some(style) => fmt::Display::fmt(&style.style(self.value), f),
            None => fmt::Display::fmt(self.value, f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_colors_only_on_a_terminal() {
        assert!(ColorChoice::Auto.should_color(true));
        assert!(!ColorChoice::Auto.should_color(false));
        assert!(ColorChoice::Always.should_color(false));
        assert!(!ColorChoice::Never.should_color(true));
    }

    #[test]
    fn plain_palette_writes_text_unchanged_and_enabled_palette_adds_escapes() {
        assert_eq!(
            Palette::plain().paint(Role::Match, "abc").to_string(),
            "abc"
        );
        let colored = Palette::new(true).paint(Role::Match, "abc").to_string();
        assert!(colored.contains("abc"));
        assert!(
            colored.starts_with("\u{1b}["),
            "expected an ANSI escape: {colored:?}"
        );
    }

    #[test]
    fn padding_flags_are_respected_without_color() {
        assert_eq!(
            format!("{:>5}", Palette::plain().paint(Role::Size, "ab")),
            "   ab"
        );
    }
}
