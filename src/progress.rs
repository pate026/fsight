//! Progress reporting: lock-free counters that workers update, and a spinner
//! on stderr that displays them.
//!
//! The counters are plain atomics, so any number of threads can bump them
//! without locks. `Ordering::Relaxed` is enough: each counter is independent,
//! and the spinner only needs an approximately current value.

use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::output::{format_count, plural};
use crate::size::format_size;

/// Counters describing how far a long operation has come.
#[derive(Debug)]
pub struct Progress {
    items: AtomicU64,
    bytes: AtomicU64,
    stage: Mutex<&'static str>,
}

/// A copy of the counters at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// Entries visited, files searched or files hashed (depends on the stage).
    pub items: u64,
    /// Bytes counted or read so far.
    pub bytes: u64,
    /// What is happening, e.g. "Scanning".
    pub stage: &'static str,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            items: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            stage: Mutex::new("Scanning"),
        }
    }
}

impl Progress {
    /// Counts `n` more items.
    pub fn add_items(&self, n: u64) {
        self.items.fetch_add(n, Ordering::Relaxed);
    }

    /// Counts `n` more bytes.
    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Starts a new stage: sets its label and resets the counters.
    pub fn start_stage(&self, stage: &'static str) {
        // A poisoned mutex only means another thread panicked while holding
        // it; the `&str` inside is still valid, so keep using it.
        *self.stage.lock().unwrap_or_else(PoisonError::into_inner) = stage;
        self.items.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
    }

    /// Reads the current values.
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            items: self.items.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            stage: *self.stage.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }
}

impl ProgressSnapshot {
    /// One-line description, e.g. `Scanning · 12,345 entries · 3.4 GiB`.
    /// `noun` is the (singular, plural) name of the items being counted.
    pub fn describe(&self, noun: Noun) -> String {
        let mut text = format!(
            "{} · {} {}",
            self.stage,
            format_count(self.items),
            plural(self.items, noun.0, noun.1)
        );
        if self.bytes > 0 {
            text.push_str(" · ");
            text.push_str(&format_size(self.bytes));
        }
        text
    }
}

/// The singular and plural name of what is being counted, e.g. `("entry", "entries")`.
pub type Noun = (&'static str, &'static str);

/// Should a spinner be shown? Only on an interactive stderr, and not when the
/// user asked for silence, JSON, or log output (logs and spinner would mix).
pub fn spinner_enabled(quiet: bool, json: bool, verbose: u8) -> bool {
    !quiet && !json && verbose == 0 && io::stderr().is_terminal()
}

/// Runs `work` on the current thread while a spinner on stderr shows
/// `progress`. Returns whatever `work` returns.
///
/// The spinner runs on a scoped thread. It stops when the `done` channel is
/// disconnected, which happens when `done_tx` is dropped — after `work`
/// returns *or* if it panics, so the spinner can never outlive the work.
pub fn with_spinner<R>(
    enabled: bool,
    progress: &Progress,
    noun: Noun,
    work: impl FnOnce() -> R,
) -> R {
    if !enabled {
        return work();
    }
    let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(0);
    std::thread::scope(|scope| {
        scope.spawn(move || spin(progress, noun, &done_rx));
        let result = work();
        drop(done_tx);
        result
    })
}

fn spin(progress: &Progress, noun: Noun, done: &Receiver<()>) {
    let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr()).with_style(style);
    loop {
        bar.set_message(progress.snapshot().describe(noun));
        bar.tick();
        match done.recv_timeout(Duration::from_millis(100)) {
            Err(RecvTimeoutError::Timeout) => {}
            // Disconnected (or an unexpected message): the work is finished.
            _ => break,
        }
    }
    bar.finish_and_clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_add_up_across_threads_and_reset_per_stage() {
        let progress = Progress::default();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        progress.add_items(1);
                        progress.add_bytes(2);
                    }
                });
            }
        });
        let snap = progress.snapshot();
        assert_eq!(
            (snap.items, snap.bytes, snap.stage),
            (4000, 8000, "Scanning")
        );

        progress.start_stage("Hashing");
        assert_eq!(progress.snapshot().items, 0);
        assert_eq!(progress.snapshot().stage, "Hashing");
    }

    #[test]
    fn snapshot_descriptions_are_readable() {
        let snap = ProgressSnapshot {
            items: 12_345,
            bytes: 3 * 1024 * 1024,
            stage: "Scanning",
        };
        assert_eq!(
            snap.describe(("entry", "entries")),
            "Scanning · 12,345 entries · 3.0 MiB"
        );
        let one = ProgressSnapshot {
            items: 1,
            bytes: 0,
            stage: "Hashing",
        };
        assert_eq!(one.describe(("entry", "entries")), "Hashing · 1 entry");
    }

    #[test]
    fn disabled_spinner_just_runs_the_work() {
        let progress = Progress::default();
        assert_eq!(
            with_spinner(false, &progress, ("entry", "entries"), || 42),
            42
        );
    }
}
