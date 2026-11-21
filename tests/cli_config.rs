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
