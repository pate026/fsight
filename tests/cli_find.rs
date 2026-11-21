//! Integration tests for `fsight find`.

mod common;

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::Duration;

use common::{Fixture, json_of, stdout_of};
use predicates::prelude::*;

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.file("src/main.rs", "fn main() {}\n");
    f.file("src/lib.rs", "pub fn lib() {}\n");
    f.file("src/util/Helpers.RS", "// uppercase extension\n");
    f.file("docs/report-01.pdf", "%PDF");
    f.file("docs/report-2.pdf", "%PDF");
    f.file("docs/annual-report.txt", "text");
    f.file("notes.TXT", "notes");
    f.file_of_size("big.bin", 2 * 1024 * 1024, 0);
    f.file_of_size("small.bin", 10, 1);
    f.file(".hidden/secret.rs", "// hidden");
    f.file(".gitignore", "target/\n");
    f.file("target/debug.rs", "// ignored");
    #[cfg(unix)]
    f.symlink("src", "link");
    f
}

/// Runs `fsight find ARGS` and returns the printed paths.
fn find(f: &Fixture, args: &[&str]) -> Vec<String> {
    let out = stdout_of(f.cmd().arg("find").args(args).arg("--no-color"), 0);
    out.lines().map(str::to_string).collect()
}

#[test]
fn globs_match_file_names_and_output_is_sorted() {
    let f = fixture();
    assert_eq!(
        find(&f, &["*.rs"]),
        ["src/lib.rs", "src/main.rs", "src/util/Helpers.RS"]
    );
    assert_eq!(find(&f, &["report-??.pdf"]), ["docs/report-01.pdf"]);
}

#[test]
fn smart_case_and_explicit_case_flags() {
    let f = fixture();
    assert_eq!(find(&f, &["*.RS"]), ["src/util/Helpers.RS"]);
    assert_eq!(find(&f, &["-i", "*.RS"]).len(), 3);
    assert_eq!(find(&f, &["-s", "*.rs"]), ["src/lib.rs", "src/main.rs"]);
}

#[test]
fn a_plain_word_matches_anywhere_in_the_name() {
    let f = fixture();
    assert_eq!(
        find(&f, &["report"]),
        [
            "docs/annual-report.txt",
            "docs/report-01.pdf",
            "docs/report-2.pdf"
        ]
    );
}

#[test]
fn regex_mode() {
    let f = fixture();
    assert_eq!(
        find(&f, &["-r", r"^report-\d+\.pdf$"]),
        ["docs/report-01.pdf", "docs/report-2.pdf"]
    );
}

#[test]
fn type_filters_can_be_combined() {
    let f = fixture();
    assert_eq!(find(&f, &["-t", "d"]), ["docs", "src", "src/util"]);
    #[cfg(unix)]
    {
        assert_eq!(find(&f, &["-t", "l"]), ["link"]);
        assert_eq!(
            find(&f, &["-t", "l", "-t", "d", "--max-depth", "1"]),
            ["docs", "link", "src"]
        );
    }
}

#[test]
fn extension_filters_are_repeatable_and_case_insensitive() {
    let f = fixture();
    assert_eq!(
        find(&f, &["-e", "pdf", "-e", ".TXT"]),
        [
            "docs/annual-report.txt",
            "docs/report-01.pdf",
            "docs/report-2.pdf",
            "notes.TXT"
        ]
    );
}

#[test]
fn size_filters_select_ranges_of_files() {
    let f = fixture();
    assert_eq!(find(&f, &["--size", "+1M"]), ["big.bin"]);
    assert_eq!(find(&f, &["--size", "-10", "-e", "bin"]), ["small.bin"]);
    assert_eq!(
        find(&f, &["--size", "+5", "--size", "-12", "-t", "f", "*.bin"]),
        ["small.bin"]
    );
}

#[test]
fn time_filters_use_the_modification_time() {
    let f = fixture();
    f.set_age("notes.TXT", Duration::from_secs(30 * 86_400));
    assert_eq!(find(&f, &["--older", "1w"]), ["notes.TXT"]);
    let recent = find(&f, &["--newer", "1w", "-t", "f"]);
    assert!(!recent.contains(&"notes.TXT".to_string()));
    assert!(recent.contains(&"src/main.rs".to_string()));
}

#[test]
fn max_depth_limits_how_deep_the_search_goes() {
    let f = fixture();
    // There are no .rs files directly in the root.
    f.cmd()
        .args(["find", "*.rs", "--max-depth", "1"])
        .assert()
        .code(1);
    assert_eq!(
        find(&f, &["*.rs", "--max-depth", "2"]),
        ["src/lib.rs", "src/main.rs"]
    );
}

#[test]
fn full_path_matching() {
    let f = fixture();
    assert_eq!(find(&f, &["-p", "**/util/*"]), ["src/util/Helpers.RS"]);
    // Without --full-path the pattern only sees file names, which have no '/'.
    f.cmd().args(["find", "**/util/*"]).assert().code(1);
}

#[test]
fn hidden_and_ignored_files_need_flags() {
    let f = fixture();
    assert!(!find(&f, &["*.rs"]).contains(&".hidden/secret.rs".to_string()));
    assert!(find(&f, &["*.rs", "--hidden"]).contains(&".hidden/secret.rs".to_string()));
    assert!(find(&f, &["*.rs", "--no-ignore"]).contains(&"target/debug.rs".to_string()));
}

#[test]
fn long_format_shows_size_and_utc_time() {
    let f = fixture();
    let lines = find(&f, &["big.bin", "-l"]);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.starts_with("   2.0 MiB  "), "{line:?}");
    assert!(line.ends_with("  big.bin"), "{line:?}");
    let date = &line[12..28];
    assert!(
        date.len() == 16 && date.as_bytes()[4] == b'-' && date.as_bytes()[13] == b':',
        "{date:?}"
    );
}

#[test]
fn json_lists_matches_with_metadata() {
    let f = fixture();
    let json = json_of(f.cmd().args(["find", "*.bin"]), 0);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["path"], "big.bin");
    assert_eq!(matches[0]["type"], "file");
    assert_eq!(matches[0]["size"], 2 * 1024 * 1024);
    assert!(matches[0]["modified"].as_i64().unwrap() > 1_600_000_000);
    assert_eq!(json["errors"], 0);
}

#[test]
fn exit_status_is_1_without_matches_and_2_on_errors() {
    let f = fixture();
    f.cmd()
        .args(["find", "no-such-name"])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty());
    f.cmd()
        .args(["find", "-r", "(unclosed"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid pattern"));
    f.cmd().args(["find", "x", "missing-dir"]).assert().code(2);
}

/// Removes ANSI escape sequences such as `ESC[31;1m`.
fn strip_ansi(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            plain.push(c);
        }
    }
    plain
}

#[test]
fn matches_are_highlighted_when_colors_are_on() {
    let f = fixture();
    let out = stdout_of(
        f.cmd()
            .args(["find", "-r", "util", "--color", "always", "-t", "d"]),
        0,
    );
    assert_eq!(strip_ansi(&out), "src/util\n");
    // The match is wrapped in its own escape sequence: ESC[...m util ESC[0m
    let start = out.find("util").unwrap();
    assert!(out[..start].ends_with('m'), "{out:?}");
    assert!(out[start + 4..].starts_with("\u{1b}[0m"), "{out:?}");
    // Without --color always (stdout is a pipe here) there are no escapes.
    let plain = stdout_of(f.cmd().args(["find", "-r", "util", "-t", "d"]), 0);
    assert_eq!(plain, "src/util\n");
}

#[test]
fn closing_the_pipe_early_exits_quietly() {
    let f = Fixture::new();
    for i in 0..4000 {
        f.file(&format!("many/some-fairly-long-file-name-{i:05}.txt"), "");
    }
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fsight"))
        .args(["find", "*.txt"])
        .current_dir(f.path())
        .env("XDG_CONFIG_HOME", f.home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    drop(reader); // like `| head -1`: the reader goes away
    let output = child.wait_with_output().unwrap();
    assert!(first.starts_with("many/"));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "no panic, no error message"
    );
}
