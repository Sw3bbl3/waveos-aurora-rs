//! Loads a live page on the host the way Nebula does: its scripts, scripts
//! they insert, fetch/XMLHttpRequest, timers and script navigations.
//! Network goes through `curl`, sharing one cookie jar. For diagnosing
//! compatibility without booting WaveOS:
//!
//! `cargo run --release -p nebula-engine --example browse -- URL [--seconds N] [--dump out.html] [--trace]`

use nebula_engine::nebula_script::Host;
use nebula_engine::script::{HttpRequest, HttpResponse};
use nebula_engine::{Page, ScriptSource, Scripts};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, Instant};

const UA: &str = "Mozilla/5.0 (X11; WaveOS Aurora x86_64) Nebula/0.8 Pulsar/0.8";

struct StdHost;

impl Host for StdHost {
    fn now_ms(&mut self) -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
    }

    fn console(&mut self, level: &str, line: &str) {
        let line: String = line.chars().take(8000).collect();
        println!("  console.{level}: {line}");
    }
}

struct Response {
    status: u16,
    reason: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Net {
    dir: std::path::PathBuf,
    jar: String,
    seq: u32,
    trace: bool,
}

impl Net {
    fn request(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, String> {
        self.seq += 1;
        if let Some(path) = url.strip_prefix("file://") {
            let body = std::fs::read(path).map_err(|e| e.to_string())?;
            let ct = if path.ends_with(".html") { "text/html" } else { "text/plain" };
            let headers = vec![(String::from("content-type"), String::from(ct))];
            return Ok(Response { status: 200, reason: "OK".into(), url: url.into(), headers, body });
        }
        let head = self.dir.join(format!("h{}", self.seq));
        let out = self.dir.join(format!("b{}", self.seq));
        let mut c = Command::new("curl");
        c.args(["-sL", "--compressed", "--max-time", "30", "-A", UA, "-b", &self.jar, "-c", &self.jar]);
        c.args(["-X", method, "-D"]).arg(&head).arg("-o").arg(&out).args(["-w", "%{url_effective}"]);
        for (k, v) in headers {
            if !k.eq_ignore_ascii_case("user-agent") {
                c.arg("-H").arg(format!("{k}: {v}"));
            }
        }
        if !body.is_empty() {
            let bf = self.dir.join(format!("q{}", self.seq));
            std::fs::write(&bf, body).unwrap();
            c.arg("--data-binary").arg(format!("@{}", bf.display()));
        }
        c.arg(url);
        let t = Instant::now();
        let o = c.output().map_err(|e| e.to_string())?;
        let final_url = String::from_utf8_lossy(&o.stdout).into_owned();
        let head = std::fs::read_to_string(&head).unwrap_or_default();
        // The last response's header block (after redirects).
        let block = head.split("\r\n\r\n").filter(|b| b.starts_with("HTTP/")).last().unwrap_or("");
        let mut lines = block.lines();
        let status_line = lines.next().unwrap_or("");
        let mut parts = status_line.splitn(3, ' ');
        parts.next();
        let status = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let reason = parts.next().unwrap_or("").to_string();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .filter(|(k, _)| k != "content-encoding" && k != "content-length")
            .collect();
        let body = std::fs::read(&out).unwrap_or_default();
        if self.trace {
            println!("  net {method} {status} {} bytes {:?} {}", body.len(), t.elapsed(), short(url));
        }
        if status == 0 {
            return Err(format!("curl failed: {}", String::from_utf8_lossy(&o.stderr)));
        }
        Ok(Response { status, reason, url: final_url, headers, body })
    }

    fn get(&mut self, url: &str) -> Result<Response, String> {
        self.request("GET", url, &[], &[])
    }
}

fn short(u: &str) -> String {
    u.chars().take(140).collect()
}

fn join(base: &str, href: &str) -> Option<String> {
    nebula_web::Url::parse(base)?.join(href).map(|u| u.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut url = args[1].clone();
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let seconds: f64 = opt("--seconds").and_then(|s| s.parse().ok()).unwrap_or(15.0);
    let dump = opt("--dump");
    let dir = std::env::temp_dir().join(format!("nebula-browse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let jar = opt("--jar").unwrap_or_else(|| dir.join("cookies.txt").display().to_string());
    let mut net = Net { dir: dir.clone(), jar, seq: 0, trace: args.iter().any(|a| a == "--trace") };
    let t_all = Instant::now();
    for _navigation in 0..5 {
        let t0 = Instant::now();
        let resp = match net.get(&url) {
            Ok(r) => r,
            Err(e) => {
                println!("load failed: {e}");
                return;
            }
        };
        url = resp.url.clone();
        let ct = resp.headers.iter().find(|(k, _)| k == "content-type").map(|(_, v)| v.clone()).unwrap_or_default();
        println!("document {} {} ({} bytes) {:?}", resp.status, short(&url), resp.body.len(), t0.elapsed());
        let mut page = Page::parse(&nebula_engine::decode(&resp.body, &ct));
        let base = page.base.as_ref().and_then(|b| join(&url, b)).unwrap_or_else(|| url.clone());
        let mut js = Scripts::new(Box::new(StdHost), &url, (1280, 800), BTreeMap::new());
        if std::env::var("PROFILE").is_ok() {
            js.realm.profile = Some(BTreeMap::new());
        }
        // Stylesheets, as Nebula loads them before scripts run.
        let mut sheets = 0;
        page.load_stylesheets(&nebula_web::Url::parse(&base).unwrap(), 40, |u| {
            sheets += 1;
            let r = net.get(&u.to_string()).ok()?;
            Some((nebula_web::Url::parse(&r.url)?, String::from_utf8_lossy(&r.body).into_owned()))
        });
        println!("{sheets} stylesheets");
        let mut sources = Vec::new();
        if let Ok(path) = std::env::var("INJECT") {
            sources.push((String::from("inject.js"), std::fs::read_to_string(path).unwrap()));
        }
        for s in page.scripts() {
            match s {
                ScriptSource::Inline(c) => sources.push((format!("inline #{}", sources.len() + 1), c)),
                ScriptSource::External(href) => {
                    let Some(u) = join(&base, &href) else { continue };
                    let code = net.get(&u).map(|r| String::from_utf8_lossy(&r.body).into_owned()).unwrap_or_default();
                    sources.push((u, code));
                }
            }
        }
        for (name, code) in sources {
            let t = Instant::now();
            page.run_script(&mut js, &code, &name);
            let el = t.elapsed();
            if el > Duration::from_millis(50) {
                println!("script {} ({} KB) took {:?}", short(&name), code.len() >> 10, el);
            }
        }
        page.finish_loading(&mut js);
        println!("loaded in {:?}", t0.elapsed());
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        // Profile what happens after loading (PROFILE=load includes it).
        if std::env::var("PROFILE").is_ok_and(|v| v != "load") {
            if let Some(p) = js.realm.profile.as_mut() {
                p.clear();
            }
        }
        let mut navigate = None;
        let mut players: BTreeMap<usize, SimPlayer> = BTreeMap::new();
        let (mut laid, mut last_layout) = (u64::MAX, Instant::now() - Duration::from_secs(5));
        while Instant::now() < deadline {
            let mut busy = false;
            for (node, src) in js.take_fetches() {
                busy = true;
                let u = join(&base, &src).unwrap_or(src);
                let code =
                    net.get(&u).ok().filter(|r| r.status < 400).map(|r| String::from_utf8_lossy(&r.body).into_owned());
                let t = Instant::now();
                page.run_fetched(&mut js, node, &u, code.as_deref());
                println!("inserted script {} ({} KB) {:?}", short(&u), code.map_or(0, |c| c.len()) >> 10, t.elapsed());
            }
            let requests: Vec<HttpRequest> = js.take_requests();
            for req in requests {
                busy = true;
                let u = join(&base, &req.url).unwrap_or(req.url.clone());
                let r = net.request(&req.method, &u, &req.headers, &req.body).map(|r| HttpResponse {
                    status: r.status,
                    status_text: r.reason,
                    url: r.url,
                    body: r.body,
                    headers: r.headers,
                });
                println!("request {} {} -> {}", req.method, short(&u), r.as_ref().map(|r| r.status).unwrap_or(0));
                if let Ok(resp) = &r {
                    if resp.status >= 400 && std::env::var("ERRBODY").is_ok() {
                        println!("  request headers: {:?}", req.headers);
                        println!(
                            "  request body: {}",
                            String::from_utf8_lossy(&req.body).chars().take(4000).collect::<String>()
                        );
                        println!(
                            "  response: {}",
                            String::from_utf8_lossy(&resp.body).chars().take(1500).collect::<String>()
                        );
                    }
                }
                if std::env::var("BODIES").is_ok() && !req.body.is_empty() {
                    println!("  body: {}", String::from_utf8_lossy(&req.body).chars().take(30000).collect::<String>());
                }
                let t = Instant::now();
                page.complete_request(&mut js, req.id, r);
                if t.elapsed() > Duration::from_millis(30) {
                    println!("  slow response handler {:?}", t.elapsed());
                }
            }
            busy |= simulate_media(&mut page, &mut js, &mut players);
            if page.doc.version != laid && last_layout.elapsed() > Duration::from_millis(1000) {
                let t = Instant::now();
                let styles = page.styles(1280, 800);
                let cascade = t.elapsed();
                let layout = page.layout_viewport(&styles, 1280, 800, &FakeFonts);
                if std::env::var("LAYOUT").is_ok() {
                    println!(
                        "  cascade {:?} layout {:?} ({} nodes)",
                        cascade,
                        t.elapsed() - cascade,
                        page.doc.nodes.len()
                    );
                }
                js.set_layout(&layout, (0, 0));
                laid = page.doc.version;
                last_layout = Instant::now();
                if t.elapsed() > Duration::from_millis(300) {
                    println!("  layout took {:?}", t.elapsed());
                }
            }
            if let Some(n) = js.take_navigation() {
                navigate = Some(n);
                break;
            }
            let now = StdHost.now_ms();
            match js.next_wakeup(now) {
                Some(w) if w <= now + 16.0 => {
                    if w > now && !busy {
                        std::thread::sleep(Duration::from_secs_f64((w - now) / 1000.0));
                    }
                    let t = Instant::now();
                    page.run_timers(&mut js, StdHost.now_ms());
                    if t.elapsed() > Duration::from_millis(30) {
                        println!(
                            "  slow timers {:?} (heap {} live, {} collections)",
                            t.elapsed(),
                            js.realm.heap.live(),
                            js.realm.heap.collections
                        );
                    }
                }
                w if !busy => {
                    let wait = w.map_or(50.0, |w| (w - now).clamp(1.0, 50.0));
                    std::thread::sleep(Duration::from_secs_f64(wait / 1000.0));
                }
                _ => {}
            }
        }
        report(&page, &url, t_all.elapsed());
        if let Some(p) = js.realm.profile.take() {
            let total: u64 = p.values().sum();
            let mut v: Vec<_> = p.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            println!("profile: {total} samples");
            for ((name, file, line, col), n) in v.iter().take(40) {
                println!("  {:5.1}% {name} {}:{line}:{col}", *n as f64 * 100.0 / total as f64, short(file));
            }
        }
        if let Some(pt) = opt("--probe") {
            let v: Vec<i32> = pt.split(',').map(|a| a.parse().unwrap_or(0)).collect();
            let (px, py, vw, vh) = (v[0], v[1], *v.get(2).unwrap_or(&1280), *v.get(3).unwrap_or(&800));
            let styles = page.styles(vw, vh);
            let layout = page.layout_viewport(&styles, vw, vh, &FakeFonts);
            for b in layout.boxes.iter().filter(|b| b.rect.contains(px, py)) {
                let e = page.doc.element(b.node);
                let st = styles.styles[b.node].as_ref();
                println!(
                    "  box {:?} <{} id={:?} class={:?}> bg={:08x} pos={:?} visible={}",
                    b.rect,
                    page.doc.tag(b.node),
                    e.and_then(|e| e.attr("id")),
                    e.and_then(|e| e.attr("class")).map(|c| c.chars().take(60).collect::<String>()),
                    st.map_or(0, |s| s.background),
                    st.map(|s| s.position),
                    st.is_some_and(|s| s.visible)
                );
            }
            for it in layout.items.iter() {
                if let nebula_engine::Item::Video { rect, .. } = it {
                    println!("  video item {rect:?}");
                }
            }
        }
        if args.iter().any(|a| a == "--decode") {
            decode_all(&page);
        }
        if let Some(path) = &dump {
            std::fs::write(path, nebula_engine::html::serialize_children(&page.doc, 0)).unwrap();
        }
        match navigate {
            Some(n) => {
                let Some(next) = join(&url, &n) else { return };
                println!("navigating to {}", short(&next));
                url = next;
            }
            None => return,
        }
    }
}

fn report(page: &Page, url: &str, took: Duration) {
    let doc = &page.doc;
    let connected = doc.descendants(0).len();
    let mut tags: BTreeMap<String, usize> = BTreeMap::new();
    for n in doc.descendants(0) {
        if let Some(e) = doc.element(n) {
            if e.tag.contains('-') || matches!(e.tag.as_str(), "video" | "audio" | "iframe") {
                *tags.entry(e.tag.clone()).or_default() += 1;
            }
        }
    }
    let mut top: Vec<_> = tags.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let mut text = String::new();
    if let Some(b) = doc.find("body") {
        let mut stack = vec![b];
        while let Some(n) = stack.pop() {
            match &doc.nodes[n].data {
                nebula_engine::dom::NodeData::Text(t) => {
                    text.push_str(t);
                    text.push(' ');
                }
                nebula_engine::dom::NodeData::Element(e)
                    if matches!(e.tag.as_str(), "script" | "style" | "template") => {}
                _ => stack.extend(doc.nodes[n].children.iter().rev()),
            }
        }
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    println!("== {} after {:?}: {connected} connected nodes, title {:?}", short(url), took, page.title);
    println!("   custom/media elements: {:?}", &top[..top.len().min(25)]);
    println!("   text ({} words): {}", words.len(), words.iter().take(80).copied().collect::<Vec<_>>().join(" "));
}

/// A stand-in for Nebula's native player: media time advances while the
/// element plays and data is buffered, with the events a real player sends.
#[derive(Default)]
struct SimPlayer {
    clock: Option<Instant>,
    last_update: Option<Instant>,
    playing: bool,
    loaded: bool,
    logged: f64,
}

fn simulate_media(page: &mut Page, js: &mut Scripts, players: &mut BTreeMap<usize, SimPlayer>) -> bool {
    for ms in page.doc.media_sources.values() {
        let p = players.entry(usize::MAX - ms.id as usize).or_default();
        if p.logged != ms.revision as f64 {
            p.logged = ms.revision as f64;
            let bufs: Vec<String> = ms
                .buffers
                .iter()
                .map(|b| format!("#{} {} init={} {:?}", b.id, b.mime, b.initialized, ms.buffered(b.id)))
                .collect();
            println!(
                "  mse {} {:?} duration {} element {:?}: {}",
                ms.id,
                ms.ready,
                ms.duration,
                ms.element,
                bufs.join("; ")
            );
        }
    }
    let nodes: Vec<usize> = page.doc.media.keys().copied().collect();
    let mut events: Vec<(usize, &'static str)> = Vec::new();
    for n in nodes {
        let p = players.entry(n).or_default();
        let doc = &mut page.doc;
        let Some(id) = doc.media.get(&n).and_then(|s| s.source) else { continue };
        let Some(ms) = doc.media_sources.get(&id) else { continue };
        let (meta, buffered, duration, ended_stream) = (
            ms.has_metadata(),
            ms.element_buffered(),
            ms.duration,
            ms.ready == nebula_engine::media::ReadyState::Ended,
        );
        let size = ms.track(nebula_engine::media::TrackKind::Video).map(|t| (t.width, t.height));
        let s = doc.media.get_mut(&n).unwrap();
        if meta && s.ready_state == 0 {
            s.ready_state = 1;
            if let Some((w, h)) = size {
                s.width = w;
                s.height = h;
            }
            s.duration = duration;
            events.extend(["durationchange", "loadedmetadata"].map(|e| (n, e)));
        }
        if s.ready_state == 0 {
            continue;
        }
        if let Some(t) = s.seek.take() {
            s.current_time = t;
            s.seeking = true;
            if s.ready_state > 1 {
                s.ready_state = 1;
            }
            p.clock = None;
            events.push((n, "seeking"));
        }
        // Small gaps (a first frame shown slightly late) are skipped, as players do.
        if let Some(&(a, _)) = buffered.iter().find(|&&(a, _)| a > s.current_time && a - s.current_time < 0.5) {
            s.current_time = a;
        }
        let t = s.current_time;
        let have = buffered.iter().any(|&(a, b)| t + 0.001 >= a && t < b);
        let ahead = buffered.iter().find(|&&(a, b)| t + 0.001 >= a && t < b).map_or(0.0, |&(_, b)| b - t);
        if have && s.ready_state < 3 {
            s.ready_state = if ahead > 4.0 { 4 } else { 3 };
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
                events.extend(["timeupdate", "seeked"].map(|e| (n, e)));
            }
        }
        if !s.paused && s.ready_state >= 3 && !p.playing {
            p.playing = true;
            events.push((n, "playing"));
        }
        if s.paused {
            p.playing = false;
            p.clock = None;
            continue;
        }
        if s.ready_state >= 3 {
            let now = Instant::now();
            let dt = p.clock.map_or(0.0, |c| (now - c).as_secs_f64());
            p.clock = Some(now);
            let end = buffered.iter().find(|&&(a, b)| t + 0.001 >= a && t < b).map_or(t, |&(_, b)| b);
            s.current_time = (t + dt * s.playback_rate).min(end);
            if p.last_update.is_none_or(|u| now - u > Duration::from_millis(250)) {
                p.last_update = Some(now);
                events.push((n, "timeupdate"));
            }
            if s.current_time - p.logged >= 5.0 {
                p.logged = s.current_time;
                println!("  media: playing at {:.1}s of {:.1}s, buffered {:?}", s.current_time, duration, buffered);
            }
            if s.current_time >= end - 0.001 {
                if ended_stream && s.current_time >= duration - 0.1 {
                    s.ended = true;
                    s.paused = true;
                    events.extend(["timeupdate", "pause", "ended"].map(|e| (n, e)));
                } else {
                    s.ready_state = 2;
                    p.clock = None;
                    p.playing = false;
                    println!("  media: waiting at {:.2}s, buffered {:?}", s.current_time, buffered);
                    events.push((n, "waiting"));
                }
            }
        }
    }
    let any = !events.is_empty();
    for (n, e) in events {
        page.dispatch(js, n, e, false, false);
    }
    any
}

/// Text measured as half an em per character, as the layout tests do.
struct FakeFonts;

impl nebula_engine::Host for FakeFonts {
    fn measure(&self, font: nebula_engine::FontSpec, text: &str) -> i32 {
        text.chars().count() as i32 * font.size as i32 / 2
    }
    fn metrics(&self, font: nebula_engine::FontSpec) -> nebula_engine::Metrics {
        nebula_engine::Metrics { ascent: font.size as i32 * 8 / 10, descent: font.size as i32 * 2 / 10 }
    }
    fn image_size(&self, _: &str) -> Option<(u32, u32)> {
        None
    }
}

/// Decodes every buffered frame of every source, as Nebula's player would.
fn decode_all(page: &Page) {
    use nebula_engine::media::TrackKind;
    for ms in page.doc.media_sources.values() {
        for t in ms.buffers.iter().flat_map(|b| &b.tracks) {
            let t0 = Instant::now();
            let (mut ok, mut none, mut errors, mut configs) = (0, 0, Vec::new(), 0);
            match t.kind {
                TrackKind::Video => {
                    let mut dec: Option<(std::sync::Arc<[u8]>, nebula_media::VideoDecoder)> = None;
                    for f in t.frames.values() {
                        if dec.as_ref().is_none_or(|(c, _)| c[..] != f.config[..]) {
                            configs += 1;
                            dec = nebula_media::VideoDecoder::new(&f.config).ok().map(|d| (f.config.clone(), d));
                        }
                        let Some((_, d)) = dec.as_mut() else { continue };
                        match d.decode(&f.data) {
                            Ok(Some(pic)) => {
                                ok += 1;
                                if ok == 40 {
                                    if let Ok(px) = nebula_media::pixels(&pic) {
                                        let mut ppm = format!("P6 {} {} 255\n", pic.width, pic.height).into_bytes();
                                        for p in px {
                                            ppm.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
                                        }
                                        std::fs::write("/tmp/nebula-frame.ppm", ppm).unwrap();
                                        let slices = {
                                            let (mut at, mut n) = (0, 0);
                                            while at + 4 < f.data.len() {
                                                let len =
                                                    u32::from_be_bytes(f.data[at..at + 4].try_into().unwrap()) as usize;
                                                if matches!(f.data[at + 4] & 31, 1 | 5) {
                                                    n += 1;
                                                }
                                                at += 4 + len;
                                            }
                                            n
                                        };
                                        println!("frame 40: {}x{}, {} slices", pic.width, pic.height, slices);
                                    }
                                }
                            }
                            Ok(None) => none += 1,
                            Err(e) => errors.push((f.pts, e, f.sync)),
                        }
                    }
                }
                TrackKind::Audio => {
                    let mut dec = nebula_media::AudioDecoder::new(&t.config).ok();
                    for f in t.frames.values() {
                        match dec.as_mut().map(|d| d.decode(&f.data)) {
                            Some(Ok(_)) => ok += 1,
                            Some(Err(e)) => errors.push((f.pts, e, f.sync)),
                            None => none += 1,
                        }
                    }
                }
                _ => {}
            }
            println!(
                "decode {:?} {}x{}: {} frames, {ok} pictures, {none} empty, {} errors, {configs} configs in {:?}; first errors {:?}",
                t.kind, t.width, t.height, t.frames.len(), errors.len(), t0.elapsed(), &errors[..errors.len().min(5)]
            );
        }
    }
}
