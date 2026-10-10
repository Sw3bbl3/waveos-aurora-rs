//! Downloads a page's stylesheets and scripts side by side (a few
//! connections at once, like other browsers), so a page with many of them
//! costs about its slowest file rather than all of them in a row.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use corekit::sync::Mutex;
use corekit::thread::JoinHandle;
use nebula_web::http::Response;
use nebula_web::Url;

/// Connections a page's own files may use at once.
pub const PARALLEL: usize = 6;

struct Shared {
    queue: Mutex<VecDeque<Url>>,
    /// Finished downloads by requested URL (Err: why it failed).
    done: Mutex<BTreeMap<String, Result<Arc<Response>, String>>>,
    /// Everything ever requested, so waiting for an unknown URL returns.
    wanted: Mutex<Vec<String>>,
    cancel: AtomicBool,
}

pub struct Prefetch {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Prefetch {
    pub fn start(urls: Vec<Url>, fetch: fn(&Url) -> Result<Arc<Response>, String>) -> Prefetch {
        let mut seen = Vec::new();
        let mut queue = VecDeque::new();
        for u in urls {
            let k = u.to_string();
            if !seen.contains(&k) {
                seen.push(k);
                queue.push_back(u);
            }
        }
        let count = queue.len().min(PARALLEL);
        let shared = Arc::new(Shared {
            queue: Mutex::new(queue),
            done: Mutex::new(BTreeMap::new()),
            wanted: Mutex::new(seen),
            cancel: AtomicBool::new(false),
        });
        let mut workers = Vec::new();
        for _ in 0..count {
            let s = shared.clone();
            let spawned = corekit::thread::spawn(move || loop {
                if s.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let Some(u) = s.queue.lock().pop_front() else { return };
                let r = fetch(&u);
                s.done.lock().insert(u.to_string(), r);
            });
            match spawned {
                Ok(h) => workers.push(h),
                // No thread: the waiter fetches it itself.
                Err(_) => break,
            }
        }
        Prefetch { shared, workers }
    }

    /// The download for `url`, waiting for it if it's still on its way. If
    /// it was never requested (an @import, say), fetches it here.
    pub fn take(&self, url: &Url, fetch: fn(&Url) -> Result<Arc<Response>, String>) -> Result<Arc<Response>, String> {
        let k = url.to_string();
        if !self.shared.wanted.lock().contains(&k) || self.workers.is_empty() {
            return fetch(url);
        }
        loop {
            if let Some(r) = self.shared.done.lock().remove(&k) {
                return r;
            }
            // Still queued with every worker busy elsewhere: do it here.
            let mine = {
                let mut q = self.shared.queue.lock();
                q.iter().position(|u| u.to_string() == k).map(|i| q.remove(i).unwrap())
            };
            if let Some(u) = mine {
                return fetch(&u);
            }
            if self.shared.cancel.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
            corekit::time::sleep_ms(4);
        }
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.queue.lock().clear();
    }

    /// Waits for the workers (their stacks are freed only when joined).
    pub fn finish(self) {
        self.cancel();
        for w in self.workers {
            w.join();
        }
    }
}
