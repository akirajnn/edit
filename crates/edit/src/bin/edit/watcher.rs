// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notices when open files are modified by something other than us.
//!
//! The editor's main loop parks in `sys::read_stdin` with no timeout, so it
//! can't poll for anything on its own. Rather than giving it a timeout and
//! waking once a second forever, a background thread does the polling and only
//! calls [`sys::wake`] when a file actually changed. Idle costs nothing.
//!
//! The thread only reports *that* something changed. Deciding what to do about
//! it needs the document state, which lives on the main thread, so that's where
//! the per-document comparison happens (see [`Document::changed_on_disk`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use edit::sys;

use crate::documents::DiskStamp;

/// How often the files are checked. The delay a user perceives is up to this.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Shared {
    /// The files to watch. Replaced wholesale by the main thread.
    paths: Mutex<Vec<PathBuf>>,
    /// Set by the watcher, cleared by the main thread.
    dirty: AtomicBool,
}

pub struct Watcher {
    shared: Arc<Shared>,
}

impl Watcher {
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();

        thread::spawn(move || {
            // The stamps the watcher itself last observed. Keeping them here
            // rather than reading the documents means the main thread never has
            // to hand anything back after a reload.
            let mut seen: Vec<(PathBuf, Option<DiskStamp>)> = Vec::new();

            loop {
                thread::sleep(POLL_INTERVAL);

                // Don't hold the lock across the file system calls below.
                let paths = {
                    let Ok(paths) = worker.paths.lock() else {
                        return;
                    };
                    paths.clone()
                };

                let mut changed = false;
                let mut next = Vec::with_capacity(paths.len());

                for path in paths {
                    let stamp = DiskStamp::of(&path);
                    match seen.iter().find(|(p, _)| *p == path) {
                        // A newly watched file isn't a change.
                        None => {}
                        Some((_, previous)) => changed |= *previous != stamp,
                    }
                    next.push((path, stamp));
                }

                seen = next;

                // Only wake the main loop on the transition, so that a change
                // it hasn't gotten around to yet doesn't wake it every second.
                if changed && !worker.dirty.swap(true, Ordering::Release) {
                    sys::wake();
                }
            }
        });

        Self { shared }
    }

    /// Replaces the set of watched files.
    ///
    /// This runs once per main loop iteration, so the unchanged case (which is
    /// nearly always) compares in place and doesn't allocate.
    pub fn set_paths<'a>(&self, paths: impl Iterator<Item = &'a Path> + Clone) {
        let Ok(mut current) = self.shared.paths.lock() else {
            return;
        };

        let unchanged = current.len() == paths.clone().count()
            && current.iter().zip(paths.clone()).all(|(a, b)| a == b);
        if unchanged {
            return;
        }

        current.clear();
        current.extend(paths.map(Path::to_path_buf));
    }

    /// Returns whether anything changed since the last call, and clears it.
    pub fn take_dirty(&self) -> bool {
        self.shared.dirty.swap(false, Ordering::Acquire)
    }
}
