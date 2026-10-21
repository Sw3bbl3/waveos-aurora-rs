//! Native media playback. Every source is a set of coded frames in the
//! document: a Media Source Extensions object a page feeds (YouTube), or an
//! MP4 file Nebula downloads and demuxes itself (`<video src=…mp4>`). Frames
//! are decoded on worker threads and presented against the audio clock.
use super::*;
use nebula_engine::media::{CodedFrame, MediaSource, ReadyState, TrackKind};

/// How far ahead of the playback position frames are handed to decoders.
/// Compressed frames are small: hand over seconds of them, so decoding and
/// sound go on while page scripts keep the browser's own thread busy.
const VIDEO_AHEAD: i64 = 3_000_000;
const AUDIO_AHEAD: i64 = 30_000_000;
/// Decoded pictures waiting to be shown, per player (by memory).
const PICTURE_BYTES: usize = 24 << 20;
/// Ids for sources made from files (scripts' own count up from 1).
const FILE_SOURCE: u32 = 0x8000_0000;

struct Picture {
    epoch: u64,
    pts: i64,
    duration: i64,
    /// Scaled to the size it was last shown at.
    image: Image,
    /// The coded picture's own size (for the aspect ratio).
    native: (u32, u32),
}

#[derive(Default)]
struct VideoShared {
    input: VecDeque<CodedFrame>,
    output: VecDeque<Picture>,
    epoch: u64,
    /// Pictures presented before this are decoded but not output.
    skip_before: i64,
    /// Non-reference frames presented before this aren't decoded (late).
    late_before: i64,
    /// The source has no more frames: output what is held back.
    drain: bool,
    stop: bool,
    error: Option<String>,
    decoded: u64,
    dropped: u64,
    /// The size pictures are shown at (0: their own).
    target: (u32, u32),
}

#[derive(Default)]
struct AudioShared {
    input: VecDeque<CodedFrame>,
    epoch: u64,
    skip_before: i64,
    stop: bool,
    paused: bool,
    muted: bool,
    volume: f64,
    /// Presentation time of the first sample written in this epoch.
    base: Option<i64>,
    /// Samples (per channel) written in this epoch.
    written: u64,
    output: Option<Arc<corekit::audio::Stream>>,
    error: Option<String>,
}

struct Pipe<S> {
    shared: Arc<Mutex<S>>,
    worker: Option<JoinHandle<()>>,
    /// Decode time of the last frame handed over (None: find a start).
    cursor: Option<i64>,
    revision: u64,
}

impl<S> Pipe<S> {
    fn retire(&mut self, retired: &mut Vec<JoinHandle<()>>) {
        if let Some(w) = self.worker.take() {
            retired.push(w);
        }
    }
}

type LoadResult = Arc<Mutex<Option<Result<MediaSource, String>>>>;

struct Player {
    src: String,
    revision: u64,
    /// The MediaSource feeding it (None while a file downloads).
    source: Option<u32>,
    loader: Option<(JoinHandle<()>, LoadResult)>,
    video: Option<Pipe<VideoShared>>,
    audio: Option<Pipe<AudioShared>>,
    epoch: u64,
    /// The playback position (µs).
    position: i64,
    /// Wall-clock anchor (uptime ms, position) when there is no audio clock.
    anchor: Option<(u64, i64)>,
    frame: Option<Picture>,
    metadata: bool,
    loaded: bool,
    playing: bool,
    waiting: bool,
    ended: bool,
    last_update: u64,
    last_log: u64,
    stalled_since: Option<u64>,
    /// The size the last picture was painted at.
    display: core::cell::Cell<(u32, u32)>,
}

impl Player {
    fn new(src: String, revision: u64) -> Player {
        Player {
            src,
            revision,
            source: None,
            loader: None,
            video: None,
            audio: None,
            epoch: 0,
            position: 0,
            anchor: None,
            frame: None,
            metadata: false,
            loaded: false,
            playing: false,
            waiting: false,
            ended: false,
            last_update: 0,
            last_log: 0,
            stalled_since: None,
            display: core::cell::Cell::new((0, 0)),
        }
    }

    fn stop(&mut self, retired: &mut Vec<JoinHandle<()>>) {
        if let Some(v) = &mut self.video {
            v.shared.lock().stop = true;
            v.retire(retired);
        }
        if let Some(a) = &mut self.audio {
            let mut s = a.shared.lock();
            s.stop = true;
            if let Some(o) = &s.output {
                let _ = o.pause(true);
                let _ = o.flush();
            }
            drop(s);
            a.retire(retired);
        }
        if let Some((w, _)) = self.loader.take() {
            retired.push(w);
        }
    }

    /// Starts decoding over from `position` (a seek, or frames replaced).
    fn restart(&mut self, position: i64) {
        self.epoch += 1;
        self.position = position;
        self.frame = None;
        self.anchor = None;
        if let Some(v) = &mut self.video {
            let mut s = v.shared.lock();
            s.epoch = self.epoch;
            s.input.clear();
            s.output.clear();
            s.skip_before = position;
            s.late_before = position;
            s.drain = false;
            v.cursor = None;
        }
        if let Some(a) = &mut self.audio {
            let mut s = a.shared.lock();
            s.epoch = self.epoch;
            s.input.clear();
            s.skip_before = position;
            s.base = None;
            s.written = 0;
            if let Some(o) = &s.output {
                let _ = o.pause(true);
                let _ = o.flush();
            }
            a.cursor = None;
        }
    }

    /// The audio clock, once samples of this epoch are playing.
    fn audio_clock(&self) -> Option<i64> {
        let a = self.audio.as_ref()?;
        let s = a.shared.lock();
        if s.epoch != self.epoch || s.error.is_some() {
            return None;
        }
        let base = s.base?;
        let played = s.output.as_ref()?.position().ok()?.min(s.written);
        Some(base + (played as i64) * 1_000_000 / 48_000)
    }
}

#[derive(Default)]
pub struct Players {
    players: BTreeMap<NodeId, Player>,
    next_file: u32,
    /// A new picture is showing (the window needs painting).
    presented: bool,
}

impl Players {
    pub fn clear(&mut self, retired: &mut Vec<JoinHandle<()>>) {
        for (_, mut p) in core::mem::take(&mut self.players) {
            p.stop(retired);
        }
    }

    pub fn active(&self) -> bool {
        !self.players.is_empty()
    }

    /// Whether a new picture appeared since the last call.
    pub fn take_presented(&mut self) -> bool {
        core::mem::take(&mut self.presented)
    }

    pub fn tick(
        &mut self,
        page: &mut Page,
        base: &Url,
        _now: u64,
        retired: &mut Vec<JoinHandle<()>>,
    ) -> Vec<(NodeId, &'static str)> {
        let now = corekit::time::uptime_ms();
        let mut events = Vec::new();
        let nodes: Vec<_> = page
            .doc
            .descendants(Document::ROOT)
            .into_iter()
            .filter(|&n| matches!(page.doc.tag(n), "video" | "audio"))
            .collect();
        let gone: Vec<_> = self.players.keys().copied().filter(|n| !nodes.contains(n)).collect();
        for n in gone {
            if let Some(mut p) = self.players.remove(&n) {
                p.stop(retired);
                if p.source.is_some_and(|id| id >= FILE_SOURCE) {
                    page.doc.media_sources.remove(&p.source.unwrap());
                }
            }
        }
        for n in nodes {
            let e = page.doc.element(n).unwrap();
            let attr_src = e.attr("src").or_else(|| {
                page.doc.nodes[n]
                    .children
                    .iter()
                    .find_map(|&c| page.doc.element(c).filter(|e| e.tag == "source").and_then(|e| e.attr("src")))
            });
            let src = attr_src
                .map(|s| {
                    if s.starts_with("blob:") {
                        String::from(s)
                    } else {
                        base.join(s).map(|u| u.to_string()).unwrap_or_default()
                    }
                })
                .unwrap_or_default();
            let autoplay = e.attr("autoplay").is_some();
            let looping = e.attr("loop").is_some();
            let default_muted = e.attr("muted").is_some();
            let state = page.doc.media.entry(n).or_default();
            let script_source = state.source.filter(|&id| id < FILE_SOURCE);
            let revision = state.revision;
            // A new source (or load()) replaces the player.
            if self.players.get(&n).is_some_and(|p| {
                p.src != src || p.revision != revision || (script_source.is_some() && p.source != script_source)
            }) {
                let mut p = self.players.remove(&n).unwrap();
                p.stop(retired);
                if let Some(id) = p.source.filter(|&id| id >= FILE_SOURCE) {
                    page.doc.media_sources.remove(&id);
                    if page.doc.media.get(&n).is_some_and(|s| s.source == Some(id)) {
                        page.doc.media.get_mut(&n).unwrap().source = None;
                    }
                }
            }
            let state = page.doc.media.get_mut(&n).unwrap();
            if src.is_empty() && script_source.is_none() {
                continue;
            }
            if !self.players.contains_key(&n) {
                if self.players.len() >= 4 {
                    if state.error.is_none() {
                        state.error = Some(String::from("Too many simultaneous videos"));
                        events.push((n, "error"));
                    }
                    continue;
                }
                let mut p = Player::new(src.clone(), revision);
                state.src = src.clone();
                if let Some(id) = script_source {
                    p.source = Some(id);
                } else if src.starts_with("blob:") {
                    state.error = Some(String::from("This media URL isn't available"));
                    events.push((n, "error"));
                    continue;
                } else {
                    // A file: download and demux it in the background.
                    let result: LoadResult = Arc::new(Mutex::new(None));
                    let (out, url) = (result.clone(), src.clone());
                    self.next_file += 1;
                    let id = FILE_SOURCE + self.next_file;
                    match corekit::thread::spawn(move || {
                        let r = load_file(&url, id);
                        *out.lock() = Some(r);
                    }) {
                        Ok(w) => p.loader = Some((w, result)),
                        Err(_) => {
                            state.error = Some(String::from("Could not start the media loader"));
                            events.push((n, "error"));
                            continue;
                        }
                    }
                    events.push((n, "loadstart"));
                }
                if autoplay && state.paused {
                    state.paused = false;
                    events.push((n, "play"));
                }
                self.players.insert(n, p);
            }
            let p = self.players.get_mut(&n).unwrap();
            // A downloaded file becomes the element's source.
            if let Some((_, result)) = &p.loader {
                let done = result.lock().take();
                match done {
                    Some(Ok(ms)) => {
                        let id = ms.id;
                        page.doc.media_sources.insert(id, ms);
                        let state = page.doc.media.get_mut(&n).unwrap();
                        state.source = Some(id);
                        p.source = Some(id);
                        let (w, _) = p.loader.take().unwrap();
                        retired.push(w);
                    }
                    Some(Err(e)) => {
                        corekit::io::log(&format!("[nebula] media error: {e}\n"));
                        let state = page.doc.media.get_mut(&n).unwrap();
                        state.error = Some(e);
                        state.paused = true;
                        events.push((n, "error"));
                        let (w, _) = p.loader.take().unwrap();
                        retired.push(w);
                        continue;
                    }
                    None => continue,
                }
            }
            let Some(id) = p.source else { continue };
            let doc = &mut page.doc;
            let Some(ms) = doc.media_sources.get(&id) else { continue };
            let state = doc.media.get_mut(&n).unwrap();
            if state.error.is_some() {
                continue;
            }
            let before = events.len();
            let shown = p.frame.as_ref().map(|f| (f.epoch, f.pts));
            step(p, ms, state, n, now, looping, default_muted, &mut events);
            self.presented |= p.frame.as_ref().map(|f| (f.epoch, f.pts)) != shown;
            for (_, e) in &events[before..] {
                if matches!(*e, "waiting" | "playing" | "ended" | "seeking" | "seeked" | "loadedmetadata" | "canplay") {
                    let (cursor, input, out, decoded) = p.video.as_ref().map_or((None, 0, 0, 0), |v| {
                        let sh = v.shared.lock();
                        (v.cursor, sh.input.len(), sh.output.len(), sh.decoded)
                    });
                    corekit::io::log(&format!(
                        "[nebula] media {e} at {:.2}s epoch={} video cursor={cursor:?} input={input} pictures={out} decoded={decoded} buffered={:?}\n",
                        p.position as f64 / 1e6,
                        p.epoch,
                        ms.element_buffered()
                    ));
                }
            }
        }
        events
    }

    pub fn paint(&self, cv: &mut Canvas, rect: Rect, node: NodeId, page: &Page) {
        cv.fill_rect(rect, 0xFF000000);
        let p = self.players.get(&node);
        if let (Some(p), Some(f)) = (p, p.and_then(|p| p.frame.as_ref())) {
            let (nw, nh) = (f.native.0.max(1) as f64, f.native.1.max(1) as f64);
            let scale = (rect.w as f64 / nw).min(rect.h as f64 / nh);
            let (w, h) = ((nw * scale) as i32, (nh * scale) as i32);
            let (x, y) = (rect.x + (rect.w - w) / 2, rect.y + (rect.h - h) / 2);
            // Decoders scale pictures to this size from now on.
            p.display.set((w.max(1) as u32, h.max(1) as u32));
            if (f.image.width as i32, f.image.height as i32) == (w, h) {
                cv.blit_opaque(&f.image.pixels, f.image.width, f.image.height, x, y);
            } else {
                cv.draw_image(&f.image.pixels, f.image.width, f.image.height, Rect::new(x, y, w, h), 255);
            }
        }
        let s = page.doc.media.get(&node);
        let error = s.and_then(|s| s.error.as_deref());
        let poster = page.doc.element(node).and_then(|e| e.attr("poster")).is_some();
        if let Some(e) = error {
            cv.text_clipped(rect.x + 12, rect.y + 26, e, theme::ui(14), 0xFFFFFFFF, rect.w - 24);
        } else if p.is_some_and(|p| p.frame.is_none() && (p.loader.is_some() || p.waiting)) && !poster {
            cv.text_clipped(rect.x + 12, rect.y + 26, "Loading video…", theme::ui(14), 0xFFFFFFFF, rect.w - 24);
        }
        if page.doc.element(node).is_some_and(|e| e.attr("controls").is_some()) {
            let r = Rect::new(rect.x, rect.bottom() - 32, rect.w, 32);
            cv.fill_rect(r, 0xE8202028);
            let paused = s.is_none_or(|s| s.paused);
            cv.text(r.x + 10, r.y + 21, if paused { "Play" } else { "Pause" }, theme::ui(13), 0xFFFFFFFF);
            if let Some(s) = s {
                let duration =
                    s.source.and_then(|id| page.doc.media_sources.get(&id)).map_or(s.duration, |m| m.duration);
                let label = format!("{} / {}", clock(s.current_time), clock(duration));
                cv.text_clipped(r.x + 70, r.y + 21, &label, theme::ui(12), 0xFFFFFFFF, (r.w - 90).max(0));
                if duration > 0.0 && duration.is_finite() {
                    cv.fill_rect(Rect::new(r.x, r.y, (r.w as f64 * s.current_time / duration) as i32, 3), 0xFF70ABFF);
                }
            }
        }
    }

    /// The duration the native controls seek within.
    pub fn duration(&self, page: &Page, node: NodeId) -> f64 {
        let s = page.doc.media.get(&node);
        s.and_then(|s| s.source)
            .and_then(|id| page.doc.media_sources.get(&id))
            .map_or(s.map_or(f64::NAN, |s| s.duration), |m| m.duration)
    }
}

fn clock(t: f64) -> String {
    if !t.is_finite() {
        return String::from("--:--");
    }
    let t = t as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// One frame of playback for an element: metadata, seeking, feeding the
/// decoders, the clock, presentation and the events scripts see.
#[allow(clippy::too_many_arguments)]
fn step(
    p: &mut Player,
    ms: &MediaSource,
    s: &mut nebula_engine::media::MediaState,
    n: NodeId,
    now: u64,
    looping: bool,
    default_muted: bool,
    events: &mut Vec<(NodeId, &'static str)>,
) {
    if !p.metadata {
        if !ms.has_metadata() {
            return;
        }
        p.metadata = true;
        if let Some(v) = ms.track(TrackKind::Video) {
            s.width = v.width;
            s.height = v.height;
            p.video = spawn_pipe(video_worker).ok();
        }
        if ms.track(TrackKind::Audio).is_some() {
            p.audio = spawn_pipe(audio_worker).ok();
        }
        s.duration = ms.duration;
        s.ready_state = 1;
        p.restart((s.current_time * 1e6) as i64);
        events.extend([(n, "durationchange"), (n, "loadedmetadata")]);
    }
    if let Some(t) = s.seek.take() {
        p.restart((t * 1e6) as i64);
        s.current_time = t;
        s.seeking = true;
        s.ended = false;
        p.ended = false;
        s.ready_state = s.ready_state.min(1);
        events.push((n, "seeking"));
    }
    // Frames the page removed or replaced under the decoders: start over.
    let mut lost = false;
    if let (Some(v), Some(track)) = (&mut p.video, ms.track(TrackKind::Video)) {
        if v.revision != track.revision {
            v.revision = track.revision;
            lost |= v.cursor.is_some_and(|c| !track.frames.contains_key(&c));
        }
    }
    if let (Some(a), Some(track)) = (&mut p.audio, ms.track(TrackKind::Audio)) {
        if a.revision != track.revision {
            a.revision = track.revision;
            lost |= a.cursor.is_some_and(|c| !track.frames.contains_key(&c));
        }
    }
    if lost {
        let at = p.position;
        p.restart(at);
    }
    let buffered = ms.element_buffered();
    let position_s = p.position as f64 / 1e6;
    // Small gaps — a first frame shown slightly late, a seam between
    // segments — are skipped, as other players do.
    if !buffered.iter().any(|&(a, b)| position_s + 0.001 >= a && position_s < b) {
        if let Some(&(a, _)) = buffered.iter().find(|&&(a, _)| a > position_s && a - position_s < 0.5) {
            let at = (a * 1e6) as i64 + 1;
            if p.frame.is_none() {
                p.position = at;
            } else {
                p.restart(at);
            }
        }
    }
    let ended_stream = ms.ready == ReadyState::Ended;
    feed(p, ms, ended_stream);
    // The clock.
    let playing_clock = !s.paused && s.ready_state >= 3 && !p.waiting;
    if let Some(a) = &p.audio {
        let mut sh = a.shared.lock();
        sh.paused = !playing_clock;
        sh.muted = s.muted.unwrap_or(default_muted);
        sh.volume = s.volume;
        if let Some(o) = &sh.output {
            let _ = o.pause(!playing_clock);
        }
    }
    if playing_clock {
        let audio = p.audio_clock();
        match audio {
            Some(t) if !p.audio_finished(ms) => {
                p.position = t.max(p.position);
                p.anchor = None;
            }
            _ => {
                let (since, from) = *p.anchor.get_or_insert((now, p.position));
                p.position = from + ((now.saturating_sub(since)) as f64 * 1000.0 * s.playback_rate) as i64;
            }
        }
    } else {
        p.anchor = None;
    }
    // Presentation: the latest decoded picture that is due.
    if let Some(v) = &p.video {
        let mut sh = v.shared.lock();
        if let Some(e) = sh.error.take() {
            corekit::io::log(&format!("[nebula] video decode error: {e}\n"));
        }
        while let Some(front) = sh.output.front() {
            if front.epoch != p.epoch {
                sh.output.pop_front();
                continue;
            }
            let due = front.pts <= p.position || p.frame.is_none();
            if !due {
                break;
            }
            p.frame = sh.output.pop_front();
            if !playing_clock {
                break;
            }
        }
        // Late non-reference frames needn't be decoded.
        sh.late_before = p.position - 80_000;
        sh.target = p.display.get();
    }
    // Ready states, from what is buffered and decoded at the position.
    let position_s = p.position as f64 / 1e6;
    let here = buffered.iter().find(|&&(a, b)| position_s + 0.001 >= a && position_s < b);
    let ahead = here.map_or(0.0, |&(_, b)| b - position_s);
    let picture = p.video.is_none() || p.frame.is_some();
    let at_end = ended_stream && buffered.last().is_none_or(|&(_, b)| position_s >= b - 0.05);
    let have_data = (here.is_some() || (at_end && p.frame.is_some())) && picture;
    if have_data && s.ready_state < 3 {
        s.ready_state = if ahead > 5.0 || ended_stream { 4 } else { 3 };
        if !p.loaded {
            p.loaded = true;
            events.push((n, "loadeddata"));
        }
        events.push((n, "canplay"));
        if s.ready_state == 4 {
            events.push((n, "canplaythrough"));
        }
        if s.seeking {
            s.seeking = false;
            s.current_time = position_s;
            events.extend([(n, "timeupdate"), (n, "seeked")]);
        }
    } else if have_data && s.ready_state == 3 && (ahead > 5.0 || ended_stream) {
        s.ready_state = 4;
        events.push((n, "canplaythrough"));
    }
    if p.waiting && have_data {
        p.waiting = false;
        p.stalled_since = None;
    }
    // "playing" whenever playback starts or resumes (play() fired "play").
    if !s.paused && s.ready_state >= 3 && !p.waiting {
        if !p.playing {
            p.playing = true;
            events.push((n, "playing"));
        }
    } else {
        p.playing = false;
    }
    if !s.paused && p.playing && s.ready_state >= 3 {
        // The end.
        let duration = if ms.duration.is_finite() { ms.duration } else { buffered.last().map_or(0.0, |r| r.1) };
        if ended_stream && position_s >= duration.min(buffered.last().map_or(duration, |r| r.1)) - 0.03 {
            s.current_time = duration;
            if looping {
                s.seek = Some(0.0);
            } else if !p.ended {
                p.ended = true;
                s.ended = true;
                s.paused = true;
                p.playing = false;
                events.extend([(n, "timeupdate"), (n, "pause"), (n, "ended")]);
            }
            return;
        }
        // Out of data: wait for more (scripts append it, or the player catches up).
        // Only missing data stalls playback; when decoding falls behind,
        // late pictures are dropped and the sound plays on.
        let starving = here.is_none();
        if starving && !p.waiting {
            let since = *p.stalled_since.get_or_insert(now);
            if here.is_none() || now - since > 250 {
                p.waiting = true;
                p.playing = false;
                s.ready_state = 2;
                events.push((n, "waiting"));
            }
        } else if !starving {
            p.stalled_since = None;
        }
        s.current_time = position_s;
        if now - p.last_log >= 5000 {
            p.last_log = now;
            let (decoded, dropped, queued) = p.video.as_ref().map_or((0, 0, 0), |v| {
                let sh = v.shared.lock();
                (sh.decoded, sh.dropped, sh.output.len())
            });
            corekit::io::log(&format!(
                "[nebula] media {:.1}s of {:.1}s: decoded={decoded} dropped={dropped} pictures={queued} ahead={ahead:.1}s audio={}\n",
                position_s,
                ms.duration,
                p.audio_clock().is_some()
            ));
        }
        if now - p.last_update >= 250 {
            p.last_update = now;
            events.push((n, "timeupdate"));
        }
    }
}

impl Player {
    /// Whether all the audio there is has been played (the clock then runs on).
    fn audio_finished(&self, ms: &MediaSource) -> bool {
        let Some(a) = &self.audio else { return true };
        let Some(track) = ms.track(TrackKind::Audio) else { return true };
        let last = track.frames.values().map(|f| f.pts + f.duration).max().unwrap_or(0);
        let sh = a.shared.lock();
        sh.input.is_empty()
            && a.cursor.is_some_and(|c| track.frames.range(c + 1..).next().is_none())
            && self.position >= last - 50_000
    }
}

/// Hands frames ahead of the position to the decoders, in decode order.
fn feed(p: &mut Player, ms: &MediaSource, ended: bool) {
    let position = p.position;
    if let (Some(v), Some(track)) = (&mut p.video, ms.track(TrackKind::Video)) {
        if v.cursor.is_none() {
            // The keyframe at or before the position (or the first after it).
            let start = track
                .frames
                .values()
                .filter(|f| f.sync && f.pts <= position + 1000)
                .max_by_key(|f| f.pts)
                .or_else(|| track.frames.values().filter(|f| f.sync && f.pts > position).min_by_key(|f| f.pts));
            if let Some(f) = start {
                v.cursor = Some(f.dts - 1);
                v.revision = track.revision;
            }
        }
        if let Some(cursor) = v.cursor {
            let mut sh = v.shared.lock();
            let mut last = cursor;
            for (&dts, f) in track.frames.range(cursor + 1..) {
                if sh.input.len() >= 180 || f.pts > position + VIDEO_AHEAD && sh.input.len() + sh.output.len() >= 2 {
                    break;
                }
                sh.input.push_back(f.clone());
                last = dts;
            }
            v.cursor = Some(last);
            sh.drain = ended && track.frames.range(last + 1..).next().is_none();
        }
    }
    if let (Some(a), Some(track)) = (&mut p.audio, ms.track(TrackKind::Audio)) {
        if a.cursor.is_none() {
            if let Some(f) = track.frames.values().filter(|f| f.pts + f.duration > position).min_by_key(|f| f.pts) {
                a.cursor = Some(f.dts - 1);
                a.revision = track.revision;
            }
        }
        if let Some(cursor) = a.cursor {
            let mut sh = a.shared.lock();
            let mut last = cursor;
            for (&dts, f) in track.frames.range(cursor + 1..) {
                if sh.input.len() >= 2048 || f.pts > position + AUDIO_AHEAD {
                    break;
                }
                sh.input.push_back(f.clone());
                last = dts;
            }
            a.cursor = Some(last);
        }
    }
}

fn spawn_pipe<S: Default + Send + 'static>(worker: fn(Arc<Mutex<S>>)) -> Result<Pipe<S>, ()> {
    let shared = Arc::new(Mutex::new(S::default()));
    let s = shared.clone();
    let w = corekit::thread::spawn(move || worker(s)).map_err(|_| ())?;
    Ok(Pipe { shared, worker: Some(w), cursor: None, revision: 0 })
}

fn video_worker(shared: Arc<Mutex<VideoShared>>) {
    let mut decoder: Option<(Arc<[u8]>, nebula_media::VideoDecoder)> = None;
    let mut epoch = u64::MAX;
    // Decoded pictures held until no earlier one can follow (B-frames).
    let mut held: Vec<Picture> = Vec::new();
    let mut depth = 2usize;
    let mut last_out = i64::MIN;
    loop {
        let (frame, skip, late, drain) = {
            let mut s = shared.lock();
            if s.stop {
                return;
            }
            if s.epoch != epoch {
                epoch = s.epoch;
                held.clear();
                decoder = None;
                last_out = i64::MIN;
            }
            let room = s.output.iter().map(|p| p.image.pixels.len() * 4).sum::<usize>() < PICTURE_BYTES;
            let frame = if room { s.input.pop_front() } else { None };
            (frame, s.skip_before, s.late_before, s.drain && s.input.is_empty())
        };
        let Some(frame) = frame else {
            if drain && !held.is_empty() {
                held.sort_by_key(|p| p.pts);
                let mut s = shared.lock();
                if s.epoch == epoch && s.output.len() < 64 {
                    let p = held.remove(0);
                    last_out = p.pts;
                    s.output.push_back(p);
                    continue;
                }
            }
            corekit::time::sleep_ms(4);
            continue;
        };
        // A frame nothing refers to, already late: skip it.
        if frame.pts + frame.duration < late && !frame.sync && !is_reference(&frame.data) {
            shared.lock().dropped += 1;
            continue;
        }
        if decoder.as_ref().is_none_or(|(c, _)| !Arc::ptr_eq(c, &frame.config) && c[..] != frame.config[..]) {
            if !frame.sync {
                continue;
            }
            match nebula_media::VideoDecoder::new(&frame.config) {
                Ok(d) => decoder = Some((frame.config.clone(), d)),
                Err(e) => {
                    shared.lock().error = Some(String::from(e));
                    decoder = None;
                    continue;
                }
            }
        }
        let (_, d) = decoder.as_mut().unwrap();
        let picture = match d.decode(&frame.data) {
            Ok(Some(yuv)) => yuv,
            Ok(None) => continue,
            Err(e) => {
                shared.lock().error = Some(String::from(e));
                // Wait for the next keyframe.
                decoder = None;
                continue;
            }
        };
        shared.lock().decoded += 1;
        if frame.pts + frame.duration <= skip {
            continue;
        }
        let target = shared.lock().target;
        let (tw, th) =
            if target.0 > 0 && target.1 > 0 { target } else { (picture.width as u32, picture.height as u32) };
        let Ok(pixels) = nebula_media::pixels_scaled(&picture, tw as usize, th as usize) else { continue };
        held.push(Picture {
            epoch,
            pts: frame.pts,
            duration: frame.duration,
            image: Image { width: tw, height: th, pixels },
            native: (picture.width as u32, picture.height as u32),
        });
        while held.len() > depth {
            held.sort_by_key(|p| p.pts);
            let p = held.remove(0);
            if p.pts < last_out {
                // Shown out of order: hold more back from now on.
                depth = (depth + 1).min(16);
                continue;
            }
            last_out = p.pts;
            let mut s = shared.lock();
            if s.epoch != epoch {
                break;
            }
            s.output.push_back(p);
        }
    }
}

/// Whether an AVC access unit (length-prefixed NALs) is used for reference.
fn is_reference(data: &[u8]) -> bool {
    let mut at = 0;
    while at + 5 <= data.len() {
        let len = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let header = data[at + 4];
        if matches!(header & 31, 1 | 5) {
            return header & 0x60 != 0;
        }
        at += 4 + len;
    }
    true
}

fn audio_worker(shared: Arc<Mutex<AudioShared>>) {
    let output = match corekit::audio::Stream::open() {
        Ok(o) => Arc::new(o),
        Err(_) => {
            shared.lock().error = Some(String::from("No audio output device"));
            return;
        }
    };
    let _ = output.pause(true);
    shared.lock().output = Some(output.clone());
    let mut decoder: Option<(Arc<[u8]>, nebula_media::AudioDecoder)> = None;
    let mut epoch = u64::MAX;
    loop {
        let (frame, skip) = {
            let mut s = shared.lock();
            if s.stop {
                let _ = output.flush();
                return;
            }
            if s.epoch != epoch {
                epoch = s.epoch;
                decoder = None;
            }
            (s.input.pop_front(), s.skip_before)
        };
        let Some(frame) = frame else {
            corekit::time::sleep_ms(5);
            continue;
        };
        if decoder.as_ref().is_none_or(|(c, _)| c[..] != frame.config[..]) {
            match nebula_media::AudioDecoder::new(&frame.config) {
                Ok(d) => decoder = Some((frame.config.clone(), d)),
                Err(e) => {
                    let mut s = shared.lock();
                    s.error = Some(String::from(e));
                    return;
                }
            }
        }
        let Ok(mut pcm) = decoder.as_mut().unwrap().1.decode(&frame.data) else { continue };
        let end = frame.pts + frame.duration;
        if end <= skip {
            continue;
        }
        let trim = if frame.pts < skip { ((skip - frame.pts) * 48_000 / 1_000_000) as usize * 2 } else { 0 };
        let trim = trim.min(pcm.len());
        {
            let mut s = shared.lock();
            if s.epoch != epoch {
                continue;
            }
            if s.base.is_none() {
                s.base = Some(frame.pts.max(skip));
            }
        }
        // Short writes, so pausing, seeking and leaving stop promptly.
        for chunk in pcm[trim..].chunks_mut(1920) {
            loop {
                let s = shared.lock();
                if s.stop || s.epoch != epoch {
                    break;
                }
                if !s.paused {
                    if s.muted {
                        chunk.fill(0);
                    } else if s.volume < 1.0 {
                        for v in chunk.iter_mut() {
                            *v = (*v as f64 * s.volume) as i16;
                        }
                    }
                    drop(s);
                    if output.write(chunk).is_err() {
                        shared.lock().error = Some(String::from("Audio output failed"));
                        return;
                    }
                    let mut s = shared.lock();
                    if s.epoch == epoch {
                        s.written += chunk.len() as u64 / 2;
                    }
                    break;
                }
                drop(s);
                corekit::time::sleep_ms(10);
            }
            let s = shared.lock();
            if s.stop || s.epoch != epoch {
                break;
            }
        }
    }
}

/// Downloads an MP4 file and turns its tracks into a finished source.
fn load_file(src: &str, id: u32) -> Result<MediaSource, String> {
    let url = Url::parse(src).ok_or("Invalid media URL")?;
    let response = get(&url)?;
    if !response.ok() {
        return Err(format!("Media request failed ({})", response.status));
    }
    let data = response.body;
    let video = nebula_media::mp4::Video::parse(&data).map_err(String::from)?;
    corekit::io::log(&format!(
        "[nebula] native video {}x{} {} samples {}ms audio={}\n",
        video.width,
        video.height,
        video.samples.len(),
        video.duration_ms,
        video.audio.is_some()
    ));
    let frames = |samples: &[nebula_media::mp4::Sample]| -> Vec<CodedFrame> {
        let mut dts = 0i64;
        samples
            .iter()
            .map(|s| {
                let f = CodedFrame {
                    pts: s.time_ms as i64 * 1000,
                    dts,
                    duration: s.duration_ms as i64 * 1000,
                    sync: s.sync,
                    data: Arc::from(&data[s.offset..s.offset + s.size]),
                    config: Arc::from(&[][..]),
                };
                dts += (s.duration_ms as i64 * 1000).max(1);
                f
            })
            .collect()
    };
    let mut tracks = alloc::vec![(
        TrackKind::Video,
        video.config.clone(),
        video.width as u32,
        video.height as u32,
        frames(&video.samples)
    )];
    if let Some(a) = &video.audio {
        tracks.push((TrackKind::Audio, a.config.clone(), 0, 0, frames(&a.samples)));
    }
    Ok(MediaSource::from_tracks(id, video.duration_ms as f64 / 1000.0, tracks))
}
