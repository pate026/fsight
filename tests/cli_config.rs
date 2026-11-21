//! Integration tests for the command line itself: help, version, exit codes,
//! configuration precedence and shell completions.

mod common;

use common::{Fixture, stdout_of};
use predicates::prelude::*;

#[test]
fn version_flag_prints_the_crate_version() {
    let fixture = Fixture::new();
    let out = stdout_of(fixture.cmd().arg("--version"), 0);
    assert_eq!(out.trim(), format!("fsight {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn missing_subcommand_is_a_usage_error() {
    let fixture = Fixture::new();
    fixture
        .cmd()
        .arg("--json")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("a subcommand is required"));
    // With no arguments at all, the help is shown (also with status 2).
    fixture
        .cmd()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage: fsight"));
}

#[test]
fn invalid_option_values_exit_with_status_2_and_explain_why() {
    let fixture = Fixture::new();
    fixture
        .cmd()
        .args(["scan", "--min-size", "10X"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown unit 'X'"));
    fixture
        .cmd()
        .args(["find", "--newer", "1.5h"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("use whole numbers"));
}

#[test]
fn help_screens_snapshot() {
    let fixture = Fixture::new();
    let main_help = stdout_of(fixture.cmd().arg("--help"), 0);
    insta::assert_snapshot!("help_main", main_help);
    for command in [
        "scan",
        "find",
        "grep",
        "dupes",
        "stats",
        "tui",
        "completions",
    ] {
        let help = stdout_of(fixture.cmd().args([command, "--help"]), 0);
        assert!(
            help.contains("Examples:"),
            "{command} --help needs examples"
        );
        insta::assert_snapshot!(format!("help_{command}"), help);
    }
}

/// A tree with one hidden and one git-ignored file.
fn tree() -> Fixture {
    let f = Fixture::new();
    f.file("visible.txt", "v");
    f.file(".secret.txt", "s");
    f.file(".gitignore", "ignored.txt\n");
    f.file("ignored.txt", "i");
    f
}

fn found(f: &Fixture, configure: impl FnOnce(&mut assert_cmd::Command)) -> String {
    let mut cmd = f.cmd();
    cmd.args(["find", "*.txt", "--no-color"]);
    configure(&mut cmd);
    String::from_utf8(cmd.output().unwrap().stdout).unwrap()
}

#[test]
fn config_file_sets_defaults() {
    let f = tree();
    assert_eq!(found(&f, |_| {}), "visible.txt\n");
    f.write_config("hidden = true\nno_ignore = true\n");
    assert_eq!(found(&f, |_| {}), ".secret.txt\nignored.txt\nvisible.txt\n");
}

#[test]
fn environment_overrides_config_file_and_flags_override_both() {
    let f = tree();
    f.write_config("hidden = true\n");
    assert!(found(&f, |_| {}).contains(".secret.txt"), "config file");
    assert!(
        !found(&f, |c| {
            c.env("FSIGHT_HIDDEN", "false");
        })
        .contains(".secret.txt"),
        "environment beats the config file"
    );
    assert!(
        found(&f, |c| {
            c.env("FSIGHT_HIDDEN", "false").arg("--hidden");
        })
        .contains(".secret.txt"),
        "a flag beats the environment"
    );
    assert!(
        !found(&f, |c| {
            c.arg("--no-hidden");
        })
        .contains(".secret.txt"),
        "--no-hidden beats the config file"
    );
}

#[test]
fn scan_depth_and_top_come_from_the_config_file() {
    let f = Fixture::new();
    for name in ["a", "b", "c"] {
        f.file_of_size(&format!("{name}/deep/file.bin"), 100, b'x');
    }
    f.write_config("[scan]\ndepth = 1\ntop = 2\n");
    let out = stdout_of(f.cmd().args(["scan", "--apparent-size", "--no-color"]), 0);
    assert_eq!(
        out.lines().count(),
        4,
        "root + 2 children + collapsed row:\n{out}"
    );
    assert!(out.contains("(1 other item)"), "{out}");
    let out = stdout_of(
        f.cmd()
            .args(["scan", "--apparent-size", "--no-color", "-n", "0"]),
        0,
    );
    assert_eq!(out.lines().count(), 4, "root + 3 children:\n{out}");
}

#[test]
fn print_config_shows_the_merged_result_as_toml() {
    let f = Fixture::new();
    let path = f.write_config("threads = 2\nhidden = true\n[tui]\nsort = \"name\"\n");
    let out = stdout_of(
        f.cmd()
            .env("FSIGHT_THREADS", "5")
            .args(["--print-config", "--no-color"]),
        0,
    );
    assert!(
        out.contains(&format!("# config file: {}", path.display())),
        "{out}"
    );
    let value: toml::Value = toml::from_str(&out).expect("valid TOML");
    assert_eq!(value["threads"].as_integer(), Some(5), "environment");
    assert_eq!(value["hidden"].as_bool(), Some(true), "config file");
    assert_eq!(value["color"].as_str(), Some("never"), "flag");
    assert_eq!(value["tui"]["sort"].as_str(), Some("name"), "config file");
    assert_eq!(value["scan"]["depth"].as_integer(), Some(3), "default");
}

#[test]
fn malformed_config_file_is_reported_with_path_and_location() {
    let f = Fixture::new();
    let path = f.write_config("hidden = true\n[scan]\ndepth = \"deep\"\n");
    f.cmd()
        .args(["scan"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(path.display().to_string()))
        .stderr(predicate::str::contains("line 3, column 9"));
    let path = f.write_config("hiden = true\n");
    f.cmd()
        .args(["stats"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(path.display().to_string()))
        .stderr(predicate::str::contains("unknown field `hiden`"));
}

#[test]
fn invalid_environment_values_are_errors() {
    let f = Fixture::new();
    f.cmd()
        .env("FSIGHT_THREADS", "lots")
        .arg("stats")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("FSIGHT_THREADS"));
}

#[test]
fn no_color_beats_the_config_file_but_not_the_color_flag() {
    let f = tree();
    f.write_config("color = \"always\"\n");
    let colored = |c: &mut assert_cmd::Command| {
        let out = c.args(["find", "visible"]).output().unwrap().stdout;
        String::from_utf8(out).unwrap().contains('\u{1b}')
    };
    assert!(colored(&mut f.cmd()), "config file says always");
    assert!(
        !colored(f.cmd().env("NO_COLOR", "1")),
        "NO_COLOR wins over the file"
    );
    assert!(
        colored(f.cmd().env("NO_COLOR", "1").args(["--color", "always"])),
        "the flag wins over NO_COLOR"
    );
}

#[test]
fn completions_are_generated_for_common_shells() {
    let f = Fixture::new();
    for (shell, marker) in [
        ("bash", "_fsight()"),
        ("zsh", "#compdef fsight"),
        ("fish", "complete -c fsight"),
    ] {
        let script = stdout_of(f.cmd().args(["completions", shell]), 0);
        assert!(
            script.contains(marker),
            "{shell}: {}",
            &script[..80.min(script.len())]
        );
        assert!(script.contains("dupes"), "{shell} completes subcommands");
    }
    f.cmd().args(["completions", "tcsh"]).assert().code(2);
}
