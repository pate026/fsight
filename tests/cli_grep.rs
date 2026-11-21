//! Integration tests for `fsight grep`.

mod common;

use std::io::{BufRead, BufReader};
use std::process::Stdio;

use common::{Fixture, json_of, stdout_of};
use predicates::prelude::*;

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.file(
        "src/main.rs",
        "fn main() {\n    println!(\"hello\");\n    // TODO: tidy up\n}\n",
    );
    f.file(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n// todo: lowercase\n",
    );
    f.file(
        "notes.md",
        "Line one\nTODO write docs\nline three\nline four\nline five\nTODO again\n",
    );
    f.file("words.txt", "cat\ncatalog\nthe cat sat\n");
    f.file("data.bin", b"TODO\x00binary\x00stuff\n");
    f.file("latin1.txt", b"caf\xe9 TODO\n");
    f.file(".hidden.txt", "TODO hidden\n");
    f
}

fn grep(f: &Fixture, args: &[&str]) -> String {
    stdout_of(f.cmd().arg("grep").args(args).arg("--no-color"), 0)
}

#[test]
fn output_is_grouped_by_file_with_line_numbers() {
    let f = fixture();
    let out = grep(&f, &["TODO", "notes.md"]);
    assert_eq!(out, "notes.md\n2:TODO write docs\n6:TODO again\n");
    let out = grep(&f, &["TODO", "src"]);
    assert_eq!(out, "src/main.rs\n3:    // TODO: tidy up\n");
}

#[test]
fn files_are_listed_in_sorted_order_with_blank_lines_between() {
    let f = fixture();
    let out = grep(&f, &["TODO", "-j", "4"]);
    assert_eq!(
        out,
        "latin1.txt\n1:caf\u{fffd} TODO\n\n\
         notes.md\n2:TODO write docs\n6:TODO again\n\n\
         src/main.rs\n3:    // TODO: tidy up\n"
    );
    // Same result on every run, whatever order the threads finish in.
    for _ in 0..3 {
        assert_eq!(grep(&f, &["TODO", "-j", "4"]), out);
    }
}

#[test]
fn ignore_case_word_and_fixed_string_flags() {
    let f = fixture();
    assert!(grep(&f, &["-i", "todo", "src"]).contains("src/lib.rs\n4:// todo: lowercase"));
    assert_eq!(
        grep(&f, &["-w", "cat", "words.txt"]),
        "words.txt\n1:cat\n3:the cat sat\n"
    );
    f.file("expr.txt", "a+b\naab\n");
    assert_eq!(grep(&f, &["-F", "a+b", "expr.txt"]), "expr.txt\n1:a+b\n");
    assert_eq!(grep(&f, &["a+b", "expr.txt"]), "expr.txt\n2:aab\n");
}

#[test]
fn context_lines_and_group_separators() {
    let f = fixture();
    assert_eq!(
        grep(&f, &["TODO", "notes.md", "-C", "1"]),
        "notes.md\n1-Line one\n2:TODO write docs\n3-line three\n--\n5-line five\n6:TODO again\n"
    );
    assert_eq!(
        grep(&f, &["TODO", "notes.md", "-A", "1"]),
        "notes.md\n2:TODO write docs\n3-line three\n--\n6:TODO again\n"
    );
    assert_eq!(
        grep(&f, &["TODO", "notes.md", "-B", "2"]),
        "notes.md\n1-Line one\n2:TODO write docs\n--\n4-line four\n5-line five\n6:TODO again\n"
    );
}

#[test]
fn count_and_files_with_matches() {
    let f = fixture();
    assert_eq!(
        grep(&f, &["-c", "TODO"]),
        "latin1.txt:1\nnotes.md:2\nsrc/main.rs:1\n"
    );
    assert_eq!(
        grep(&f, &["-l", "TODO"]),
        "latin1.txt\nnotes.md\nsrc/main.rs\n"
    );
}

#[test]
fn globs_restrict_and_exclude_files() {
    let f = fixture();
    assert_eq!(
        grep(&f, &["-l", "-i", "todo", "-g", "*.rs"]),
        "src/lib.rs\nsrc/main.rs\n"
    );
    assert_eq!(
        grep(&f, &["-l", "TODO", "-g", "!*.md", "-g", "!*.txt"]),
        "src/main.rs\n"
    );
}

#[test]
fn binary_files_are_skipped_unless_requested() {
    let f = fixture();
    f.cmd().args(["grep", "binary"]).assert().code(1);
    let out = grep(&f, &["binary", "--binary"]);
    assert!(
        out.starts_with("data.bin\n1:TODO\0binary\0stuff"),
        "{out:?}"
    );
}

#[test]
fn max_filesize_skips_large_files() {
    let f = fixture();
    let mut big = "filler line\n".repeat(200_000); // about 2.3 MiB
    big.push_str("needle at the end\n");
    f.file("big.log", big);
    assert_eq!(grep(&f, &["-l", "needle"]), "big.log\n");
    f.cmd()
        .args(["grep", "needle", "--max-filesize", "1M"])
        .assert()
        .code(1);
}

#[test]
fn invalid_utf8_content_is_shown_lossily() {
    let f = fixture();
    assert_eq!(
        grep(&f, &["caf", "latin1.txt"]),
        "latin1.txt\n1:caf\u{fffd} TODO\n"
    );
}

#[test]
fn hidden_files_need_the_hidden_flag() {
    let f = fixture();
    assert!(!grep(&f, &["-l", "TODO"]).contains(".hidden.txt"));
    assert!(grep(&f, &["-l", "TODO", "--hidden"]).contains(".hidden.txt"));
}

#[test]
fn exit_status_reflects_matches_and_errors() {
    let f = fixture();
    f.cmd().args(["grep", "TODO"]).assert().code(0);
    f.cmd()
        .args(["grep", "no such text anywhere"])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty());
    f.cmd()
        .args(["grep", "(unclosed"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid pattern"));
    f.cmd().args(["grep", "x", "missing.txt"]).assert().code(2);
}

#[test]
fn json_output_contains_files_lines_spans_and_totals() {
    let f = fixture();
    let json = json_of(f.cmd().args(["grep", "TODO", "notes.md"]), 0);
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "notes.md");
    assert_eq!(files[0]["matched_lines"], 2);
    let line = &files[0]["lines"][0];
    assert_eq!(line["number"], 2);
    assert_eq!(line["kind"], "match");
    assert_eq!(line["text"], "TODO write docs");
    assert_eq!(line["matches"][0]["start"], 0);
    assert_eq!(line["matches"][0]["end"], 4);
    assert_eq!(json["files_searched"], 1);
    assert_eq!(json["matched_lines"], 2);
    assert_eq!(json["errors"], 0);

    let none = json_of(f.cmd().args(["grep", "absent"]), 1);
    assert_eq!(none["files"], serde_json::json!([]));
}

#[test]
fn matches_are_highlighted_with_color() {
    let f = fixture();
    let out = stdout_of(
        f.cmd()
            .args(["grep", "write", "notes.md", "--color", "always"]),
        0,
    );
    assert!(out.contains("TODO \u{1b}["), "{out:?}");
    assert!(out.contains("write\u{1b}[0m docs"), "{out:?}");
}

#[cfg(unix)]
#[test]
fn unreadable_files_are_reported_but_do_not_stop_the_search() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let secret = f.file("secret.txt", "TODO secret\n");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&secret).is_ok() {
        return; // running as root
    }
    f.cmd()
        .args(["grep", "-l", "TODO"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("notes.md"))
        .stderr(predicate::str::contains("1 entry could not be read"));
}

#[test]
fn closing_the_pipe_early_exits_quietly() {
    let f = Fixture::new();
    f.file(
        "many.txt",
        "a matching line with some padding text\n".repeat(20_000),
    );
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fsight"))
        .args(["grep", "matching"])
        .current_dir(f.path())
        .env("XDG_CONFIG_HOME", f.home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    drop(reader);
    let output = child.wait_with_output().unwrap();
    assert_eq!(first, "many.txt\n");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}
