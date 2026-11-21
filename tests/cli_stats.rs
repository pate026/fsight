//! Integration tests for `fsight stats`.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{Fixture, json_of, stdout_of};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const DAY: u64 = 86_400;

/// 1 January of `year`, 12:00 UTC.
fn new_year(year: u64) -> SystemTime {
    let is_leap = |y: u64| (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days: u64 = (1970..year)
        .map(|y| if is_leap(y) { 366 } else { 365 })
        .sum();
    UNIX_EPOCH + Duration::from_secs(days * DAY + 12 * 3600)
}

/// A tree whose statistics are the same on every machine: fixed sizes, and
/// modification times that are either fixed dates or well inside an age bucket.
/// The two biggest files are sparse, so they cost no disk space.
fn fixture() -> Fixture {
    let f = Fixture::new();
    let dated: [(&str, u64); 11] = [
        ("src/main.rs", 1500),
        ("src/lib.rs", 8000),
        ("src/util.rs", 500),
        ("docs/guide.md", 3000),
        ("docs/notes.txt", 700),
        ("data/records.json", 150 * KIB),
        ("data/archive.tar.gz", 1536 * KIB),
        ("images/photo.JPG", 300 * KIB),
        ("images/icon.png", 2 * KIB),
        ("LICENSE", 1100),
        ("Makefile", 400),
    ];
    for (year, (path, size)) in (2001..).zip(dated) {
        f.file_of_size(path, usize::try_from(size).unwrap(), b'x');
        f.set_modified(path, new_year(year));
    }
    let sparse = |path: &str, size: u64| {
        let file = std::fs::File::create(f.dir("big").join(path)).unwrap();
        file.set_len(size).unwrap();
    };
    sparse("disk.img", 2 * GIB);
    f.set_modified("big/disk.img", new_year(2012));
    sparse("video.mp4", 200 * MIB);
    f.set_age("big/video.mp4", Duration::from_secs(100 * DAY));
    for (path, size, age_days) in [
        ("recent/today.log", 100, 0),
        ("recent/week.log", 200, 3),
        ("recent/month.log", 300, 14),
    ] {
        f.file_of_size(path, size, b'l');
        f.set_age(path, Duration::from_secs(age_days * DAY + 7200));
    }
    #[cfg(unix)]
    f.symlink("src", "link");
    f.file(".hidden/ignored.txt", "not counted");
    f
}

#[test]
fn text_report_snapshot() {
    let f = fixture();
    let out = stdout_of(f.cmd().args(["stats", "--no-color"]), 0);
    insta::assert_snapshot!("stats_text", out);
}

#[test]
fn json_report_is_valid_and_consistent() {
    let f = fixture();
    let json = json_of(f.cmd().arg("stats"), 0);
    let totals = &json["totals"];
    assert_eq!(totals["files"], 16);
    assert_eq!(totals["directories"], 6);
    assert_eq!(totals["symlinks"], u64::from(cfg!(unix)));

    // Every histogram accounts for every file and every byte.
    for histogram in ["size_histogram", "age_histogram"] {
        let groups = json[histogram].as_array().unwrap();
        let files: u64 = groups.iter().map(|g| g["files"].as_u64().unwrap()).sum();
        let size: u64 = groups.iter().map(|g| g["size"].as_u64().unwrap()).sum();
        assert_eq!(files, 16, "{histogram}");
        assert_eq!(Some(size), totals["size"].as_u64(), "{histogram}");
    }
    let ages: Vec<u64> = json["age_histogram"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["files"].as_u64().unwrap())
        .collect();
    assert_eq!(ages, [1, 1, 1, 1, 12]);

    assert_eq!(json["largest"][0]["path"], "big/disk.img");
    assert_eq!(json["largest"][0]["size"], 2 * GIB);
    assert_eq!(json["oldest"][0]["path"], "src/main.rs");
    assert_eq!(json["oldest"].as_array().unwrap().len(), 10);
    assert_eq!(json["extensions"][0]["label"], "img");
}

#[test]
fn extensions_are_grouped_case_insensitively_with_none_for_no_extension() {
    let f = Fixture::new();
    f.file("a.JPG", "12");
    f.file("b.jpg", "345");
    f.file("Makefile", "6");
    f.file(".bashrc", "78");
    let json = json_of(f.cmd().args(["stats", "--hidden"]), 0);
    let extensions = json["extensions"].as_array().unwrap();
    assert_eq!(extensions[0]["label"], "jpg");
    assert_eq!(extensions[0]["files"], 2);
    assert_eq!(extensions[0]["size"], 5);
    assert_eq!(extensions[1]["label"], "(none)");
    assert_eq!(extensions[1]["files"], 2, "Makefile and .bashrc");
}

#[test]
fn an_empty_directory_has_empty_statistics() {
    let f = Fixture::new();
    let out = stdout_of(f.cmd().args(["stats", "--no-color"]), 0);
    let files_line = out
        .lines()
        .find(|line| line.trim_start().starts_with("Files"))
        .unwrap();
    assert_eq!(
        files_line.split_whitespace().collect::<Vec<_>>(),
        ["Files", "0"]
    );
    assert!(
        out.contains("Top extensions by size\n  (no files)\n"),
        "{out}"
    );
    assert!(out.ends_with("Oldest files (UTC)\n  (no files)\n"), "{out}");
}
