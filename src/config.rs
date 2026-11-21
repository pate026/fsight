//! Configuration: built-in defaults, overridden by the config file, then by
//! environment variables, then by command-line flags.
//!
//! Every source is turned into a [`Layer`] — the same shape as [`Config`]
//! but with every field optional — and the layers are applied on top of
//! the defaults in order of increasing priority.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

use crate::cli::{Cli, Command};
use crate::error::Result;
use crate::output::color::ColorChoice;
use crate::tui::app::SortMode;

/// The effective configuration after merging all sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Config {
    /// Worker threads for walking, searching and hashing.
    pub threads: usize,
    /// Include hidden files.
    pub hidden: bool,
    /// Ignore `.gitignore` / `.ignore` rules.
    pub no_ignore: bool,
    /// When to use colors.
    pub color: ColorChoice,
    /// Defaults for `fsight scan`.
    pub scan: ScanConfig,
    /// Defaults for `fsight tui`.
    pub tui: TuiConfig,
}

/// The `[scan]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanConfig {
    /// Levels to display.
    pub depth: usize,
    /// Largest children shown per directory (0 = all).
    pub top: usize,
}

/// The `[tui]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TuiConfig {
    /// Initial sort order.
    pub sort: SortMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism().map_or(1, NonZeroUsize::get),
            hidden: false,
            no_ignore: false,
            color: ColorChoice::Auto,
            scan: ScanConfig { depth: 3, top: 10 },
            tui: TuiConfig {
                sort: SortMode::Size,
            },
        }
    }
}

/// One source of settings. `None` means "this source does not say".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub threads: Option<NonZeroUsize>,
    pub hidden: Option<bool>,
    pub no_ignore: Option<bool>,
    pub color: Option<ColorChoice>,
    #[serde(default)]
    pub scan: ScanLayer,
    #[serde(default)]
    pub tui: TuiLayer,
}

/// The optional `[scan]` settings of a [`Layer`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanLayer {
    pub depth: Option<usize>,
    pub top: Option<usize>,
}

/// The optional `[tui]` settings of a [`Layer`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuiLayer {
    pub sort: Option<SortMode>,
}

impl Config {
    /// Builds the effective configuration for this invocation.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Config`] if a configuration source is invalid.
    pub fn load(cli: &Cli) -> Result<Config> {
        Ok(Config::merged(&[Layer::from_cli(cli)]))
    }

    /// Applies `layers` (lowest priority first) on top of the defaults.
    pub fn merged(layers: &[Layer]) -> Config {
        let mut config = Config::default();
        for layer in layers {
            config.apply(layer);
        }
        config
    }

    /// Overrides every setting that `layer` specifies.
    fn apply(&mut self, layer: &Layer) {
        if let Some(threads) = layer.threads {
            self.threads = threads.get();
        }
        if let Some(hidden) = layer.hidden {
            self.hidden = hidden;
        }
        if let Some(no_ignore) = layer.no_ignore {
            self.no_ignore = no_ignore;
        }
        if let Some(color) = layer.color {
            self.color = color;
        }
        if let Some(depth) = layer.scan.depth {
            self.scan.depth = depth;
        }
        if let Some(top) = layer.scan.top {
            self.scan.top = top;
        }
        if let Some(sort) = layer.tui.sort {
            self.tui.sort = sort;
        }
    }
}

impl Layer {
    /// The settings given as command-line flags.
    pub fn from_cli(cli: &Cli) -> Layer {
        let global = &cli.global;
        let mut layer = Layer {
            threads: global.threads,
            hidden: global.hidden_flag(),
            no_ignore: global.no_ignore_flag(),
            color: global.color_flag(),
            ..Layer::default()
        };
        match &cli.command {
            Some(Command::Scan(args)) => {
                layer.scan.depth = args.depth;
                layer.scan.top = args.top;
            }
            Some(Command::Tui(args)) => layer.tui.sort = args.sort,
            _ => {}
        }
        layer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn later_layers_override_earlier_ones_field_by_field() {
        let low = Layer {
            hidden: Some(true),
            scan: ScanLayer {
                depth: Some(7),
                top: Some(2),
            },
            ..Layer::default()
        };
        let high = Layer {
            scan: ScanLayer {
                depth: Some(1),
                top: None,
            },
            ..Layer::default()
        };
        let config = Config::merged(&[low, high]);
        assert!(config.hidden, "kept from the lower layer");
        assert_eq!(config.scan.depth, 1, "overridden by the higher layer");
        assert_eq!(config.scan.top, 2, "unset in the higher layer, so kept");
        assert_eq!(config.tui.sort, SortMode::Size, "built-in default");
    }

    #[test]
    fn cli_layer_takes_subcommand_specific_settings() {
        let cli = Cli::parse_from(["fsight", "-j", "3", "--no-hidden", "scan", "-d", "5"]);
        let layer = Layer::from_cli(&cli);
        assert_eq!(layer.threads, NonZeroUsize::new(3));
        assert_eq!(layer.hidden, Some(false));
        assert_eq!(layer.scan.depth, Some(5));
        assert_eq!(layer.scan.top, None);
    }
}
