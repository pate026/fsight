//! Integration tests for `fsight dupes`.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use common::{Fixture, json_of, stdout_of};
use predicates::prelude::*;

const KIB: usize = 1024;

/// Duplicates of several sizes, plus near-misses that each stage must reject.
fn fixture() -> Fixture {
    let f = Fixture::new();
    // Three copies of a 100 KiB file: 200 KiB wasted.
    for path in ["photos/a.jpg", "backup/a.jpg", "old/a-copy.jpg"] {
        f.file_of_size(path, 100 * KIB, 1);
    }
    // Two copies of a tiny text file: 11 bytes wasted.
    f.file("docs/x.txt", "hello world");
    f.file("docs/y.txt", "hello world");
    // Same size as the photos, different content from the first byte (stage 2 rejects).
    f.file_of_size("unique.bin", 100 * KIB, 2);
    // Same size and same first 16 KiB, different tail (stage 3 rejects).
    let mut tail_a = vec![3u8; 40 * KIB];
    let mut tail_b = tail_a.clone();
    *tail_a.last_mut().unwrap() = 4;
    *tail_b.last_mut().unwrap() = 5;
    f.file("tails/a.dat", tail_a);
    f.file("tails/b.dat", tail_b);
    // Empty files are skipped unless --include-empty.
    f.file("empty1", "");
    f.file("empty2", "");
    f
}

#[test]
fn reports_groups_sorted_by_wasted_space_with_a_summary() {
    let f = fixture();
    let out = stdout_of(f.cmd().args(["dupes", "--no-color"]), 0);
    assert_eq!(
        out,
        "\
100.0 KiB × 3 copies (200.0 KiB wasted)
  backup/a.jpg
  old/a-copy.jpg
  photos/a.jpg

11 B × 2 copies (11 B wasted)
  docs/x.txt
  docs/y.txt

Found 2 duplicate groups, 5 files, 200.0 KiB wasted
"
    );
}

#[test]
fn json_report_is_valid_and_complete() {
    let f = fixture();
    let json = json_of(f.cmd().arg("dupes"), 0);
    let groups = json["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["size"], 100 * 1024);
    assert_eq!(groups[0]["wasted"], 200 * 1024);
    assert_eq!(groups[0]["files"].as_array().unwrap().len(), 3);
    assert_eq!(groups[1]["files"][0]["path"], "docs/x.txt");
    assert_eq!(json["duplicate_files"], 5);
    assert_eq!(json["wasted"], 200 * 1024 + 11);
    assert_eq!(
        json["files_checked"], 8,
        "every non-empty file was considered"
    );
    assert_eq!(json["errors"], 0);
}

#[test]
fn empty_files_only_with_include_empty() {
    let f = fixture();
    let out = stdout_of(f.cmd().args(["dupes", "--include-empty", "--no-color"]), 0);
    assert!(
        out.contains("0 B × 2 copies (0 B wasted)\n  empty1\n  empty2\n"),
        "{out}"
    );
    assert!(
        out.ends_with("Found 3 duplicate groups, 7 files, 200.0 KiB wasted\n"),
        "{out}"
    );
}

#[test]
fn min_size_and_extension_filters() {
    let f = fixture();
    let out = stdout_of(f.cmd().args(["dupes", "--min-size", "1K", "--no-color"]), 0);
    assert!(!out.contains("docs/x.txt"), "{out}");
    assert!(
        out.ends_with("Found 1 duplicate group, 3 files, 200.0 KiB wasted\n"),
        "{out}"
    );

    let out = stdout_of(f.cmd().args(["dupes", "-e", "TXT", "--no-color"]), 0);
    assert!(
        out.ends_with("Found 1 duplicate group, 2 files, 11 B wasted\n"),
        "{out}"
    );
}

#[test]
fn hard_links_are_shown_but_not_counted_as_duplicates() {
    let f = Fixture::new();
    f.file_of_size("original.bin", 50 * KIB, 7);
    f.hard_link("original.bin", "link.bin");
    // A file and its hard link only: nothing is duplicated.
    let out = stdout_of(f.cmd().args(["dupes", "--no-color"]), 0);
    assert_eq!(out, "Found 0 duplicate groups, 0 files, 0 B wasted\n");

    // Add a real copy: one group of two files, and the hard link is noted.
    f.file_of_size("copy.bin", 50 * KIB, 7);
    let out = stdout_of(f.cmd().args(["dupes", "--no-color"]), 0);
    assert_eq!(
        out,
        "50.0 KiB × 2 copies (50.0 KiB wasted)\n  copy.bin\n  link.bin  (+1 hard link: original.bin)\n\n\
         Found 1 duplicate group, 2 files, 50.0 KiB wasted\n"
    );
    let json = json_of(f.cmd().arg("dupes"), 0);
    assert_eq!(json["hard_links"], 1);
}

/// Contents and modification time of every file below `root`.
fn snapshot(root: &std::path::Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                files.insert(path.clone(), (fs::read(&path).unwrap(), modified));
            }
        }
    }
    files
}

#[test]
fn dupes_never_modifies_anything() {
    let f = fixture();
    let before = snapshot(f.path());
    f.cmd()
        .args(["dupes", "--include-empty"])
        .assert()
        .success();
    f.cmd().args(["dupes", "--json"]).assert().success();
    assert_eq!(snapshot(f.path()), before);
    // And there is no option that could delete files.
    let help = stdout_of(f.cmd().args(["dupes", "--help"]), 0);
    assert!(!help.to_lowercase().contains("--delete"));
}

#[cfg(unix)]
#[test]
fn unreadable_candidates_are_counted_and_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let locked = f.file_of_size("locked.jpg", 100 * KIB, 1);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        return; // running as root
    }
    f.cmd()
        .args(["dupes", "--no-color"])
        .assert()
        .success()
        .stdout(predicate::str::contains("100.0 KiB × 3 copies"))
        .stderr(predicate::str::contains("1 entry could not be read"));
}
