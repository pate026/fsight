//! Helpers shared by the integration tests: build fixture directory trees
//! programmatically and run the `fsight` binary in an isolated environment.

// Each test binary includes this module but uses a different subset of it.
#![allow(dead_code)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use assert_cmd::Command;
use tempfile::TempDir;

/// Environment variables that could leak the developer's settings into tests.
const ISOLATED_VARS: [&str; 9] = [
    "FSIGHT_THREADS",
    "FSIGHT_HIDDEN",
    "FSIGHT_NO_IGNORE",
    "FSIGHT_COLOR",
    "FSIGHT_SCAN_DEPTH",
    "FSIGHT_SCAN_TOP",
    "FSIGHT_TUI_SORT",
    "NO_COLOR",
    "GIT_CONFIG_GLOBAL",
];

/// A temporary directory tree, deleted when dropped.
pub struct Fixture {
    root: TempDir,
    /// Empty directory used as `$HOME` and `$XDG_CONFIG_HOME` so that neither
    /// a real fsight config nor a global gitignore can affect the tests.
    home: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            root: TempDir::new().expect("create fixture directory"),
            home: TempDir::new().expect("create fake home directory"),
        }
    }

    /// Absolute path of the fixture root.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Absolute path of `rel` inside the fixture.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// The fake home directory (also used as `XDG_CONFIG_HOME`).
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Creates a directory (and its parents).
    pub fn dir(&self, rel: &str) -> PathBuf {
        let path = self.join(rel);
        fs::create_dir_all(&path).expect("create directory");
        path
    }

    /// Creates a file with the given contents (and its parent directories).
    pub fn file(&self, rel: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(&path, contents).expect("write file");
        path
    }

    /// Creates a file of exactly `size` bytes, all equal to `fill`.
    pub fn file_of_size(&self, rel: &str, size: usize, fill: u8) -> PathBuf {
        self.file(rel, vec![fill; size])
    }

    /// Creates a symbolic link at `link` pointing to `target` (taken literally).
    #[cfg(unix)]
    pub fn symlink(&self, target: &str, link: &str) -> PathBuf {
        let path = self.join(link);
        std::os::unix::fs::symlink(target, &path).expect("create symlink");
        path
    }

    /// Creates a hard link `link` to the existing file `original`.
    pub fn hard_link(&self, original: &str, link: &str) -> PathBuf {
        let path = self.join(link);
        fs::hard_link(self.join(original), &path).expect("create hard link");
        path
    }

    /// Sets the modification time of `rel`.
    pub fn set_modified(&self, rel: &str, time: SystemTime) {
        File::options()
            .write(true)
            .open(self.join(rel))
            .and_then(|file| file.set_modified(time))
            .expect("set modification time");
    }

    /// Sets the modification time of `rel` to `age` before now.
    pub fn set_age(&self, rel: &str, age: Duration) {
        self.set_modified(rel, SystemTime::now() - age);
    }

    /// A `fsight` command that runs inside the fixture with a clean environment.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fsight"));
        cmd.current_dir(self.path())
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home())
            .timeout(Duration::from_secs(60));
        for var in ISOLATED_VARS {
            cmd.env_remove(var);
        }
        cmd
    }

    /// Writes `contents` to the fake `$XDG_CONFIG_HOME/fsight/config.toml`.
    pub fn write_config(&self, contents: &str) -> PathBuf {
        let dir = self.home().join("fsight");
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.toml");
        fs::write(&path, contents).expect("write config file");
        path
    }
}

/// Runs `cmd`, asserts the exit code and returns stdout as a string.
pub fn stdout_of(cmd: &mut Command, expected_code: i32) -> String {
    let output = cmd.output().expect("run fsight");
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "unexpected exit status\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs `cmd` with `--json`, asserts success and parses stdout as JSON.
pub fn json_of(cmd: &mut Command, expected_code: i32) -> serde_json::Value {
    let text = stdout_of(cmd.arg("--json"), expected_code);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("invalid JSON ({err}):\n{text}"))
}

/// The lines of `text`, sorted (for output whose order does not matter).
pub fn sorted_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines
}

/// Temporarily removes all permissions from a directory; restores them on drop
/// so the temporary directory can still be deleted.
#[cfg(unix)]
pub struct Unreadable {
    path: PathBuf,
}

#[cfg(unix)]
impl Unreadable {
    /// Makes `path` unreadable. Returns `None` if the current user can read it
    /// anyway (for example when the tests run as root), in which case tests
    /// about unreadable directories should be skipped.
    pub fn new(path: PathBuf) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let guard = Self { path };
        if fs::read_dir(&guard.path).is_ok() {
            eprintln!(
                "note: running with elevated privileges, skipping unreadable-directory checks"
            );
            return None;
        }
        Some(guard)
    }
}

#[cfg(unix)]
impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755));
    }
}
