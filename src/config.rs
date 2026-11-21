//! Configuration: built-in defaults, overridden by the config file, then by
//! environment variables, then by command-line flags.
//!
//! Every source is turned into a [`Layer`] — the same shape as [`Config`]
//! but with every field optional — and the layers are applied on top of the
//! defaults in order of increasing priority:
//!
//! ```text
//! defaults  <  config file  <  FSIGHT_* / NO_COLOR  <  command-line flags
//! ```
//!
//! The functions that read a source take the environment as a parameter
//! (`lookup`) instead of calling `std::env::var` directly. That keeps them
//! pure: the tests pass in a fake environment. (Changing the real environment
//! is `unsafe` in Rust 2024 because other threads may read it concurrently.)

use std::ffi::OsString;
use std::fmt::Display;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::cli::{Cli, Command};
use crate::error::{Error, Result};
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
    /// Builds the effective configuration for this invocation from the real
    /// config file, the real environment and the parsed command line.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the config file cannot be read or parsed,
    /// or if an environment variable has an invalid value.
    pub fn load(cli: &Cli) -> Result<Config> {
        let lookup = |name: &str| std::env::var_os(name);
        let file = match config_path(&lookup) {
            Some(path) => Layer::from_file(&path)?,
            None => Layer::default(),
        };
        let env = Layer::from_env(&lookup)?;
        Ok(Config::merged(&[file, env, Layer::from_cli(cli)]))
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

    /// The configuration as TOML, in the same format as the config file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if serialization fails (it does not for this struct).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).map_err(|err| Error::Config(err.to_string()))
    }
}

/// Where the config file is: `$XDG_CONFIG_HOME/fsight/config.toml`, or
/// `$HOME/.config/fsight/config.toml` if `XDG_CONFIG_HOME` is unset, empty
/// or relative (the XDG specification says relative paths must be ignored).
pub fn config_path(lookup: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let xdg = lookup("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let base = xdg.or_else(|| {
        lookup("HOME")
            .map(PathBuf::from)
            .or_else(std::env::home_dir)
            .map(|home| home.join(".config"))
    })?;
    Some(base.join("fsight").join("config.toml"))
}

impl Layer {
    /// Reads the config file at `path`. A missing file is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the file exists but cannot be read or parsed.
    pub fn from_file(path: &Path) -> Result<Layer> {
        match fs::read_to_string(path) {
            Ok(text) => Layer::from_toml(&text, path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Layer::default()),
            Err(err) => Err(Error::Config(format!(
                "cannot read config file {}: {err}",
                path.display()
            ))),
        }
    }

    /// Parses config file text. `path` is only used in error messages.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] with the file path, line and column if the
    /// text is not valid TOML or contains an unknown or mistyped setting.
    pub fn from_toml(text: &str, path: &Path) -> Result<Layer> {
        // The toml error already says "TOML parse error at line X, column Y"
        // and shows the offending line.
        toml::from_str(text)
            .map_err(|err| Error::Config(format!("invalid config file {}:\n{err}", path.display())))
    }

    /// Reads the `FSIGHT_*` environment variables (and `NO_COLOR`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the variable if a value is invalid.
    pub fn from_env(lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Layer> {
        // Unset and empty variables both mean "not set".
        let get = |name: &str| {
            lookup(name)
                .map(|value| value.to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
        };
        let mut color = env_value(&get, "FSIGHT_COLOR", parse_enum::<ColorChoice>)?;
        if color.is_none() && get("NO_COLOR").is_some() {
            // https://no-color.org: any non-empty value disables colors.
            color = Some(ColorChoice::Never);
        }
        Ok(Layer {
            threads: env_value(&get, "FSIGHT_THREADS", parse_from_str::<NonZeroUsize>)?,
            hidden: env_value(&get, "FSIGHT_HIDDEN", parse_bool)?,
            no_ignore: env_value(&get, "FSIGHT_NO_IGNORE", parse_bool)?,
            color,
            scan: ScanLayer {
                depth: env_value(&get, "FSIGHT_SCAN_DEPTH", parse_from_str::<usize>)?,
                top: env_value(&get, "FSIGHT_SCAN_TOP", parse_from_str::<usize>)?,
            },
            tui: TuiLayer {
                sort: env_value(&get, "FSIGHT_TUI_SORT", parse_enum::<SortMode>)?,
            },
        })
    }

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

/// Reads and parses one environment variable; `Ok(None)` if it is not set.
fn env_value<T>(
    get: &dyn Fn(&str) -> Option<String>,
    name: &str,
    parse: fn(&str) -> std::result::Result<T, String>,
) -> Result<Option<T>> {
    get(name)
        .map(|value| {
            parse(value.trim()).map_err(|expected| {
                Error::Config(format!(
                    "invalid value '{value}' for {name}: expected {expected}"
                ))
            })
        })
        .transpose()
}

/// Accepts the usual spellings of yes and no.
fn parse_bool(value: &str) -> std::result::Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err("true or false".to_string()),
    }
}

/// Parses anything that implements [`FromStr`], such as numbers.
fn parse_from_str<T: FromStr>(value: &str) -> std::result::Result<T, String>
where
    T::Err: Display,
{
    value
        .parse()
        .map_err(|err: T::Err| format!("a whole number ({err})"))
}

/// Parses one of the values of a clap [`ValueEnum`], case-insensitively.
fn parse_enum<T: ValueEnum>(value: &str) -> std::result::Result<T, String> {
    T::from_str(value, true).map_err(|_| {
        let names: Vec<String> = T::value_variants()
            .iter()
            .filter_map(ValueEnum::to_possible_value)
            .map(|possible| possible.get_name().to_string())
            .collect();
        format!("one of {}", names.join(", "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::collections::HashMap;

    /// A fake environment for the pure `from_env` / `config_path` functions.
    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

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
    fn precedence_is_cli_then_env_then_file_then_defaults() {
        let file = Layer::from_toml(
            "threads = 2\nhidden = true\nno_ignore = true\ncolor = \"always\"\n\
             [scan]\ndepth = 5\ntop = 7\n[tui]\nsort = \"name\"\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let env = Layer::from_env(&env(&[
            ("FSIGHT_THREADS", "3"),
            ("FSIGHT_HIDDEN", "no"),
            ("FSIGHT_SCAN_DEPTH", "4"),
        ]))
        .unwrap();
        let cli = Layer::from_cli(&Cli::parse_from(["fsight", "--hidden", "scan", "-d", "1"]));
        let config = Config::merged(&[file, env, cli]);

        assert_eq!(config.threads, 3, "env beats file");
        assert!(config.hidden, "cli beats env (env said no, file said yes)");
        assert!(config.no_ignore, "only the file sets it");
        assert_eq!(config.color, ColorChoice::Always, "only the file sets it");
        assert_eq!(config.scan.depth, 1, "cli beats env and file");
        assert_eq!(config.scan.top, 7, "only the file sets it");
        assert_eq!(config.tui.sort, SortMode::Name);
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

    #[test]
    fn environment_values_are_parsed_flexibly_and_empty_means_unset() {
        let layer = Layer::from_env(&env(&[
            ("FSIGHT_HIDDEN", "TRUE"),
            ("FSIGHT_NO_IGNORE", "0"),
            ("FSIGHT_COLOR", "Never"),
            ("FSIGHT_TUI_SORT", "modified"),
            ("FSIGHT_SCAN_TOP", ""),
        ]))
        .unwrap();
        assert_eq!(layer.hidden, Some(true));
        assert_eq!(layer.no_ignore, Some(false));
        assert_eq!(layer.color, Some(ColorChoice::Never));
        assert_eq!(layer.tui.sort, Some(SortMode::Modified));
        assert_eq!(layer.scan.top, None);
    }

    #[test]
    fn no_color_disables_colors_unless_fsight_color_says_otherwise() {
        let layer = Layer::from_env(&env(&[("NO_COLOR", "1")])).unwrap();
        assert_eq!(layer.color, Some(ColorChoice::Never));
        let layer = Layer::from_env(&env(&[("NO_COLOR", "")])).unwrap();
        assert_eq!(layer.color, None, "an empty NO_COLOR is ignored");
        let layer =
            Layer::from_env(&env(&[("NO_COLOR", "1"), ("FSIGHT_COLOR", "always")])).unwrap();
        assert_eq!(layer.color, Some(ColorChoice::Always));
    }

    #[test]
    fn invalid_environment_values_name_the_variable() {
        let err = Layer::from_env(&env(&[("FSIGHT_THREADS", "0")])).unwrap_err();
        assert!(err.to_string().contains("FSIGHT_THREADS"), "{err}");
        let err = Layer::from_env(&env(&[("FSIGHT_HIDDEN", "maybe")])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid value 'maybe' for FSIGHT_HIDDEN: expected true or false"
        );
        let err = Layer::from_env(&env(&[("FSIGHT_TUI_SORT", "random")])).unwrap_err();
        assert!(
            err.to_string()
                .ends_with("expected one of size, name, modified"),
            "{err}"
        );
    }

    #[test]
    fn malformed_files_report_path_line_and_column() {
        let err = Layer::from_toml(
            "hidden = true\nthreads = \"many\"\n",
            Path::new("/x/config.toml"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/x/config.toml"), "{err}");
        assert!(err.contains("line 2, column 11"), "{err}");

        let err = Layer::from_toml("[scan]\ndeep = 3\n", Path::new("c.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `deep`"), "{err}");
    }

    #[test]
    fn missing_config_file_is_fine() {
        let layer = Layer::from_file(Path::new("/definitely/not/here/config.toml")).unwrap();
        assert_eq!(layer, Layer::default());
    }

    #[test]
    fn config_path_follows_the_xdg_rules() {
        let path = |vars: &[(&str, &str)]| config_path(&env(vars));
        assert_eq!(
            path(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]),
            Some(PathBuf::from("/xdg/fsight/config.toml"))
        );
        assert_eq!(
            path(&[("XDG_CONFIG_HOME", "relative/dir"), ("HOME", "/home/u")]),
            Some(PathBuf::from("/home/u/.config/fsight/config.toml")),
            "relative XDG paths are ignored"
        );
        assert_eq!(
            path(&[("HOME", "/home/u")]),
            Some(PathBuf::from("/home/u/.config/fsight/config.toml"))
        );
    }

    #[test]
    fn printed_config_round_trips_through_the_file_format() {
        let config = Config::default();
        let text = config.to_toml().unwrap();
        let layer = Layer::from_toml(&text, Path::new("printed.toml")).unwrap();
        assert_eq!(Config::merged(&[layer]), config);
        assert!(text.contains("[scan]\ndepth = 3\ntop = 10\n"), "{text}");
    }
}
