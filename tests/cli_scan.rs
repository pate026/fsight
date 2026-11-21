//! Integration tests for `fsight scan`.

mod common;

use common::{Fixture, json_of, stdout_of};
use predicates::prelude::*;
use serde_json::Value;

/// A small project-like tree. With `--apparent-size` the sizes are exact and
/// identical on every file system, which makes the snapshots deterministic.
fn project() -> Fixture {
    let f = Fixture::new();
    f.file_of_size("docs/guide.md", 3000, b'g');
    f.file_of_size("docs/api.md", 1200, b'a');
    f.file_of_size("src/main.rs", 5000, b'm');
    f.file_of_size("src/lib.rs", 12000, b'l');
    f.file_of_size("src/util/helpers.rs", 800, b'h');
    f.file_of_size("src/util/mod.rs", 200, b'u');
    f.file_of_size("assets/logo.png", 40000, b'p');
    f.file_of_size("assets/icons/a.svg", 2000, b's');
    f.file_of_size("assets/icons/b.svg", 1500, b't');
    f.file_of_size("readme.md", 500, b'r');
    // Excluded by default: hidden, or matched by .gitignore.
    f.file_of_size(".hidden/secret.txt", 1000, b'x');
    f.file(".gitignore", "ignored/\n*.log\n"); // 15 bytes
    f.file_of_size("ignored/big.bin", 50000, b'b');
    f.file_of_size("debug.log", 3000, b'd');
    f
}

/// Sum of all visible files in `project()`.
const VISIBLE_TOTAL: u64 = 66_200;

fn total_size(f: &Fixture, extra: &[&str]) -> u64 {
    let json = json_of(f.cmd().args(["scan", "--apparent-size"]).args(extra), 0);
    json["tree"]["size"].as_u64().expect("size is a number")
}

#[test]
fn default_tree_snapshot() {
    let f = project();
    let out = stdout_of(f.cmd().args(["scan", "--apparent-size", "--no-color"]), 0);
    insta::assert_snapshot!("scan_default", out);
}

#[test]
fn depth_and_top_snapshot() {
    let f = project();
    let out = stdout_of(
        f.cmd().args([
            "scan",
            "--apparent-size",
            "--no-color",
            "-d",
            "1",
            "-n",
            "2",
        ]),
        0,
    );
    insta::assert_snapshot!("scan_depth1_top2", out);
}

/// In apparent mode a directory's size must equal the sum of its children.
fn assert_sizes_add_up(node: &Value) {
    let children = node["children"].as_array().expect("children array");
    let other = node["other"]["size"].as_u64().unwrap_or(0);
    let sum: u64 = children.iter().map(|c| c["size"].as_u64().unwrap()).sum();
    if !children.is_empty() {
        assert_eq!(
            node["size"].as_u64().unwrap(),
            sum + other,
            "{}",
            node["name"]
        );
    }
    children.iter().for_each(assert_sizes_add_up);
}

#[test]
fn json_output_is_valid_and_sizes_add_up() {
    let f = project();
    let json = json_of(
        f.cmd()
            .args(["scan", "--apparent-size", "-d", "10", "-n", "0"]),
        0,
    );
    assert_eq!(json["root"], ".");
    assert_eq!(json["size_mode"], "apparent");
    assert_eq!(json["errors"], 0);
    assert_eq!(json["interrupted"], false);
    assert_eq!(json["tree"]["size"], VISIBLE_TOTAL);
    assert_sizes_add_up(&json["tree"]);
}

#[test]
fn compact_json_is_a_single_line() {
    let f = project();
    let out = stdout_of(f.cmd().args(["scan", "--compact"]), 0);
    assert_eq!(out.lines().count(), 1);
    serde_json::from_str::<Value>(&out).expect("valid JSON");
}

#[test]
fn hidden_and_ignore_flags_change_what_is_counted() {
    let f = project();
    assert_eq!(total_size(&f, &[]), VISIBLE_TOTAL);
    assert_eq!(total_size(&f, &["--hidden"]), VISIBLE_TOTAL + 1000 + 15);
    assert_eq!(
        total_size(&f, &["--no-ignore"]),
        VISIBLE_TOTAL + 50_000 + 3000
    );
    assert_eq!(
        total_size(&f, &["--hidden", "--no-ignore"]),
        VISIBLE_TOTAL + 1000 + 15 + 50_000 + 3000
    );
}

#[test]
fn hard_links_are_counted_once() {
    let f = Fixture::new();
    f.file_of_size("original.bin", 10_000, b'o');
    f.dir("other");
    f.hard_link("original.bin", "other/link.bin");
    let json = json_of(f.cmd().args(["scan", "--apparent-size"]), 0);
    assert_eq!(json["tree"]["size"], 10_000);
    // Credited to the path that sorts first ("original.bin" < "other/link.bin").
    let other = json["tree"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "other")
        .unwrap();
    assert_eq!(other["size"], 0);
}

#[test]
fn sparse_files_take_less_allocated_space_than_their_length() {
    let f = Fixture::new();
    let file = std::fs::File::create(f.join("sparse.img")).unwrap();
    file.set_len(10 * 1024 * 1024).unwrap(); // a 10 MiB hole, nothing written
    assert!(total_size(&f, &[]) >= 10 * 1024 * 1024);
    let allocated = json_of(f.cmd().arg("scan"), 0)["tree"]["size"]
        .as_u64()
        .unwrap();
    assert!(allocated < 1024 * 1024, "allocated {allocated} bytes");
}

#[test]
fn min_size_hides_small_entries() {
    let f = project();
    let out = stdout_of(
        f.cmd().args([
            "scan",
            "--apparent-size",
            "--no-color",
            "-d",
            "1",
            "--min-size",
            "10K",
        ]),
        0,
    );
    assert!(out.contains("assets") && out.contains("src"), "{out}");
    assert!(!out.contains("docs") && !out.contains("readme.md"), "{out}");
}

#[test]
fn a_single_file_can_be_scanned() {
    let f = project();
    let json = json_of(f.cmd().args(["scan", "--apparent-size", "readme.md"]), 0);
    assert_eq!(json["tree"]["name"], "readme.md");
    assert_eq!(json["tree"]["kind"], "file");
    assert_eq!(json["tree"]["size"], 500);
}

#[test]
fn missing_path_is_an_error_with_exit_status_2() {
    let f = Fixture::new();
    f.cmd()
        .args(["scan", "does-not-exist"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot access 'does-not-exist'"));
}

#[cfg(unix)]
#[test]
fn unreadable_directories_are_counted_not_fatal() {
    let f = project();
    f.file_of_size("locked/inside.txt", 100, b'i');
    let Some(_guard) = common::Unreadable::new(f.join("locked")) else {
        return; // running as root: permissions cannot be tested
    };
    f.cmd()
        .args(["scan", "--apparent-size"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src"))
        .stderr(predicate::str::contains(
            "warning: 1 entry could not be read (run with -v for details)",
        ));
    // With -v the reason is logged too.
    f.cmd()
        .args(["scan", "-v"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Permission denied"));
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed_unless_asked() {
    let f = Fixture::new();
    let outside = Fixture::new();
    outside.file_of_size("big.bin", 100_000, b'b');
    f.file_of_size("small.txt", 10, b's');
    f.symlink(outside.path().to_str().unwrap(), "link");

    let plain = total_size(&f, &[]);
    assert!(plain < 1000, "the link itself is tiny: {plain}");
    let followed = total_size(&f, &["--follow"]);
    assert_eq!(followed, 100_010);
}

#[cfg(unix)]
#[test]
fn symlink_loops_are_detected_when_following() {
    let f = Fixture::new();
    f.file_of_size("a/file.txt", 10, b'f');
    f.symlink("..", "a/loop"); // a/loop -> a/.. (the root): a cycle
    f.cmd()
        .args(["scan", "--follow", "--apparent-size"])
        .assert()
        .success()
        .stderr(predicate::str::contains("could not be read"));
    // Without --follow the link is just a small entry and nothing is reported.
    f.cmd()
        .args(["scan"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn names_that_are_not_utf8_are_shown_lossily() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    let name = OsStr::from_bytes(b"bad-\xff-name.txt");
    std::fs::write(f.path().join(name), "data").unwrap();
    let out = stdout_of(f.cmd().args(["scan", "--no-color"]), 0);
    assert!(out.contains("bad-\u{fffd}-name.txt"), "{out}");
    json_of(f.cmd().arg("scan"), 0);
}
