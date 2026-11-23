//! What the Store knows and does away from the interface thread: the
//! repositories' catalogues (cached ones at once, then fresh ones), a queue
//! of installs, updates and removals worked through one at a time (each
//! download pausable, resumable and cancellable), pictures fetched,
//! verified and decoded in the background, and the system update check.
//!
//! The interface reads [`Shared`] when it polls; nothing here touches it.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use corekit::apps::download::Control;
use corekit::apps::repo::{self, Catalog, Progress};
use corekit::apps::{self, Options, Scope};
use corekit::sync::Mutex;
use corekit::{thread, time};
use gina::repo::{Asset, Entry};

/// Every repository's apps, merged: the newest version of each.
#[derive(Clone, Default)]
pub struct Shelf {
    pub catalogs: Vec<Catalog>,
    /// Repositories that failed (address, why).
    pub errors: Vec<(String, String)>,
    /// (catalogue, entry), by name.
    pub apps: Vec<(usize, Entry)>,
    /// Featured app ids, in order.
    pub featured: Vec<String>,
    /// Some catalogue came from the cache because the network failed.
    pub offline: bool,
}

impl Shelf {
    fn build(catalogs: Vec<Catalog>, errors: Vec<(String, String)>) -> Shelf {
        let mut apps: Vec<(usize, Entry)> = Vec::new();
        let mut featured = Vec::new();
        for (ci, c) in catalogs.iter().enumerate() {
            for f in &c.index.featured {
                if !featured.contains(f) {
                    featured.push(f.clone());
                }
            }
            let mut ids: Vec<&str> = c.index.packages.iter().map(|e| e.id.as_str()).collect();
            ids.sort();
            ids.dedup();
            for id in ids {
                let Some(e) = c.index.latest(id) else { continue };
                match apps.iter_mut().find(|(_, a)| a.id == id) {
                    Some(slot) if gina::compare_versions(&e.version, &slot.1.version).is_gt() => *slot = (ci, e.clone()),
                    Some(_) => {}
                    None => apps.push((ci, e.clone())),
                }
            }
        }
        apps.sort_by_key(|(_, e)| e.name.to_lowercase());
        let offline = catalogs.iter().any(|c| c.cached);
        Shelf { catalogs, errors, apps, featured, offline }
    }

    pub fn app(&self, id: &str) -> Option<&(usize, Entry)> {
        self.apps.iter().find(|(_, e)| e.id == id)
    }

    /// Every version of `id` the repositories offer, newest first.
    pub fn versions(&self, id: &str) -> Vec<Entry> {
        let mut v: Vec<Entry> =
            self.catalogs.iter().flat_map(|c| c.index.versions(id)).cloned().collect();
        v.sort_by(|a, b| gina::compare_versions(&b.version, &a.version));
        v.dedup_by(|a, b| a.version == b.version);
        v
    }

    /// Categories that have apps, in the usual order.
    pub fn categories(&self) -> Vec<&'static str> {
        gina::CATEGORIES.iter().copied().filter(|c| self.apps.iter().any(|(_, e)| e.categories.iter().any(|x| x == c))).collect()
    }

    pub fn search(&self, term: &str) -> Vec<(usize, Entry)> {
        let mut out = Vec::new();
        for (ci, c) in self.catalogs.iter().enumerate() {
            for e in c.index.search(term) {
                if !out.iter().any(|(_, o): &(usize, Entry)| o.id == e.id) {
                    out.push((ci, e.clone()));
                }
            }
        }
        out.sort_by_key(|(_, e)| e.name.to_lowercase());
        out
    }
}

/// What a queued piece of work does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    Install(Scope),
    Remove { delete_data: bool },
    Rollback,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Queued,
    Downloading(u64, u64),
    Paused(u64, u64),
    Verifying,
    Installing(usize, usize),
    Done,
    Failed(String),
}

#[derive(Clone)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub work: Work,
    pub catalog: Option<Catalog>,
    pub entry: Option<Entry>,
    pub state: JobState,
    pub control: Arc<Control>,
    pub finished_at: u64,
}

impl Job {
    pub fn active(&self) -> bool {
        !matches!(self.state, JobState::Done | JobState::Failed(_))
    }
    /// 0.0–1.0 for a bar, None while unknown.
    pub fn fraction(&self) -> Option<f32> {
        match self.state {
            JobState::Downloading(n, t) | JobState::Paused(n, t) if t > 0 => Some(n as f32 / t as f32 * 0.9),
            JobState::Installing(n, t) if t > 0 => Some(0.9 + n as f32 / t as f32 * 0.1),
            JobState::Verifying => Some(0.9),
            _ => None,
        }
    }
}

/// A decoded picture, as plain pixels (it crosses threads).
pub type Picture = (u32, u32, Vec<u32>);

pub enum Pic {
    Loading,
    Ready(Picture),
    Failed,
}

/// The newer system a repository offers, as Software Update would show it.
#[derive(Clone, Default)]
pub enum SystemState {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        version: String,
        notes: String,
        download: u64,
    },
    Failed(String),
}

#[derive(Default)]
pub struct Shared {
    pub shelf: Option<Shelf>,
    /// Bumped when the shelf changes.
    pub shelf_gen: u64,
    pub loading: bool,
    pub jobs: Vec<Job>,
    pub jobs_gen: u64,
    pub pictures: BTreeMap<[u8; 32], Pic>,
    /// Pictures asked for, not yet fetched: (catalogue, picture, max size).
    pub wanted: Vec<(Catalog, Asset, u32)>,
    pub pictures_gen: u64,
    pub system: SystemState,
    pub system_gen: u64,
}

pub type State = Arc<Mutex<Shared>>;

pub fn new_state() -> State {
    Arc::new(Mutex::new(Shared::default()))
}

/// Loads the catalogues: what is cached first (the Store opens at once and
/// works offline), then fresh ones from the network.
pub fn refresh(state: &State) {
    {
        let mut s = state.lock();
        if s.loading {
            return;
        }
        s.loading = true;
    }
    let st = state.clone();
    let spawned = thread::spawn(move || {
        apps::recover();
        let sources = repo::sources();
        let cached: Vec<Catalog> = sources.iter().filter_map(|s| repo::cached(s)).collect();
        if !cached.is_empty() && st.lock().shelf.is_none() {
            let mut s = st.lock();
            s.shelf = Some(Shelf::build(cached, Vec::new()));
            s.shelf_gen += 1;
        }
        let mut catalogs = Vec::new();
        let mut errors = Vec::new();
        for (source, c) in repo::catalogs() {
            match c {
                Ok(c) => catalogs.push(c),
                Err(e) => errors.push((source, e)),
            }
        }
        let mut s = st.lock();
        s.shelf = Some(Shelf::build(catalogs, errors));
        s.shelf_gen += 1;
        s.loading = false;
    });
    if spawned.is_err() {
        state.lock().loading = false;
    }
}

/// Looks for a system update in the background.
pub fn check_system(state: &State) {
    {
        let mut s = state.lock();
        if matches!(s.system, SystemState::Checking) {
            return;
        }
        s.system = SystemState::Checking;
        s.system_gen += 1;
    }
    let st = state.clone();
    let _ = thread::spawn(move || {
        let result = match corekit::update::check(None) {
            Ok(Some(av)) => SystemState::Available {
                version: av.update.version.clone(),
                notes: av.update.notes.clone(),
                download: av.download_size(),
            },
            Ok(None) => SystemState::UpToDate,
            Err(e) => SystemState::Failed(e),
        };
        let mut s = st.lock();
        s.system = result;
        s.system_gen += 1;
    });
}

/// Queues work (an install, update, removal or rollback).
pub fn queue(state: &State, id: &str, name: &str, work: Work, source: Option<(Catalog, Entry)>) {
    let mut s = state.lock();
    s.jobs.retain(|j| j.id != id || j.active());
    if s.jobs.iter().any(|j| j.id == id && j.active()) {
        return;
    }
    let (catalog, entry) = source.map_or((None, None), |(c, e)| (Some(c), Some(e)));
    s.jobs.push(Job {
        id: id.into(),
        name: name.into(),
        work,
        catalog,
        entry,
        state: JobState::Queued,
        control: Arc::new(Control::new()),
        finished_at: 0,
    });
    s.jobs_gen += 1;
}

fn set(state: &State, id: &str, f: impl FnOnce(&mut Job)) {
    let mut s = state.lock();
    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id && j.active()) {
        f(j);
        s.jobs_gen += 1;
    }
}

/// Pauses a download (it resumes where it stopped).
pub fn pause(state: &State, id: &str) {
    set(state, id, |j| {
        j.control.pause();
        if j.state == JobState::Queued {
            j.state = JobState::Paused(0, j.entry.as_ref().map_or(0, |e| e.size));
        }
    });
}

pub fn resume(state: &State, id: &str) {
    set(state, id, |j| {
        j.control.resume();
        if matches!(j.state, JobState::Paused(..)) {
            j.state = JobState::Queued;
        }
    });
}

/// Cancels: a queued or paused job goes; a running download stops and its
/// partial file is deleted.
pub fn cancel(state: &State, id: &str) {
    let mut s = state.lock();
    if let Some(i) = s.jobs.iter().position(|j| j.id == id && j.active()) {
        let j = &s.jobs[i];
        j.control.cancel();
        if matches!(j.state, JobState::Queued | JobState::Paused(..)) {
            if let Some(e) = &j.entry {
                let _ = corekit::fs::remove(&alloc::format!("{}.part", repo::download_path(e)));
            }
            s.jobs.remove(i);
        }
        s.jobs_gen += 1;
    }
}

/// Starts the worker that does queued jobs one at a time.
pub fn start_worker(state: &State) {
    let st = state.clone();
    let _ = thread::spawn(move || loop {
        let next = {
            let mut s = st.lock();
            // Finished jobs are forgotten a while after.
            let now = time::uptime_ms();
            let before = s.jobs.len();
            s.jobs.retain(|j| j.active() || now < j.finished_at + 8000);
            if s.jobs.len() != before {
                s.jobs_gen += 1;
            }
            s.jobs.iter().find(|j| j.state == JobState::Queued).cloned()
        };
        match next {
            Some(job) => run(&st, job),
            None => time::sleep_ms(120),
        }
    });
}

fn run(state: &State, job: Job) {
    let id = job.id.clone();
    let result: Result<bool, String> = match &job.work {
        Work::Install(scope) => {
            let (Some(c), Some(e)) = (&job.catalog, &job.entry) else { return };
            // An update stays where the app is installed now.
            let scope = apps::installed(&e.id).map(|a| a.scope).unwrap_or(*scope);
            let opts = Options { scope, ..Options::default() };
            let st = state.clone();
            let progress = move |p: Progress| {
                let new = match p {
                    Progress::Downloading(n, t) => JobState::Downloading(n, t),
                    Progress::Verifying => JobState::Verifying,
                    Progress::Installing(n, t) => JobState::Installing(n, t),
                };
                set(&st, &e.id, |j| j.state = new);
            };
            set(state, &id, |j| j.state = JobState::Downloading(0, e.size));
            repo::install_entry(c, e, &opts, &job.control, progress).map(|done| done.is_some())
        }
        Work::Remove { delete_data } => apps::uninstall(&id, *delete_data).map(|_| true),
        Work::Rollback => apps::rollback(&id).map(|_| true),
    };
    let mut s = state.lock();
    let now = time::uptime_ms();
    if let Some(i) = s.jobs.iter().position(|j| j.id == id && j.active()) {
        let j = &mut s.jobs[i];
        match result {
            Ok(true) => {
                j.state = JobState::Done;
                j.finished_at = now;
            }
            Ok(false) if j.control.cancelled() => {
                s.jobs.remove(i);
            }
            Ok(false) => {
                let (n, t) = match j.state {
                    JobState::Downloading(n, t) => (n, t),
                    _ => (0, j.entry.as_ref().map_or(0, |e| e.size)),
                };
                j.state = JobState::Paused(n, t);
            }
            Err(e) => {
                j.state = JobState::Failed(e);
                j.finished_at = now + 60_000;
            }
        }
        s.jobs_gen += 1;
    }
}

/// Asks for a picture (fetched, checked and decoded in the background,
/// scaled to at most `max` pixels across).
pub fn want_picture(state: &State, catalog: &Catalog, asset: &Asset, max: u32) {
    let mut s = state.lock();
    if s.pictures.contains_key(&asset.sha256) {
        return;
    }
    s.pictures.insert(asset.sha256, Pic::Loading);
    s.wanted.push((catalog.clone(), asset.clone(), max));
}

/// Starts the picture loader: the newest request first (what was just
/// scrolled to), with a small memory cache in front of the disk cache.
pub fn start_pictures(state: &State) {
    let st = state.clone();
    let _ = thread::spawn(move || loop {
        let next = st.lock().wanted.pop();
        let Some((catalog, asset, max)) = next else {
            time::sleep_ms(60);
            continue;
        };
        let pic = repo::asset(&catalog, &asset)
            .ok()
            .and_then(|p| corekit::fs::read(&p).ok())
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| {
                let img = if img.width > max || img.height > max { img.thumbnail(max, max) } else { img };
                (img.width, img.height, img.pixels)
            });
        let mut s = st.lock();
        s.pictures.insert(asset.sha256, pic.map_or(Pic::Failed, Pic::Ready));
        s.pictures_gen += 1;
        // Keep at most 96 decoded pictures; the disk cache has the rest.
        if s.pictures.len() > 96 {
            let old: Vec<[u8; 32]> =
                s.pictures.iter().filter(|(_, p)| !matches!(p, Pic::Loading)).map(|(k, _)| *k).take(16).collect();
            for k in old {
                s.pictures.remove(&k);
            }
        }
    });
}

/// "12.3 MB"
pub fn human(bytes: u64) -> String {
    corekit::update::human(bytes)
}

/// "Game", "Productivity", …
pub fn category_name(c: &str) -> String {
    match c {
        "game" => "Games".to_string(),
        "utility" => "Utilities".to_string(),
        c => {
            let mut s = c.to_string();
            if let Some(f) = s.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            s
        }
    }
}
