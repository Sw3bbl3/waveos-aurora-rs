//! Nebula — the WaveOS Aurora web browser.
//!
//! Pages load on a background thread (the document, its stylesheets and
//! scripts), images on a few more; layout (`nebula-engine`), scripts
//! (Pulsar) and painting happen here.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_image::Image;
use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use corekit::sync::Mutex;
use corekit::thread::JoinHandle;
use nebula_engine::dom::{Document, NodeId};
use nebula_engine::layout::Item;
use nebula_engine::nebula_script;
use nebula_engine::style::ListStyle;
use nebula_engine::{FontSpec, Host, Layout, Metrics, Page, ScriptSource, Scripts};
use nebula_web::http::{self, Response};
use nebula_web::Url;
use waveui::lumen::canvas::Canvas;
use waveui::lumen::font::{self, Face, Font};
use waveui::lumen::geom::Rect;
use waveui::lumen::theme;
use waveui::text::TextEdit;
use waveui::{KeyCode, KeyEvent};

corekit::entry!(main);

mod cache;
mod checks;
mod chrome;
mod media;
mod prefetch;

const MAX_TABS: usize = 8;
const IMAGE_BYTES: usize = 12 * 1024 * 1024;
const HOME: &str = "about:home";
const SEARCH: &str = "https://html.duckduckgo.com/html/?q=";
const IMAGE_WORKERS: usize = 6;
const MAX_IMAGES: usize = 80;
const MAX_STYLESHEETS: usize = 16;
const MAX_SCRIPTS: usize = 48;

/// Whether pages may run JavaScript (preference `nebula_javascript`).
fn javascript_enabled() -> bool {
    corekit::prefs::get_int("nebula_javascript", 1) != 0
}

/// Where scripts' console output and the clock come from.
struct PageHost;

impl nebula_script::Host for PageHost {
    fn now_ms(&mut self) -> f64 {
        corekit::time::unix_ms()
    }

    fn console(&mut self, level: &str, line: &str) {
        corekit::io::log(&format!("[nebula] console.{level}: {line}\n"));
    }

    fn seed(&mut self) -> u64 {
        let mut b = [0u8; 8];
        corekit::net::random(&mut b);
        u64::from_le_bytes(b)
    }

    fn timezone_offset(&mut self, _utc_ms: f64) -> f64 {
        -(corekit::time::utc_offset_minutes() as f64)
    }

    fn fill_random(&mut self, buf: &mut [u8]) -> bool {
        corekit::net::random(buf);
        true
    }
}

/// localStorage is kept per site, in a file under /Settings/Nebula.
fn storage_path(url: &Url) -> String {
    let site: String =
        url.host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let site = if site.is_empty() { String::from("local-files") } else { site };
    format!("/Settings/Nebula/Storage/{site}")
}

fn load_storage(url: &Url) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let raw = corekit::fs::read_to_string(&storage_path(url)).unwrap_or_default();
    for entry in raw.split('\u{1E}') {
        if let Some((k, v)) = entry.split_once('\u{1F}') {
            m.insert(String::from(k), String::from(v));
        }
    }
    m
}

fn save_storage(url: &Url, m: &BTreeMap<String, String>) {
    let _ = corekit::fs::mkdir("/Settings/Nebula");
    let _ = corekit::fs::mkdir("/Settings/Nebula/Storage");
    let raw: Vec<String> = m.iter().map(|(k, v)| format!("{k}\u{1F}{v}")).collect();
    let path = storage_path(url);
    if let Err(e) = corekit::fs::write(&path, raw.join("\u{1E}").as_bytes()) {
        corekit::io::log(&format!("[nebula] couldn't save {path}: {e}\n"));
    }
}

/// A localStorage change: (key, old value, new value); no key means clear().
type StorageChange = (Option<String>, Option<String>, Option<String>);

/// localStorage shared by every tab: one copy per site, and a short log of
/// changes so other tabs on the same site get `storage` events.
#[derive(Default)]
struct StorageHub {
    sites: BTreeMap<String, BTreeMap<String, String>>,
    /// (sequence, site, writing tab, page URL, changes).
    log: VecDeque<(u64, String, u64, String, Vec<StorageChange>)>,
    seq: u64,
    /// Compiled scripts every tab reuses (revisits skip parsing them again).
    code: Rc<RefCell<nebula_script::bytecode::CodeCache>>,
}

type Hub = Rc<RefCell<StorageHub>>;

impl StorageHub {
    fn get(&mut self, url: &Url) -> BTreeMap<String, String> {
        self.sites.entry(storage_path(url)).or_insert_with(|| load_storage(url)).clone()
    }

    /// A tab's scripts changed its site's storage to `new`.
    fn write(&mut self, from: u64, url: &Url, new: BTreeMap<String, String>) {
        let site = storage_path(url);
        let old = self.sites.remove(&site).unwrap_or_else(|| load_storage(url));
        let mut changes = Vec::new();
        if new.is_empty() && !old.is_empty() {
            changes.push((None, None, None));
        } else {
            for (k, v) in &old {
                match new.get(k) {
                    None => changes.push((Some(k.clone()), Some(v.clone()), None)),
                    Some(n) if n != v => changes.push((Some(k.clone()), Some(v.clone()), Some(n.clone()))),
                    _ => {}
                }
            }
            for (k, n) in &new {
                if !old.contains_key(k) {
                    changes.push((Some(k.clone()), None, Some(n.clone())));
                }
            }
        }
        save_storage(url, &new);
        self.sites.insert(site.clone(), new);
        if !changes.is_empty() {
            self.seq += 1;
            self.log.push_back((self.seq, site, from, url.to_string(), changes));
            if self.log.len() > 64 {
                self.log.pop_front();
            }
        }
    }

    /// Changes other tabs made to `url`'s site after `seq`, and the newest sequence.
    fn since(&self, seq: u64, url: &Url, me: u64) -> (u64, Vec<(String, Vec<StorageChange>)>) {
        let site = storage_path(url);
        let changes = self
            .log
            .iter()
            .filter(|(s, at, from, _, _)| *s > seq && *at == site && *from != me)
            .map(|(_, _, _, page, c)| (page.clone(), c.clone()))
            .collect();
        (self.seq, changes)
    }
}

/// Cookies are shared by every page and kept, when they ask to be, here.
const COOKIES_PATH: &str = "/Settings/Nebula/Cookies";

fn load_cookies() {
    if let Ok(text) = corekit::fs::read_to_string(COOKIES_PATH) {
        *corekit::web::cookies() = nebula_web::cookie::Jar::load(&text, corekit::web::unix_time());
    }
}

/// Saves the persistent cookies, if any changed.
fn save_cookies() {
    let text = {
        let mut jar = corekit::web::cookies();
        if !core::mem::take(&mut jar.dirty) {
            return;
        }
        jar.remove_expired(corekit::web::unix_time());
        jar.dirty = false;
        jar.save()
    };
    let _ = corekit::fs::mkdir("/Settings/Nebula");
    if let Err(e) = corekit::fs::write(COOKIES_PATH, text.as_bytes()) {
        corekit::io::log(&format!("[nebula] couldn't save cookies: {e}\n"));
    }
}

/// `document.cookie` reads and writes the browser's jar.
struct BrowserCookies;

impl nebula_engine::script::CookieHost for BrowserCookies {
    fn with_jar(&mut self, f: &mut dyn FnMut(&mut nebula_web::cookie::Jar)) {
        f(&mut corekit::web::cookies())
    }
}

/// Cookies for a script's request: only to the page's own origin, as with
/// `credentials: "same-origin"` (there's no CORS yet, so a page must never
/// read another site's responses as the user).
struct SameOrigin(String);

fn origin(u: &Url) -> String {
    format!("{}://{}:{}", u.scheme, u.host, u.port_or_default())
}

impl http::Cookies for SameOrigin {
    fn header(&mut self, url: &Url) -> String {
        if origin(url) == self.0 {
            corekit::web::SharedCookies.header(url)
        } else {
            String::new()
        }
    }
    fn store(&mut self, url: &Url, set_cookie: &str) {
        if origin(url) == self.0 {
            corekit::web::SharedCookies.store(url, set_cookie);
        }
    }
}

// ------------------------------------------------------------------ loading

/// Where a page load's time went (ms since the load started).
#[derive(Clone, Copy, Default)]
struct Timing {
    started: u64,
    document: u64,
    styles: u64,
    scripts: u64,
    style_count: usize,
    script_count: usize,
    bytes: usize,
    net: corekit::web::Stats,
}

struct Loaded {
    timing: Timing,
    page: Page,
    url: Url,
    /// Images that came with the document (an image opened directly).
    images: Vec<(String, Image)>,
    /// The page's scripts, fetched, in order: (name, source).
    scripts: Vec<(String, String)>,
    /// Its scripts come later, through the load job.
    pending: bool,
}

/// A script ready to run: (name for errors, source).
type NamedSource = (String, String);
/// A script a script inserted: (page generation, element, URL, source if it loaded).
type ScriptLoad = (u64, NodeId, String, Option<String>);
/// The answer to a script's request: (page generation, id, result).
type RequestResult = (u64, u32, Result<nebula_engine::script::HttpResponse, String>);

struct LoadJob {
    cancel: AtomicBool,
    progress: Mutex<String>,
    /// The page with its styles: shown as soon as it's here.
    result: Mutex<Option<Result<Loaded, String>>>,
    /// Then its scripts, in order, when the page had external ones (and
    /// when they had all arrived, uptime ms).
    scripts: Mutex<Option<(Vec<NamedSource>, u64)>>,
}

/// What's still downloading once the page is shown: its scripts.
struct Pending {
    prefetch: prefetch::Prefetch,
    scripts: Vec<ScriptSource>,
    base: Url,
    final_url: String,
}

/// GETs any URL Nebula opens, through the cache for the network.
fn get_shared(url: &Url) -> Result<Arc<Response>, String> {
    match url.scheme.as_str() {
        "http" | "https" => cached_fetch(url),
        _ => get(url).map(Arc::new),
    }
}

/// Stage two of a load: the scripts, in document order.
fn load_scripts(p: Pending, job: &LoadJob) -> Vec<NamedSource> {
    let mut out = Vec::new();
    for (i, src) in p.scripts.into_iter().enumerate() {
        if job.cancel.load(Ordering::Relaxed) {
            p.prefetch.cancel();
            break;
        }
        match src {
            ScriptSource::Inline(code) => out.push((format!("{} (inline script {})", p.final_url, i + 1), code)),
            ScriptSource::External(href) => {
                let Some(u) = p.base.join(&href) else { continue };
                *job.progress.lock() = format!("Loading scripts from {}…", u.host);
                if let Ok(r) = p.prefetch.take(&u, get_shared) {
                    if r.ok() {
                        out.push((u.to_string(), nebula_engine::decode(&r.body, "")));
                    }
                }
            }
        }
    }
    p.prefetch.finish();
    out
}

fn describe(e: http::Error) -> String {
    match e {
        http::Error::Io(errno) => String::from(corekit::net::describe(corekit::Error(errno))),
        other => other.to_string(),
    }
}

fn content_type_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        "text/html"
    } else if aurora_image::is_image_name(&lower) {
        "image/png"
    } else {
        "text/plain"
    }
}

/// GETs a URL (http, https or file).
fn get(url: &Url) -> Result<Response, String> {
    match url.scheme.as_str() {
        "http" | "https" => cached_fetch(url).map(|r| (*r).clone()),
        "file" => {
            let path = nebula_web::url::percent_decode(&url.path, false);
            let body = corekit::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            Ok(Response {
                status: 200,
                reason: String::from("OK"),
                headers: alloc::vec![(String::from("content-type"), String::from(content_type_for(&path)))],
                body,
                url: url.clone(),
            })
        }
        other => Err(format!("\"{other}:\" addresses aren't supported")),
    }
}

/// GETs over the network through the shared cache: fresh responses come
/// from memory, stale ones are revalidated.
fn cached_fetch(url: &Url) -> Result<Arc<Response>, String> {
    match cache::lookup(url) {
        cache::Lookup::Fresh(r) => Ok(r),
        cache::Lookup::Stale(validators) => {
            let r = corekit::web::request("GET", url, &validators, &[]).map_err(describe)?;
            if r.status == 304 {
                if let Some(stored) = cache::revalidated(url, &r) {
                    return Ok(stored);
                }
            }
            Ok(cache::store(url, &r))
        }
        cache::Lookup::Missing => {
            let r = corekit::web::fetch(url).map_err(describe)?;
            Ok(cache::store(url, &r))
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Loads a document and its stylesheets (on a worker thread).
fn load(url: Url, job: &LoadJob) -> Result<(Loaded, Option<Pending>), String> {
    if url.scheme == "about" {
        let html = if url.path == "home" { home_page() } else { String::from("<html><body></body></html>") };
        let page = Page::parse(&html);
        return Ok((
            Loaded { timing: Timing::default(), page, url, images: Vec::new(), scripts: Vec::new(), pending: false },
            None,
        ));
    }
    let started = corekit::time::uptime_ms();
    let mut timing = Timing { started, net: corekit::web::stats(), ..Timing::default() };
    *job.progress.lock() = format!("Connecting to {}…", url.host);
    let resp = get(&url)?;
    timing.document = corekit::time::uptime_ms() - started;
    timing.bytes += resp.body.len();
    let final_url = resp.url.clone();
    let ct = resp.content_type();
    let is_html = ct.contains("html") || (ct.is_empty() && resp.body.iter().take(512).any(|b| *b == b'<'));
    let mut images = Vec::new();
    let mut page = if ct.starts_with("image/") || (ct.is_empty() && aurora_image::is_image_name(&final_url.path)) {
        let img =
            aurora_image::decode(&resp.body).map_err(|_| String::from("this kind of picture can't be shown yet"))?;
        let src = final_url.to_string();
        let html = format!(
            "<html><head><title>{} ({}×{})</title></head><body style='margin:0;background:#1c1c20;text-align:center'><img src=\"{}\" style='margin:16px auto'></body></html>",
            escape(corekit::fs::file_name(&final_url.path)),
            img.width,
            img.height,
            escape(&src)
        );
        images.push((src, img));
        Page::parse(&html)
    } else if ct.starts_with("video/mp4") || final_url.path.ends_with(".mp4") {
        Page::parse(&format!("<html><head><title>{}</title></head><body style='margin:24px;background:#18181c;color:white'><video controls autoplay src=\"{}\" style='width:960px;max-width:100%'></video></body></html>", escape(corekit::fs::file_name(&final_url.path)), escape(&final_url.to_string())))
    } else if is_html {
        *job.progress.lock() = String::from("Reading the page…");
        Page::parse(&nebula_engine::decode(&resp.body, resp.header("content-type").unwrap_or("")))
    } else if ct.starts_with("text/") || ct.contains("json") || ct.contains("javascript") || ct.contains("xml") {
        let text = nebula_engine::decode(&resp.body, resp.header("content-type").unwrap_or(""));
        Page::parse(&format!(
            "<html><head><title>{}</title></head><body><pre style='white-space:pre-wrap'>{}</pre></body></html>",
            escape(corekit::fs::file_name(&final_url.path)),
            escape(&text)
        ))
    } else {
        // Anything else is a download.
        let mut name = String::from(corekit::fs::file_name(final_url.path.split('?').next().unwrap_or("")));
        if name.is_empty() {
            name = String::from("download");
        }
        let (stem, ext) = match name.rfind('.') {
            Some(p) => (String::from(&name[..p]), String::from(&name[p..])),
            None => (name.clone(), String::new()),
        };
        let path = corekit::fs::unique_name("/Downloads", &stem, &ext);
        corekit::fs::write(&path, &resp.body).map_err(|e| format!("couldn't save the download: {e}"))?;
        Page::parse(&format!(
            "<html><head><title>Downloaded</title></head><body style='font-family:sans-serif;margin:48px'><h2>Downloaded {}</h2><p>Saved {} to <b>{}</b>.</p></body></html>",
            escape(&name),
            human_size(resp.body.len() as u64),
            escape(&path)
        ))
    };
    if !resp.ok() && resp.body.is_empty() {
        return Err(format!("the server answered {} {}", resp.status, resp.reason));
    }
    drop(resp);
    // Stylesheets and scripts download together; the page shows once its
    // styles are in, and its scripts run as they arrive.
    let base = document_base(&page, &final_url);
    let sources = if javascript_enabled() { page.scripts() } else { Vec::new() };
    let sources: Vec<ScriptSource> = sources.into_iter().take(MAX_SCRIPTS).collect();
    let mut wanted: Vec<Url> =
        page.pending_stylesheets().iter().take(MAX_STYLESHEETS).filter_map(|(_, href)| base.join(href)).collect();
    let externals: Vec<Url> = sources
        .iter()
        .filter_map(|s| match s {
            ScriptSource::External(href) => base.join(href),
            ScriptSource::Inline(_) => None,
        })
        .collect();
    timing.script_count = externals.len();
    wanted.extend(externals.iter().cloned());
    let prefetch = prefetch::Prefetch::start(wanted, get_shared);
    let (mut style_count, mut style_bytes) = (0, 0);
    page.load_stylesheets(&base, MAX_STYLESHEETS, |u| {
        if job.cancel.load(Ordering::Relaxed) {
            return None;
        }
        *job.progress.lock() = format!("Loading styles from {}…", u.host);
        let r = prefetch.take(u, get_shared).ok()?;
        style_count += 1;
        style_bytes += r.body.len();
        r.ok().then(|| (r.url.clone(), nebula_engine::decode(&r.body, "")))
    });
    timing.styles = corekit::time::uptime_ms() - started;
    timing.style_count = style_count;
    timing.bytes += style_bytes;
    let url = final_url.clone();
    if externals.is_empty() {
        // Inline scripts only: they run with the page.
        prefetch.finish();
        let scripts = sources
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                ScriptSource::Inline(code) => Some((format!("{} (inline script {})", final_url, i + 1), code)),
                ScriptSource::External(_) => None,
            })
            .collect();
        timing.scripts = timing.styles;
        return Ok((Loaded { timing, page, url, images, scripts, pending: false }, None));
    }
    let pending = Pending { prefetch, scripts: sources, base, final_url: final_url.to_string() };
    Ok((Loaded { timing, page, url, images, scripts: Vec::new(), pending: true }, Some(pending)))
}

/// The URL relative links resolve against (<base href>, else the document's).
fn document_base(page: &Page, url: &Url) -> Url {
    page.base.as_deref().and_then(|b| url.join(b)).unwrap_or_else(|| url.clone())
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{}.{} MB", n >> 20, ((n % (1 << 20)) * 10) >> 20),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

// ------------------------------------------------------------------ images

struct ImageQueue {
    queue: VecDeque<(u64, String)>,
    done: Vec<(u64, String, Option<Image>)>,
    bytes: usize,
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn load_image(url: &str) -> Option<Image> {
    let bytes = if let Some(data) = url.strip_prefix("data:") {
        let (meta, payload) = data.split_once(',')?;
        if meta.ends_with(";base64") {
            base64(payload)?
        } else {
            nebula_web::url::percent_decode(payload, false).into_bytes()
        }
    } else {
        let u = Url::parse(url)?;
        let r = get(&u).ok()?;
        if !r.ok() {
            return None;
        }
        r.body
    };
    let img = aurora_image::decode(&bytes).ok()?;
    Some(shrink(img, 1200))
}

/// Halves a picture until both dimensions fit `max` (saves memory).
fn shrink(mut img: Image, max: u32) -> Image {
    while img.width > max || img.height > max {
        let (w, h) = ((img.width / 2).max(1), (img.height / 2).max(1));
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let at = |dx: u32, dy: u32| {
                    img.pixels
                        [((y * 2 + dy).min(img.height - 1) * img.width + (x * 2 + dx).min(img.width - 1)) as usize]
                };
                let quad = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                let mut c = 0u32;
                for shift in [0, 8, 16, 24] {
                    let sum: u32 = quad.iter().map(|p| (p >> shift) & 0xFF).sum();
                    c |= (sum / 4) << shift;
                }
                px.push(c);
            }
        }
        img = Image { width: w, height: h, pixels: px };
    }
    img
}

fn image_worker(q: Arc<Mutex<ImageQueue>>, generation: Arc<AtomicU64>) {
    loop {
        let next = q.lock().queue.pop_front();
        let Some((g, url)) = next else { return };
        if g != generation.load(Ordering::Relaxed) {
            continue;
        }
        let img = load_image(&url);
        let mut q = q.lock();
        if g != generation.load(Ordering::Relaxed) {
            continue;
        }
        let bytes = img.as_ref().map_or(0, |i| i.pixels.len() * 4);
        let img = if q.bytes + bytes <= IMAGE_BYTES {
            q.bytes += bytes;
            img
        } else {
            None
        };
        q.done.push((g, url, img));
    }
}

// ------------------------------------------------------------------ fonts

fn font_for(f: FontSpec) -> Font {
    let face = if f.mono {
        Face::Mono
    } else if f.bold {
        Face::SemiBold
    } else {
        Face::Regular
    };
    font::get(face, f.size)
}

/// Canvas text, with the system fonts.
struct CanvasFonts;

fn canvas_font(px: f32, bold: bool, mono: bool) -> Font {
    let face = if mono {
        Face::Mono
    } else if bold {
        Face::SemiBold
    } else {
        Face::Regular
    };
    font::get(face, (px + 0.5).clamp(4.0, 512.0) as u16)
}

impl nebula_engine::script::TextHost for CanvasFonts {
    fn render(
        &mut self,
        text: &str,
        px: f32,
        bold: bool,
        _italic: bool,
        mono: bool,
    ) -> nebula_engine::script::TextMask {
        let f = canvas_font(px, bold, mono);
        let advance = f.width64(text) as f32 / 64.0;
        let (ascent, descent) = (f.ascent as i32, -(f.descent as i32));
        let (w, h) = (advance as i32 + 2, (ascent + descent).max(1));
        if w <= 0 || w as i64 * h as i64 > 4 << 20 {
            return nebula_engine::script::TextMask::default();
        }
        // White text on transparent black: the alpha channel is the coverage.
        let mut buf = alloc::vec![0u32; (w * h) as usize];
        Canvas::new(&mut buf, w, h).text(0, ascent, text, f, 0xFFFF_FFFF);
        nebula_engine::script::TextMask {
            width: w as u32,
            height: h as u32,
            advance,
            ascent: ascent as f32,
            descent: descent as f32,
            data: buf.iter().map(|p| (p >> 24) as u8).collect(),
        }
    }

    fn measure(&mut self, text: &str, px: f32, bold: bool, mono: bool) -> (f32, f32, f32) {
        let f = canvas_font(px, bold, mono);
        (f.width64(text) as f32 / 64.0, f.ascent as f32, -(f.descent as f32))
    }
}

struct Fonts<'a> {
    images: &'a BTreeMap<String, Option<Image>>,
    base: Option<Url>,
}

impl Host for Fonts<'_> {
    fn measure(&self, f: FontSpec, text: &str) -> i32 {
        let w = font_for(f).width(text);
        if f.italic {
            w + f.size as i32 / 8
        } else {
            w
        }
    }
    fn measure64(&self, f: FontSpec, text: &str) -> i32 {
        let w = font_for(f).width64(text);
        if f.italic {
            w + f.size as i32 * 8
        } else {
            w
        }
    }
    fn metrics(&self, f: FontSpec) -> Metrics {
        let font = font_for(f);
        Metrics { ascent: font.ascent as i32, descent: -(font.descent as i32) }
    }
    fn image_size(&self, src: &str) -> Option<(u32, u32)> {
        let key = resolve(self.base.as_ref(), src)?;
        match self.images.get(&key) {
            Some(Some(img)) => Some((img.width, img.height)),
            _ => None,
        }
    }
}

fn resolve(base: Option<&Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("data:") {
        return Some(String::from(href));
    }
    base?.join(href).map(|u| u.to_string())
}

// ------------------------------------------------------------------ the browser

struct Nebula {
    address: TextEdit,
    editing: bool,
    history: Vec<String>,
    pos: usize,
    url: Option<Url>,
    page: Option<Page>,
    layout: Option<Layout>,
    laid_out_width: i32,
    scroll: i32,
    view: Rect,
    hover: Option<String>,
    job: Option<(Arc<LoadJob>, Option<JoinHandle<()>>)>,
    old_jobs: Vec<JoinHandle<()>>,
    loading_since: u64,
    images: BTreeMap<String, Option<Image>>,
    queue: Arc<Mutex<ImageQueue>>,
    generation: Arc<AtomicU64>,
    workers: Vec<JoinHandle<()>>,
    relayout_at: Option<u64>,
    fields: BTreeMap<NodeId, TextEdit>,
    focus: Option<NodeId>,
    fragment: Option<String>,
    press: Option<(i32, i32)>,
    /// Page zoom in percent.
    zoom: i32,
    /// The cascade for the current viewport (reused when only images change).
    styles: Option<((i32, i32), nebula_engine::style::Computed)>,
    find: Option<Find>,
    /// The document version the current layout reflects (scripts bump it).
    laid_version: u64,
    /// When page scripts next want to run (Unix ms): timers, animation frames.
    js_wakeup: Option<f64>,
    /// The current page's JavaScript, if it has any.
    js: Option<Scripts>,
    /// Scripts that scripts inserted, as they finish loading:
    /// (page generation, element, URL, source if it loaded).
    script_loads: Arc<Mutex<Vec<ScriptLoad>>>,
    scripts_loading: usize,
    /// Answers to scripts' network requests: (page generation, id, result).
    request_results: Arc<Mutex<Vec<RequestResult>>>,
    /// Image URLs this page has asked for (decoded, failed or queued).
    requested: alloc::collections::BTreeSet<String>,
    /// Whether the page's canvases have been given the decoded images.
    canvas_images: bool,
    /// When the current page started loading, until its images are done.
    images_since: Option<u64>,
    /// The current load's timings and when its page was shown, until logged.
    timing: Option<(Timing, u64)>,
    /// Page scripts still to run, a slice per tick so the window stays
    /// responsive; and (when the first ran, when the last arrived).
    script_queue: VecDeque<(String, String)>,
    script_run: Option<(u64, u64)>,
    /// Images that finished while scripts were still starting: (URL, size).
    deferred_images: Vec<(String, Option<(u32, u32)>)>,
    /// A new page is on its way: the old one is frozen (no timers, image
    /// events or layout) so it can't hold up the new one.
    awaiting_page: bool,
    /// Images arrived while scripts were starting (lay out after them), and
    /// when the window last repainted for a script slice.
    images_during_scripts: bool,
    slice_painted: u64,
    /// This tab, for telling its storage changes from other tabs'.
    id: u64,
    storage: Hub,
    /// The last storage change this tab has seen.
    storage_seq: u64,
    /// Not the selected tab: no layout, and timers at most once a second.
    background: bool,
    /// When a background tab last ran its timers (Unix ms).
    timers_ran: f64,
    media: media::Players,
    logged_videos: usize,
}

/// Find in page: the query and its matches as (text item, byte range).
struct Find {
    edit: TextEdit,
    matches: Vec<(usize, usize, usize)>,
    current: usize,
}

const ZOOMS: [i32; 12] = [50, 67, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// Dropping a JoinHandle does not unmap its stack. Join finished workers.
fn reap(handles: &mut Vec<JoinHandle<()>>) {
    let mut i = 0;
    while i < handles.len() {
        if handles[i].is_finished() {
            handles.swap_remove(i).join();
        } else {
            i += 1;
        }
    }
}

struct Browser {
    tabs: Vec<Nebula>,
    active: usize,
    closed: Vec<String>,
    retired: Vec<JoinHandle<()>>,
    storage: Hub,
    next_id: u64,
    /// Until when (uptime ms) the tab strip says all eight tabs are open.
    notice_until: u64,
    /// The address pill shows a field being typed into (not host and path).
    address_open: bool,
}

impl Browser {
    fn new(start: &str) -> Self {
        let storage = Hub::default();
        let first = Nebula::new(start, 0, storage.clone());
        Self {
            tabs: alloc::vec![first],
            active: 0,
            closed: Vec::new(),
            retired: Vec::new(),
            storage,
            next_id: 1,
            notice_until: 0,
            address_open: false,
        }
    }

    fn select(&mut self, index: usize) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        tab.layout = None;
        tab.styles = None;
        tab.hover = None;
        tab.press = None;
        tab.background = true;
        self.active = index;
        self.tabs[index].background = false;
    }

    /// Opens `url` in a new tab, selected unless `background`. At the limit
    /// it says so instead and returns false.
    fn open(&mut self, url: &str, background: bool) -> bool {
        if self.tabs.len() >= MAX_TABS {
            self.notice_until = corekit::time::uptime_ms() + 3000;
            return false;
        }
        let mut tab = Nebula::new(url, self.next_id, self.storage.clone());
        self.next_id += 1;
        tab.background = background;
        self.tabs.push(tab);
        if !background {
            self.select(self.tabs.len() - 1);
            if url == HOME {
                self.tabs[self.active].editing = true;
                self.tabs[self.active].address.set_text("");
            }
        }
        true
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(index);
        if let Some(url) = tab.history.get(tab.pos) {
            self.closed.push(url.clone());
            if self.closed.len() > 8 {
                self.closed.remove(0);
            }
        }
        tab.stop();
        tab.media.clear(&mut tab.workers);
        tab.generation.fetch_add(1, Ordering::Relaxed);
        {
            let mut q = tab.queue.lock();
            q.queue.clear();
            q.done.clear();
        }
        self.retired.append(&mut tab.old_jobs);
        self.retired.append(&mut tab.workers);
        if self.tabs.is_empty() {
            self.tabs.push(Nebula::new(HOME, self.next_id, self.storage.clone()));
            self.next_id += 1;
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.tabs[self.active].background = false;
    }

    fn notice(&self) -> bool {
        corekit::time::uptime_ms() < self.notice_until
    }

    /// Ticks every tab; true if the selected one (or the tab strip) changed.
    fn tick_all(&mut self, now: u64) -> bool {
        reap(&mut self.retired);
        let showing = self.notice_until != 0;
        if showing && !self.notice() {
            self.notice_until = 0;
        }
        let mut dirty = showing && self.notice_until == 0;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == self.active {
                dirty |= tab.tick(now);
            } else {
                // Background tabs only repaint the tab strip.
                let before = (tab.title(), tab.loading());
                tab.tick(now);
                dirty |= (tab.title(), tab.loading()) != before;
            }
        }
        dirty
    }

    fn tick_interval(&self) -> u64 {
        let mut ms = self.tabs.iter().map(Nebula::tick_interval).min().unwrap_or(1000);
        if self.notice_until != 0 {
            ms = ms.min(self.notice_until.saturating_sub(corekit::time::uptime_ms()).max(16));
        }
        ms
    }

    /// Lays out and paints the selected tab into `cv` (the self-check's frame).
    #[allow(dead_code)]
    fn render(&mut self, cv: &mut Canvas, r: Rect) {
        let tab = &mut self.tabs[self.active];
        tab.prepare(r.w, r.h);
        tab.paint(cv, r);
    }
}

impl Nebula {
    fn new(start: &str, id: u64, storage: Hub) -> Nebula {
        let mut s = Nebula {
            address: TextEdit::new("", false),
            editing: false,
            history: Vec::new(),
            pos: 0,
            url: None,
            page: None,
            layout: None,
            laid_out_width: 0,
            scroll: 0,
            view: Rect::new(0, 0, 1100, 680),
            hover: None,
            job: None,
            old_jobs: Vec::new(),
            loading_since: 0,
            images: BTreeMap::new(),
            queue: Arc::new(Mutex::new(ImageQueue { queue: VecDeque::new(), done: Vec::new(), bytes: 0 })),
            generation: Arc::new(AtomicU64::new(0)),
            workers: Vec::new(),
            relayout_at: None,
            fields: BTreeMap::new(),
            focus: None,
            fragment: None,
            press: None,
            zoom: corekit::prefs::get_int("nebula_zoom", 100).clamp(50, 300) as i32,
            styles: None,
            find: None,
            laid_version: 0,
            js_wakeup: None,
            js: None,
            script_loads: Arc::new(Mutex::new(Vec::new())),
            scripts_loading: 0,
            request_results: Arc::new(Mutex::new(Vec::new())),
            requested: alloc::collections::BTreeSet::new(),
            canvas_images: false,
            images_since: None,
            timing: None,
            script_queue: VecDeque::new(),
            script_run: None,
            deferred_images: Vec::new(),
            awaiting_page: false,
            images_during_scripts: false,
            slice_painted: 0,
            id,
            storage,
            storage_seq: 0,
            background: false,
            timers_ran: 0.0,
            media: media::Players::default(),
            logged_videos: 0,
        };
        s.go(start, true);
        s
    }

    fn loading(&self) -> bool {
        self.job.is_some()
    }

    /// Turns what was typed into an address: a URL, a path, or a search.
    fn normalize(input: &str) -> String {
        let s = input.trim();
        if s.is_empty() {
            return String::from(HOME);
        }
        if s.contains("://") || s.starts_with("about:") || s.starts_with("data:") {
            return String::from(s);
        }
        if s.starts_with('/') {
            return format!("file://{s}");
        }
        let hostlike = !s.contains(' ') && (s.contains('.') || s.starts_with("localhost")) && !s.ends_with('.');
        if hostlike {
            format!("https://{s}")
        } else {
            format!("{SEARCH}{}", form_encode(s))
        }
    }

    /// Navigates to `input`, adding a history entry when `push`.
    fn go(&mut self, input: &str, push: bool) {
        let target = Self::normalize(input);
        if push {
            self.history.truncate(if self.history.is_empty() { 0 } else { self.pos + 1 });
            self.history.push(target.clone());
            if self.history.len() > 100 {
                self.history.remove(0);
            }
            self.pos = self.history.len() - 1;
        }
        self.start(target);
    }

    fn start(&mut self, target: String) {
        self.stop();
        self.editing = false;
        self.address.set_text(&target);
        let Some(url) = Url::parse(&target) else {
            self.show_error(&target, "that isn't a web address");
            return;
        };
        // Same document, different fragment: just scroll.
        if let (Some(cur), Some(frag)) = (&self.url, &url.fragment) {
            let mut a = cur.clone();
            let mut b = url.clone();
            a.fragment = None;
            b.fragment = None;
            if a == b && self.page.is_some() {
                self.url = Some(url.clone());
                self.scroll_to_fragment(&frag.clone());
                return;
            }
        }
        let job = Arc::new(LoadJob {
            cancel: AtomicBool::new(false),
            progress: Mutex::new(String::new()),
            result: Mutex::new(None),
            scripts: Mutex::new(None),
        });
        let j = job.clone();
        let u = url.clone();
        match corekit::thread::spawn(move || match load(u, &j) {
            Ok((loaded, pending)) => {
                *j.result.lock() = Some(Ok(loaded));
                if let Some(p) = pending {
                    let scripts = load_scripts(p, &j);
                    *j.scripts.lock() = Some((scripts, corekit::time::uptime_ms()));
                }
            }
            Err(e) => *j.result.lock() = Some(Err(e)),
        }) {
            Ok(h) => {
                self.job = Some((job, Some(h)));
                self.freeze();
                self.loading_since = corekit::time::uptime_ms();
                self.fragment = url.fragment.clone();
            }
            Err(e) => self.show_error(&target, &format!("couldn't start loading ({e})")),
        }
    }

    /// Stops the current page's background work while the next one loads.
    fn freeze(&mut self) {
        self.awaiting_page = true;
        self.generation.fetch_add(1, Ordering::Relaxed);
        {
            let mut q = self.queue.lock();
            q.queue.clear();
            q.done.clear();
        }
        self.relayout_at = None;
        self.js_wakeup = None;
        self.script_queue.clear();
        self.script_run = None;
        self.deferred_images.clear();
        self.images_since = None;
        self.images_during_scripts = false;
    }

    /// The load thread is done: join it.
    fn finish_job(&mut self) {
        if let Some((_, Some(h))) = self.job.take() {
            h.join();
        }
    }

    fn stop(&mut self) {
        if let Some((job, h)) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            if let Some(h) = h {
                self.old_jobs.push(h);
            }
        }
    }

    fn show_error(&mut self, url: &str, msg: &str) {
        let html = format!(
            "<html><head><title>Can't open the page</title></head><body style='margin:0;background:#f4f5f9;font-family:sans-serif'>\
             <div style='max-width:560px;margin:80px auto;padding:28px 32px;background:#fff;border:1px solid #e1e1e8;border-radius:14px'>\
             <h1 style='font-size:22px;margin:0 0 10px'>Nebula can't open this page</h1>\
             <p style='color:#50505a;margin:0 0 14px'>{}.</p><p style='color:#8a8a96;font-size:13px;margin:0'>{}</p></div></body></html>",
            escape(&capitalize(msg)),
            escape(url)
        );
        self.install(Loaded {
            timing: Timing::default(),
            page: Page::parse(&html),
            url: Url::parse(url).unwrap_or_else(|| Url::parse(HOME).unwrap()),
            images: Vec::new(),
            scripts: Vec::new(),
            pending: false,
        });
    }

    fn install(&mut self, loaded: Loaded) {
        self.media.clear(&mut self.workers);
        self.find = None;
        self.styles = None;
        self.layout = None;
        let Loaded { timing, page, url, images, scripts, pending } = loaded;
        self.awaiting_page = false;
        self.generation.fetch_add(1, Ordering::Relaxed);
        {
            let mut q = self.queue.lock();
            q.queue.clear();
            q.done.clear();
            q.bytes = 0;
        }
        self.images.clear();
        for (k, img) in images {
            let img = shrink(img, 1200);
            self.queue.lock().bytes += img.pixels.len() * 4;
            self.images.insert(k, Some(img));
        }
        self.fields.clear();
        self.focus = None;
        self.scroll = 0;
        self.hover = None;
        let url_string = url.to_string();
        self.js = None;
        self.script_queue.clear();
        self.script_run = None;
        self.deferred_images.clear();
        if !self.editing {
            self.address.set_text(&url_string);
        }
        if let Some(h) = self.history.get_mut(self.pos) {
            *h = url_string;
        }
        self.requested.clear();
        self.canvas_images = false;
        let refresh = page.refresh();
        self.page = Some(page);
        self.url = Some(url);
        self.laid_version = 0;
        self.js_wakeup = None;
        if pending {
            // Shown now, without scripts; they run when they arrive.
            self.relayout();
            self.timing = Some((timing, corekit::time::uptime_ms()));
        } else {
            self.timing = Some((timing, corekit::time::uptime_ms()));
            self.start_scripts(scripts, 0);
        }
        self.queue_images();
        // A background tab scrolls to its fragment once it's shown and laid out.
        if !self.background {
            if let Some(frag) = self.fragment.take() {
                self.scroll_to_fragment(&frag);
            }
        }
        if let Some((0, target)) = refresh {
            if !target.is_empty() {
                if let Some(u) = self.url.as_ref().and_then(|u| u.join(&target)) {
                    self.go(&u.to_string(), false);
                }
            }
        }
    }

    /// Starts the page's scripts (as it finishes loading). They run in
    /// slices between frames; then the page finishes loading and is laid out.
    /// `arrived`: when the last script finished downloading (0: with the page).
    fn start_scripts(&mut self, scripts: Vec<NamedSource>, arrived: u64) {
        let Some(page) = self.page.as_mut() else { return };
        let Some(url) = self.url.clone() else { return };
        let has_handlers = page.doc.nodes.iter().any(|n| match &n.data {
            nebula_engine::dom::NodeData::Element(e) => e.attrs.iter().any(|(k, _)| k.starts_with("on")),
            _ => false,
        });
        self.script_queue.clear();
        if javascript_enabled() && (!scripts.is_empty() || has_handlers) {
            let (w, h) = ((self.view.w * 100 / self.zoom).max(200), (self.view.h * 100 / self.zoom).max(200));
            let stored = self.storage.borrow_mut().get(&url);
            self.storage_seq = self.storage.borrow().seq;
            let mut js = Scripts::new(alloc::boxed::Box::new(PageHost), &url.to_string(), (w, h), stored);
            js.set_cookies(alloc::boxed::Box::new(BrowserCookies));
            js.set_text_host(alloc::boxed::Box::new(CanvasFonts));
            js.set_code_cache(self.storage.borrow().code.clone());
            // Preference `nebula_profile`: sample where page scripts spend time.
            if corekit::prefs::get_int("nebula_profile", 0) != 0 {
                js.realm.profile = Some(BTreeMap::new());
            }
            self.js = Some(js);
            self.script_queue.extend(scripts);
        }
        self.script_run = Some((corekit::time::uptime_ms(), arrived));
        self.run_scripts_slice();
    }

    /// Runs queued page scripts for up to a frame's worth of time (at least
    /// one script); after the last, finishes loading the page.
    fn run_scripts_slice(&mut self) {
        let Some((started, arrived)) = self.script_run else { return };
        let slice = corekit::time::uptime_ms();
        if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
            while let Some((name, code)) = self.script_queue.pop_front() {
                let t = corekit::time::uptime_ms();
                page.run_script(js, &code, &name);
                let ms = corekit::time::uptime_ms() - t;
                if ms >= 300 {
                    corekit::io::log(&format!("[nebula] slow script {ms} ms ({} KB): {name}\n", code.len() >> 10));
                }
                if corekit::time::uptime_ms() - slice >= 40 {
                    break;
                }
            }
            if !self.script_queue.is_empty() {
                return;
            }
            page.finish_loading(js);
        }
        self.script_queue.clear();
        self.script_run = None;
        let ran = corekit::time::uptime_ms() - started;
        // Load and error events for images that came in meanwhile.
        let page_url = self.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
        for (url, size) in core::mem::take(&mut self.deferred_images) {
            if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                page.image_loaded(js, &page_url, &url, size);
                if let (true, Some(Some(i))) = (self.canvas_images, self.images.get(&url)) {
                    js.set_image(&url, i.width, i.height, &i.pixels);
                }
            }
        }
        self.after_scripts();
        let images = core::mem::take(&mut self.images_during_scripts);
        self.spawn_workers();
        if images || self.layout.is_none() || self.page.as_ref().is_some_and(|p| p.doc.version != self.laid_version) {
            self.relayout();
        }
        self.log_timing(ran, corekit::time::uptime_ms() - started - ran, arrived);
        self.queue_images();
    }

    /// One line per load: where the time went.
    fn log_timing(&mut self, ran: u64, layout: u64, arrived: u64) {
        if let Some(p) = self.js.as_mut().and_then(|js| js.realm.profile.take()) {
            let total: u64 = p.values().sum::<u64>().max(1);
            let mut v: Vec<_> = p.into_iter().collect();
            v.sort_by_key(|e| core::cmp::Reverse(e.1));
            let mut out = format!("[nebula] profile: {total} samples\n");
            for ((name, file, line, col), n) in v.iter().take(30) {
                let file: String = file.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
                out.push_str(&format!(
                    "[nebula]   {:5.1}% {name} …{file}:{line}:{col}\n",
                    *n as f64 * 100.0 / total as f64
                ));
            }
            corekit::io::log(&out);
        }
        let Some((t, shown)) = self.timing.take() else { return };
        if t.started == 0 {
            return;
        }
        let now = corekit::time::uptime_ms();
        let n = corekit::web::stats();
        let (hits, revalidated, misses) = *cache::STATS.lock();
        corekit::io::log(&format!(
            "[nebula] timing {} total={} paint={} document={} styles={}({}) scripts={}({}) run={} layout={} bytes={} \
             opened={} reused={} dns={} connect={} tls={} cache={}/{}/{}\n",
            self.url.as_ref().map(|u| u.to_string()).unwrap_or_default(),
            now - t.started,
            shown - t.started,
            t.document,
            t.styles - t.document,
            t.style_count,
            if arrived > 0 { arrived.saturating_sub(t.started + t.styles) } else { 0 },
            t.script_count,
            ran,
            layout,
            t.bytes,
            n.opened - t.net.opened,
            n.reused - t.net.reused,
            n.dns_ms - t.net.dns_ms,
            n.connect_ms - t.net.connect_ms,
            n.tls_ms - t.net.tls_ms,
            hits,
            revalidated,
            misses,
        ));
        self.images_since = Some(t.started);
    }

    /// Queues the page's images not asked for yet: at first, and again when
    /// scripts add images (or set the `src` of `new Image()`).
    fn queue_images(&mut self) {
        let (Some(page), Some(url)) = (&self.page, &self.url) else { return };
        let base = document_base(page, url);
        let g = self.generation.load(Ordering::Relaxed);
        let mut added = false;
        {
            let mut q = self.queue.lock();
            for s in page.images().iter().filter_map(|s| resolve(Some(&base), s)) {
                if self.requested.len() >= MAX_IMAGES {
                    break;
                }
                if self.requested.insert(s.clone()) && !self.images.contains_key(&s) {
                    q.queue.push_back((g, s));
                    added = true;
                }
            }
        }
        if added {
            self.spawn_workers();
        }
    }

    fn spawn_workers(&mut self) {
        reap(&mut self.workers);
        // One decoder while the page's scripts are starting: they share the CPU.
        let limit = if self.script_run.is_some() { 1 } else { IMAGE_WORKERS };
        while self.workers.len() < limit && !self.queue.lock().queue.is_empty() {
            let (q, g) = (self.queue.clone(), self.generation.clone());
            match corekit::thread::spawn(move || image_worker(q, g)) {
                Ok(h) => self.workers.push(h),
                Err(_) => break,
            }
        }
    }

    /// Follows up on what scripts did: navigation, storage, inserted
    /// scripts, scrolling, DOM changes.
    fn after_scripts(&mut self) {
        let Some(page) = self.page.as_mut() else { return };
        let Some(js) = self.js.as_mut() else { return };
        // Scripts that scripts inserted load in the background.
        let base = self.url.as_ref().map(|u| document_base(page, u));
        for (node, src) in js.take_fetches() {
            let Some(u) = base.as_ref().and_then(|b| b.join(&src)) else { continue };
            let (loads, g) = (self.script_loads.clone(), self.generation.load(Ordering::Relaxed));
            self.scripts_loading += 1;
            let spawned = corekit::thread::spawn(move || {
                let source = match get(&u) {
                    Ok(r) if r.ok() => Some(nebula_engine::decode(&r.body, "")),
                    _ => None,
                };
                loads.lock().push((g, node, u.to_string(), source));
            });
            match spawned {
                Ok(h) => self.old_jobs.push(h),
                Err(_) => self.scripts_loading -= 1,
            }
        }
        // fetch() and XMLHttpRequest, also in the background.
        let page_origin = self.url.as_ref().map(origin).unwrap_or_default();
        for req in js.take_requests() {
            let (results, g) = (self.request_results.clone(), self.generation.load(Ordering::Relaxed));
            let mut cookies = SameOrigin(page_origin.clone());
            let Some(u) = base.as_ref().and_then(|b| b.join(&req.url)) else {
                results.lock().push((g, req.id, Err(String::from("bad URL"))));
                continue;
            };
            self.scripts_loading += 1;
            let spawned = corekit::thread::spawn(move || {
                let r = if u.scheme == "file" {
                    get(&u)
                } else {
                    let body = &req.body[..];
                    let pool = &mut corekit::web::Pooled;
                    http::request_with_cookies(&req.method, &u, &req.headers, body, pool, &mut cookies)
                        .map_err(describe)
                };
                let r = r.map(|resp| nebula_engine::script::HttpResponse {
                    status: resp.status,
                    status_text: resp.reason.clone(),
                    url: resp.url.to_string(),
                    body: resp.body,
                    headers: resp.headers,
                });
                results.lock().push((g, req.id, r));
            });
            match spawned {
                Ok(h) => self.old_jobs.push(h),
                Err(_) => self.scripts_loading -= 1,
            }
        }
        let scroll_to = js.take_scroll_request().map(|(_, y)| y * self.zoom / 100);
        self.js_wakeup = js.next_wakeup(corekit::time::unix_ms());
        if let Some(m) = js.take_storage() {
            if let Some(u) = &self.url {
                self.storage.borrow_mut().write(self.id, u, m);
            }
        }
        if let Some(target) = js.take_navigation() {
            if target == "about:back" {
                self.back();
            } else if let Some(u) = self.url.as_ref().and_then(|u| u.join(&target)) {
                self.go(&u.to_string(), true);
            }
            return;
        }
        // Values scripts set show in the fields being edited.
        for (node, edit) in self.fields.iter_mut() {
            if let Some(v) = page.doc.values.get(node) {
                if edit.text() != v {
                    edit.set_text(v);
                }
            }
        }
        if page.doc.version != self.laid_version {
            self.queue_images();
            self.request_relayout();
        }
        // A page that starts drawing gets the images decoded so far.
        if !self.canvas_images && self.js.as_mut().is_some_and(|js| js.uses_canvas()) {
            self.canvas_images = true;
            if let Some(js) = self.js.as_mut() {
                for (u, img) in &self.images {
                    if let Some(img) = img {
                        js.set_image(u, img.width, img.height, &img.pixels);
                    }
                }
            }
        }
        if let Some(y) = scroll_to {
            self.scroll = y;
            self.clamp_scroll();
        }
    }

    /// Lays out again soon: changes from scripts and events within 50 ms
    /// share one layout (at once when there's none yet).
    fn request_relayout(&mut self) {
        if self.layout.is_none() {
            self.relayout();
            return;
        }
        let soon = corekit::time::uptime_ms() + 50;
        self.relayout_at = Some(self.relayout_at.map_or(soon, |t| t.min(soon)));
    }

    fn relayout(&mut self) {
        self.relayout_at = None;
        // Background tabs lay out when they're selected.
        if self.background {
            return;
        }
        let Some(page) = &self.page else { return };
        if page.doc.version != self.laid_version {
            // The tree changed: the cascade must run again.
            self.styles = None;
            self.laid_version = page.doc.version;
        }
        let base = self.url.as_ref().map(|u| document_base(page, u));
        let host = Fonts { images: &self.images, base };
        let w = (self.view.w * 100 / self.zoom).max(200);
        let h = (self.view.h * 100 / self.zoom).max(200);
        let t0 = corekit::time::uptime_ms();
        if self.styles.as_ref().is_none_or(|(size, _)| *size != (w, h)) {
            self.styles = Some(((w, h), page.styles(w, h)));
        }
        let t1 = corekit::time::uptime_ms();
        let l = page.layout_viewport(&self.styles.as_ref().unwrap().1, w, h, &host);
        let t2 = corekit::time::uptime_ms();
        if t2 - t0 >= 500 {
            corekit::io::log(&format!(
                "[nebula] slow layout: cascade={} layout={} nodes={} items={}\n",
                t1 - t0,
                t2 - t1,
                page.doc.nodes.len(),
                l.items.len()
            ));
        }
        self.laid_out_width = self.view.w;
        let videos: Vec<_> = l
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Video { rect, node } => Some((*node, rect.x, rect.y, rect.w, rect.h)),
                _ => None,
            })
            .collect();
        if !videos.is_empty() && videos.len() != self.logged_videos {
            self.logged_videos = videos.len();
            corekit::io::log(&format!("[nebula] video boxes {videos:?} zoom={} view={}x{}\n", self.zoom, w, h));
        }
        if let Some(js) = self.js.as_mut() {
            js.set_viewport(w, h);
            js.set_layout(&l, (0, self.scroll * 100 / self.zoom));
        }
        self.layout = Some(l);
        self.clamp_scroll();
        if self.find.is_some() {
            self.search(false);
        }
    }

    /// Page (CSS) pixels to screen pixels.
    fn z(&self, v: i32) -> i32 {
        v * self.zoom / 100
    }

    fn set_zoom(&mut self, zoom: i32) {
        let zoom = zoom.clamp(50, 300);
        if zoom == self.zoom {
            return;
        }
        // Keep the same part of the page in view.
        let frac = if self.max_scroll() > 0 { self.scroll as i64 * 1000 / self.max_scroll() as i64 } else { 0 };
        self.zoom = zoom;
        let _ = corekit::prefs::set("nebula_zoom", &format!("{zoom}"));
        self.relayout();
        self.scroll = (frac * self.max_scroll() as i64 / 1000) as i32;
        self.clamp_scroll();
    }

    fn zoom_step(&mut self, dir: i32) {
        let i = ZOOMS.iter().position(|z| *z >= self.zoom).unwrap_or(4) as i32;
        let i = if ZOOMS[i as usize] != self.zoom && dir < 0 { i - 1 } else { i + dir };
        self.set_zoom(ZOOMS[i.clamp(0, ZOOMS.len() as i32 - 1) as usize]);
    }

    /// Finds the query in the page's text; `jump` scrolls to the first match below the view's top.
    fn search(&mut self, jump: bool) {
        let Some(f) = &mut self.find else { return };
        f.matches.clear();
        let q = f.edit.text().to_ascii_lowercase();
        if q.is_empty() {
            return;
        }
        let Some(l) = &self.layout else { return };
        for (i, item) in l.items.iter().enumerate() {
            if let Item::Text { text, .. } = item {
                let lower = text.to_ascii_lowercase();
                let mut from = 0;
                while let Some(p) = lower[from..].find(&q) {
                    f.matches.push((i, from + p, from + p + q.len()));
                    from += p + q.len().max(1);
                }
            }
        }
        if jump {
            let top = self.scroll * 100 / self.zoom;
            let y_of = |m: &(usize, usize, usize)| match &l.items[m.0] {
                Item::Text { y, .. } => *y,
                _ => 0,
            };
            f.current = f.matches.iter().position(|m| y_of(m) >= top).unwrap_or(0);
            self.reveal_match();
        } else if f.current >= f.matches.len() {
            f.current = 0;
        }
    }

    fn reveal_match(&mut self) {
        let Some(f) = &self.find else { return };
        let Some(&(i, _, _)) = f.matches.get(f.current) else { return };
        if let Some(Item::Text { y, .. }) = self.layout.as_ref().map(|l| &l.items[i]) {
            let sy = self.z(*y);
            if sy < self.scroll + 20 || sy > self.scroll + self.view.h - 20 {
                self.scroll = sy - self.view.h / 3;
                self.clamp_scroll();
            }
        }
    }

    fn find_step(&mut self, dir: i32) {
        if let Some(f) = &mut self.find {
            let n = f.matches.len() as i32;
            if n > 0 {
                f.current = (f.current as i32 + dir).rem_euclid(n) as usize;
            }
        }
        self.reveal_match();
    }

    fn max_scroll(&self) -> i32 {
        self.layout.as_ref().map_or(0, |l| (self.z(l.height) - self.view.h).max(0))
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    fn scroll_to_fragment(&mut self, frag: &str) {
        let name = nebula_web::url::percent_decode(frag, false);
        if let Some(y) = self.layout.as_ref().and_then(|l| l.anchor(&name)) {
            self.scroll = self.z(y);
            self.clamp_scroll();
        } else if frag.is_empty() || frag == "top" {
            self.scroll = 0;
        }
    }

    fn back(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
            let t = self.history[self.pos].clone();
            self.go(&t, false);
        }
    }

    fn forward(&mut self) {
        if self.pos + 1 < self.history.len() {
            self.pos += 1;
            let t = self.history[self.pos].clone();
            self.go(&t, false);
        }
    }

    fn reload(&mut self) {
        if let Some(t) = self.history.get(self.pos).cloned() {
            self.go(&t, false);
        }
    }

    fn doc(&self) -> Option<&Document> {
        self.page.as_ref().map(|p| &p.doc)
    }

    /// The absolute URL of an <a> element.
    fn href(&self, node: NodeId) -> Option<String> {
        let page = self.page.as_ref()?;
        let href = page.element(node)?.attr("href")?;
        let base = document_base(page, self.url.as_ref()?);
        let href = href.trim();
        if href.starts_with("javascript:") {
            return None;
        }
        base.join(href).map(|u| u.to_string())
    }

    fn follow(&mut self, node: NodeId) {
        if let Some(target) = self.href(node) {
            if target.starts_with("mailto:") || target.starts_with("tel:") {
                return;
            }
            self.go(&target, true);
        }
    }

    // ------------------------------------------------------------ forms

    fn form_of(&self, node: NodeId) -> Option<NodeId> {
        let doc = self.doc()?;
        let mut p = doc.parent(node);
        while let Some(n) = p {
            if doc.tag(n) == "form" {
                return Some(n);
            }
            p = doc.parent(n);
        }
        None
    }

    fn field_value(&self, node: NodeId) -> String {
        if let Some(e) = self.fields.get(&node) {
            return String::from(e.text());
        }
        nebula_engine::script::control_value(self.doc().unwrap(), node)
    }

    /// Submits the form containing `from` (GET; POST forms are sent as GET too).
    fn submit(&mut self, from: NodeId) {
        let Some(form) = self.form_of(from) else { return };
        if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
            let go_on = page.dispatch(js, form, "submit", true, true);
            self.after_scripts();
            if !go_on {
                return;
            }
        }
        let Some(doc) = self.doc() else { return };
        let fe = doc.element(form).unwrap();
        let action = fe.attr("action").unwrap_or("").trim();
        let base = document_base(self.page.as_ref().unwrap(), self.url.as_ref().unwrap());
        let Some(mut target) = (if action.is_empty() { Some(base.clone()) } else { base.join(action) }) else { return };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for n in doc.descendants(form) {
            let Some(e) = doc.element(n) else { continue };
            let Some(name) = e.attr("name") else { continue };
            if e.attr("disabled").is_some() {
                continue;
            }
            let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
            match e.tag.as_str() {
                "input" => match kind.as_str() {
                    "submit" | "image" | "button" | "reset" | "file" => {
                        if n == from && kind != "reset" {
                            pairs.push((String::from(name), String::from(e.attr("value").unwrap_or(""))));
                        }
                    }
                    "checkbox" | "radio" => {
                        if e.attr("checked").is_some() {
                            pairs.push((String::from(name), String::from(e.attr("value").unwrap_or("on"))));
                        }
                    }
                    _ => pairs.push((String::from(name), self.field_value(n))),
                },
                "button" if n == from => pairs.push((String::from(name), String::from(e.attr("value").unwrap_or("")))),
                "select" | "textarea" => pairs.push((String::from(name), self.field_value(n))),
                _ => {}
            }
        }
        let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect();
        let path = target.path.split('?').next().unwrap_or("/").to_string();
        target.path = format!("{}?{}", path, query.join("&"));
        target.fragment = None;
        self.go(&target.to_string(), true);
    }

    // ------------------------------------------------------------ geometry

    /// The toolbar row's top-left corner (the view sits right below it).
    /// Page coordinates of a window point in the content area.
    fn page_point(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        (self.view.contains(x, y))
            .then(|| ((x - self.view.x) * 100 / self.zoom, (y - self.view.y + self.scroll) * 100 / self.zoom))
    }

    // ------------------------------------------------------------ painting

    fn paint_page(&self, cv: &mut Canvas, origin: Rect) {
        let v = self.view.offset(origin.x, origin.y);
        let Some(l) = &self.layout else {
            cv.fill_rect(v, 0xFFFF_FFFF);
            return;
        };
        let bg = if l.background >> 24 == 0xFF { l.background } else { 0xFFFF_FFFF };
        cv.fill_rect(v, 0xFFFF_FFFF);
        cv.fill_rect(v, bg);
        let (ox, oy) = (v.x, v.y - self.scroll);
        let base = self.page.as_ref().zip(self.url.as_ref()).map(|(p, u)| document_base(p, u));
        let visible = |r: Rect| r.y + r.h >= v.y && r.y < v.bottom();
        let zm = self.zoom;
        let z = |v: i32| v * zm / 100;
        // A page rectangle on screen (never collapsing a visible one to nothing).
        let zr = |r: &nebula_engine::Rect| {
            let (x0, y0) = (ox + z(r.x), oy + z(r.y));
            let (x1, y1) = (ox + z(r.x + r.w), oy + z(r.y + r.h));
            Rect::new(x0, y0, (x1 - x0).max((r.w > 0) as i32), (y1 - y0).max((r.h > 0) as i32))
        };
        let zfont = |f: FontSpec| FontSpec { size: ((f.size as i32 * zm / 100).max(1)) as u16, ..f };
        cv.with_clip(v, |cv| {
            for item in &l.items {
                match item {
                    Item::Border { rect, color, radius, width } => {
                        let r = zr(rect);
                        if !visible(r) {
                            continue;
                        }
                        let radius = z(*radius).min(r.w / 2).min(r.h / 2);
                        for inset in 0..z(*width).max(1).min(r.w.min(r.h) / 2) {
                            cv.stroke_round_rect(
                                Rect::new(r.x + inset, r.y + inset, r.w - 2 * inset, r.h - 2 * inset),
                                (radius - inset).max(0),
                                *color,
                            );
                        }
                    }
                    Item::Rect { rect, color, radius } => {
                        if *color >> 24 == 0 {
                            continue;
                        }
                        let r = zr(rect);
                        if !visible(r) {
                            continue;
                        }
                        if *radius > 0 {
                            cv.fill_round_rect(r, z(*radius).min(r.h / 2).min(r.w / 2), *color);
                        } else {
                            cv.fill_rect(r, *color);
                        }
                    }
                    Item::Text { x, y, text, font, color, underline, strike } => {
                        let by = oy + z(*y);
                        let font = &zfont(*font);
                        let size = font.size as i32;
                        if by + size < v.y || by - size * 2 > v.bottom() {
                            continue;
                        }
                        let f = font_for(*font);
                        let bx = ox + z(*x);
                        let w = if font.italic {
                            cv.text_slanted(bx, by, text, f, *color)
                        } else {
                            cv.text(bx, by, text, f, *color)
                        };
                        let t = (size / 14).max(1);
                        if *underline {
                            cv.fill_rect(Rect::new(bx, by + (size / 8).max(1) + 1, w, t), *color);
                        }
                        if *strike {
                            cv.fill_rect(Rect::new(bx, by - f.ascent as i32 * 3 / 10, w, t), *color);
                        }
                    }
                    Item::Video { rect, node } => {
                        let r = zr(rect);
                        if visible(r) {
                            if let Some(page) = &self.page {
                                self.media.paint(cv, r, *node, page);
                            }
                        }
                    }
                    Item::Canvas { rect, node } => {
                        let r = zr(rect);
                        if visible(r) {
                            if let Some(bitmap) = self.page.as_ref().and_then(|p| p.doc.canvases.get(node)) {
                                if bitmap.width > 0 && bitmap.height > 0 {
                                    cv.draw_image(&bitmap.pixels, bitmap.width, bitmap.height, r, 255);
                                }
                            }
                        }
                    }
                    Item::Image { rect, src } => {
                        let r = zr(rect);
                        if !visible(r) {
                            continue;
                        }
                        let key = resolve(base.as_ref(), src);
                        match key.as_ref().and_then(|k| self.images.get(k)) {
                            Some(Some(img)) => cv.draw_image(&img.pixels, img.width, img.height, r, 255),
                            // Couldn't load (or an unsupported format): leave the space empty.
                            Some(None) => {}
                            None => cv.fill_rect(r, 0x18808088),
                        }
                    }
                    Item::Bullet { rect, color, kind } => {
                        let r = zr(rect);
                        if !visible(r) {
                            continue;
                        }
                        match kind {
                            ListStyle::Square => cv.fill_rect(r, *color),
                            ListStyle::Circle => cv.stroke_round_rect(r, r.w / 2, *color),
                            _ => cv.fill_circle(r.x + r.w / 2, r.y + r.h / 2, r.w / 2, *color),
                        }
                    }
                }
            }
            // Typed text in form fields.
            for (node, edit) in &self.fields {
                let Some(f) = l.fields.iter().find(|f| f.node == *node) else { continue };
                let fr = zr(&f.rect);
                let r = Rect::new(fr.x + 2, fr.y + 2, fr.w - 4, fr.h - 4);
                if !visible(r) {
                    continue;
                }
                let style = self.styles.as_ref().and_then(|(_, c)| c.styles.get(*node)).and_then(|s| s.as_ref());
                let spec = style.map(|s| FontSpec::of(s));
                let font = spec.map(|f| font_for(zfont(f))).unwrap_or_else(|| theme::ui(z(14).max(1) as u16));
                let bg =
                    style.map_or(0xFFFF_FFFF, |s| if s.background >> 24 == 0 { 0xFFFF_FFFF } else { s.background });
                let color = style.map_or(0xFF1D_1D24, |s| s.color);
                let radius = style.map_or(0, |s| z(s.radius as i32).min(fr.h / 2));
                let padding = style.map_or(3, |s| z(s.padding[3].or_zero(f.rect.w)).max(3));
                cv.fill_round_rect(r, (radius - 2).max(0), bg);
                let base_y = r.y + (r.h + font.ascent as i32 + font.descent as i32) / 2;
                cv.with_clip(r, |cv| {
                    let caret_x = font.width(&edit.text()[..edit.caret()]);
                    let shift = (caret_x - (r.w - padding - 5)).max(0);
                    let tx = r.x + padding - shift;
                    if let Some((a, b)) = edit.selection() {
                        let (xa, xb) = (font.width(&edit.text()[..a]), font.width(&edit.text()[..b]));
                        cv.fill_rect(
                            Rect::new(tx + xa, r.y + 2, xb - xa, r.h - 4),
                            theme::accent() & 0x00FF_FFFF | 0x5000_0000,
                        );
                    }
                    cv.text(tx, base_y, edit.text(), font, color);
                    if self.focus == Some(*node) {
                        cv.fill_rect(Rect::new(tx + caret_x, r.y + 3, 1, r.h - 6), theme::accent());
                    }
                });
            }
        });
        // Find highlights.
        if let Some(f) = &self.find {
            cv.with_clip(v, |cv| {
                for (k, &(i, a, b)) in f.matches.iter().enumerate() {
                    let Item::Text { x, y, text, font, .. } = &l.items[i] else { continue };
                    let by = oy + z(*y);
                    if by < v.y - 40 || by > v.bottom() + 40 {
                        continue;
                    }
                    let ft = font_for(zfont(*font));
                    let hx = ox + z(*x) + ft.width(&text[..a]);
                    let hw = ft.width(&text[a..b]);
                    let color = if k == f.current { 0xB0FF_9500 } else { 0x80FF_E14D };
                    cv.fill_round_rect(
                        Rect::new(hx - 1, by - ft.ascent as i32 - 1, hw + 2, ft.ascent as i32 - ft.descent as i32 + 2),
                        3,
                        color,
                    );
                }
            });
        }
        // A slim scroll indicator.
        if let Some(l) = &self.layout {
            let height = self.z(l.height);
            if height > v.h {
                let h = (v.h * v.h / height).max(24);
                let y = v.y + (v.h - h) * self.scroll / self.max_scroll().max(1);
                cv.fill_round_rect(Rect::new(v.right() - 7, y + 2, 5, h - 4), 2, 0x60000000);
            }
        }
    }

    fn paint_status(&self, cv: &mut Canvas, origin: Rect) {
        let text = match (&self.hover, &self.job) {
            (Some(h), _) => h.clone(),
            (None, Some((job, _))) => job.progress.lock().clone(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let t = theme::current();
        let font = theme::ui(12);
        let v = self.view.offset(origin.x, origin.y);
        let w = (font.width(&text) + 20).min(v.w - 20);
        let r = Rect::new(v.x + 8, v.bottom() - 30, w, 24);
        cv.fill_round_rect(r, 7, t.panel_tint | 0xFF00_0000);
        cv.stroke_round_rect(r, 7, t.separator);
        cv.text_clipped(r.x + 10, r.y + 16, &text, font, t.text, r.w - 20);
    }

    fn page_key(&mut self, ev: &KeyEvent) -> bool {
        let page_step = self.view.h - 60;
        let before = self.scroll;
        match ev.code {
            KeyCode::Down => self.scroll += 48,
            KeyCode::Up => self.scroll -= 48,
            KeyCode::PageDown => self.scroll += page_step,
            KeyCode::PageUp => self.scroll -= page_step,
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = i32::MAX / 2,
            KeyCode::Char if ev.ch == Some(' ') => self.scroll += if ev.mods.shift { -page_step } else { page_step },
            KeyCode::Backspace => {
                self.back();
                return true;
            }
            _ => return false,
        }
        self.clamp_scroll();
        self.scroll != before
    }
}

impl Nebula {
    fn title(&self) -> String {
        match self.page.as_ref().map(|p| p.title.as_str()) {
            Some(t) if !t.is_empty() => String::from(t),
            _ if self.loading() => String::from("Loading…"),
            _ => String::from("Nebula"),
        }
    }

    /// The page area is `w` × `h`: lays the page out again if that changed.
    fn prepare(&mut self, w: i32, h: i32) {
        let old = self.view;
        self.view = Rect::new(0, 0, w.max(1), h.max(1));
        if self.page.is_some() && (self.layout.is_none() || self.laid_out_width != self.view.w || old.h != self.view.h)
        {
            self.relayout();
        }
        if !self.loading() && self.layout.is_some() {
            if let Some(frag) = self.fragment.take() {
                self.scroll_to_fragment(&frag);
            }
        }
    }

    /// Paints the page (and the link under the pointer) at `origin`.
    fn paint(&self, cv: &mut Canvas, origin: Rect) {
        self.paint_page(cv, origin);
        self.paint_status(cv, origin);
    }

    fn key(&mut self, ev: &KeyEvent, now: u64) -> bool {
        if !ev.pressed {
            return false;
        }
        let (ctrl, alt) = (ev.mods.ctrl || ev.mods.super_key, ev.mods.alt);
        // Browser shortcuts.
        if ctrl && ev.code == KeyCode::Char {
            match ev.ch.map(|c| c.to_ascii_lowercase()) {
                Some('f') => {
                    let f = self.find.get_or_insert_with(|| Find {
                        edit: TextEdit::new("", false),
                        matches: Vec::new(),
                        current: 0,
                    });
                    f.edit.select_all();
                    self.editing = false;
                    self.focus = None;
                    return true;
                }
                Some('=') | Some('+') => {
                    self.zoom_step(1);
                    return true;
                }
                Some('-') => {
                    self.zoom_step(-1);
                    return true;
                }
                Some('0') => {
                    self.set_zoom(100);
                    return true;
                }
                Some('g') => {
                    self.find_step(if ev.mods.shift { -1 } else { 1 });
                    return true;
                }
                Some('l') => {
                    self.find = None;
                    self.editing = true;
                    self.focus = None;
                    self.address.select_all();
                    return true;
                }
                Some('r') => {
                    self.reload();
                    return true;
                }
                Some('[') => {
                    self.back();
                    return true;
                }
                Some(']') => {
                    self.forward();
                    return true;
                }
                _ => {}
            }
        }
        if ev.code == KeyCode::F5 {
            self.reload();
            return true;
        }
        if alt && ev.code == KeyCode::Left {
            self.back();
            return true;
        }
        if alt && ev.code == KeyCode::Right {
            self.forward();
            return true;
        }
        if alt && ev.code == KeyCode::Home {
            self.go(HOME, true);
            return true;
        }
        if let (Some(f), false, None) = (&mut self.find, self.editing, self.focus) {
            match ev.code {
                KeyCode::Enter => {
                    self.find_step(if ev.mods.shift { -1 } else { 1 });
                    return true;
                }
                KeyCode::Escape => {
                    self.find = None;
                    return true;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {}
                _ => {
                    let r = f.edit.handle_key(ev, now);
                    if r.edited {
                        self.search(true);
                    }
                    if r.handled {
                        return true;
                    }
                }
            }
        }
        if self.editing {
            match ev.code {
                KeyCode::Enter => {
                    let t = String::from(self.address.text());
                    self.go(&t, true);
                    return true;
                }
                KeyCode::Escape => {
                    self.editing = false;
                    let cur = self.history.get(self.pos).cloned().unwrap_or_default();
                    self.address.set_text(&cur);
                    return true;
                }
                _ => return self.address.handle_key(ev, now).handled,
            }
        }
        if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
            let target = self.focus.or_else(|| page.doc.find("body")).unwrap_or(Document::ROOT);
            let (key, code) = key_names(ev);
            let go_on = page.dispatch_key(js, target, "keydown", &key, &code, ev.mods.ctrl, ev.mods.shift, ev.mods.alt);
            self.after_scripts();
            if !go_on {
                return true;
            }
        }
        if let Some(node) = self.focus {
            match ev.code {
                KeyCode::Enter => {
                    self.submit(node);
                    return true;
                }
                KeyCode::Escape => {
                    self.focus = None;
                    return true;
                }
                KeyCode::Tab => {}
                _ => {
                    if !self.fields.contains_key(&node) {
                        let v = self.field_value(node);
                        self.fields.insert(node, TextEdit::new(&v, false));
                    }
                    let e = self.fields.get_mut(&node).unwrap();
                    let before = String::from(e.text());
                    let handled = e.handle_key(ev, now).handled;
                    let after = String::from(e.text());
                    if after != before {
                        if let Some(page) = self.page.as_mut() {
                            page.doc.values.insert(node, after);
                            if let Some(js) = self.js.as_mut() {
                                page.dispatch(js, node, "input", true, false);
                                self.after_scripts();
                            }
                        }
                    }
                    return handled;
                }
            }
        }
        if ev.code == KeyCode::Escape && self.loading() {
            self.stop();
            return true;
        }
        self.page_key(ev)
    }

    /// A click in the page, at page-area coordinates.
    fn click(&mut self, x: i32, y: i32) -> bool {
        self.press = Some((x, y));
        let was_editing = core::mem::take(&mut self.editing);
        if was_editing {
            let cur = self.history.get(self.pos).cloned().unwrap_or_default();
            self.address.set_text(&cur);
        }
        let Some((px, py)) = self.page_point(x, y) else { return was_editing };
        let Some(l) = &self.layout else { return was_editing };
        let (field, link, target) = (l.field_at(px, py), l.link_at(px, py), l.node_at(px, py));
        // Scripts see the click first; preventDefault() stops the browser's action.
        if let Some(t) = target.or(field).or(link) {
            if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                let go_on = page.dispatch_click(js, t, px, py);
                self.after_scripts();
                if !go_on {
                    return true;
                }
            }
        }
        if let Some(l) = &self.layout {
            let video = l.items.iter().find_map(|item| match item {
                Item::Video { rect, node } if rect.contains(px, py) => Some((*node, *rect)),
                _ => None,
            });
            if let (Some((node, rect)), Some(page)) = (video, self.page.as_mut()) {
                if page.doc.element(node).is_some_and(|e| e.attr("controls").is_some()) {
                    let duration = self.media.duration(page, node);
                    let s = page.doc.media.entry(node).or_default();
                    if px > rect.x + 70 && py >= rect.bottom() - 32 && duration.is_finite() {
                        s.seek = Some(((px - rect.x) as f64 / rect.w.max(1) as f64 * duration).max(0.0));
                    } else {
                        s.paused = !s.paused;
                        if !s.paused && s.ended {
                            s.seek = Some(0.0);
                            s.ended = false;
                        }
                    }
                    return true;
                }
            }
        }
        if let Some(node) = field {
            let doc = self.doc().unwrap();
            let e = doc.element(node).unwrap();
            let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
            match (e.tag.as_str(), kind.as_str()) {
                ("input", "submit" | "image") | ("button", _) => {
                    if e.tag == "button" && kind == "button" {
                        return was_editing;
                    }
                    self.submit(node);
                }
                ("input", "checkbox" | "radio") => {
                    let checked = e.attr("checked").is_some();
                    let radio = kind == "radio";
                    let name = String::from(e.attr("name").unwrap_or(""));
                    let page = self.page.as_mut().unwrap();
                    if radio {
                        // Uncheck the group.
                        for n in page.doc.find_all("input") {
                            if let nebula_engine::dom::NodeData::Element(el) = &mut page.doc.nodes[n].data {
                                if el.attr("name") == Some(name.as_str())
                                    && el.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"))
                                {
                                    el.attrs.retain(|(k, _)| k != "checked");
                                }
                            }
                        }
                    }
                    if let nebula_engine::dom::NodeData::Element(el) = &mut page.doc.nodes[node].data {
                        if checked && !radio {
                            el.attrs.retain(|(k, _)| k != "checked");
                        } else if !checked {
                            el.attrs.push((String::from("checked"), String::new()));
                        }
                    }
                    self.styles = None;
                    if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                        page.doc.version += 1;
                        page.dispatch(js, node, "input", true, false);
                        page.dispatch(js, node, "change", true, false);
                        self.after_scripts();
                    }
                    self.relayout();
                }
                ("input", "hidden") => {}
                _ => {
                    self.focus = Some(node);
                    if !self.fields.contains_key(&node) {
                        let v = self.field_value(node);
                        self.fields.insert(node, TextEdit::new(&v, false));
                    }
                    if let Some(e) = self.fields.get_mut(&node) {
                        let end = e.text().len();
                        e.set_caret(end, false);
                    }
                }
            }
            return true;
        }
        self.focus = None;
        if let Some(link) = link {
            self.follow(link);
            return true;
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        let link = self.page_point(x, y).and_then(|(px, py)| self.layout.as_ref()?.link_at(px, py));
        let text = link.and_then(|l| self.href(l));
        if text != self.hover {
            self.hover = text;
            return true;
        }
        false
    }

    /// Scrolls by `dy` pixels (down positive).
    fn scroll(&mut self, dy: i32) -> bool {
        let before = self.scroll;
        self.scroll += dy;
        self.clamp_scroll();
        self.scroll != before
    }

    fn tick(&mut self, now: u64) -> bool {
        let mut dirty = false;
        save_cookies();
        if !self.awaiting_page {
            if let (Some(page), Some(url)) = (&mut self.page, &self.url) {
                let events = self.media.tick(page, url, now, &mut self.workers);
                dirty |= !events.is_empty() || self.media.take_presented();
                if let Some(js) = &mut self.js {
                    for (node, kind) in events {
                        page.dispatch(js, node, kind, false, false);
                    }
                }
            }
        }
        if dirty {
            self.after_scripts();
        }
        // Scripts that scripts inserted, now loaded.
        let loaded: Vec<ScriptLoad> = core::mem::take(&mut *self.script_loads.lock());
        if !loaded.is_empty() {
            let g = self.generation.load(Ordering::Relaxed);
            for (gen, node, url, source) in loaded {
                self.scripts_loading = self.scripts_loading.saturating_sub(1);
                if gen != g {
                    continue;
                }
                if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                    page.run_fetched(js, node, &url, source.as_deref());
                }
                self.after_scripts();
                dirty = true;
            }
        }
        // Answers to fetch() and XMLHttpRequest.
        let answers: Vec<_> = core::mem::take(&mut *self.request_results.lock());
        if !answers.is_empty() {
            let g = self.generation.load(Ordering::Relaxed);
            for (gen, id, result) in answers {
                self.scripts_loading = self.scripts_loading.saturating_sub(1);
                if gen != g {
                    continue;
                }
                if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                    page.complete_request(js, id, result);
                }
                self.after_scripts();
                dirty = true;
            }
        }
        // localStorage changes from other tabs on this site.
        if let Some(url) = self.url.clone() {
            let (seq, changes) = self.storage.borrow().since(self.storage_seq, &url, self.id);
            self.storage_seq = seq;
            if !changes.is_empty() {
                let contents = self.storage.borrow_mut().get(&url);
                if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                    for (from, c) in changes {
                        page.storage_event(js, contents.clone(), &c, &from);
                    }
                    self.after_scripts();
                    dirty = true;
                }
            }
        }
        // Page timers and animation frames.
        if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
            let now = corekit::time::unix_ms();
            // Timers and animation frames wait while the page's own scripts
            // are still starting (or a new page is on its way).
            let throttled = (self.background && now - self.timers_ran < 1000.0)
                || self.awaiting_page
                || !self.script_queue.is_empty();
            if self.js_wakeup.is_some_and(|w| w <= now) && !throttled {
                self.timers_ran = now;
                page.run_timers(js, now);
                self.after_scripts();
                dirty = true;
            }
            if self.js_wakeup.is_none() && !self.awaiting_page {
                self.js_wakeup = self.js.as_mut().and_then(|j| j.next_wakeup(now));
            }
        } else {
            self.js_wakeup = None;
        }
        // A finished page load.
        // The page and its styles first, then (on the same job) its scripts.
        let done = self.job.as_ref().and_then(|(job, _)| job.result.lock().take());
        if let Some(result) = done {
            let target = self.history.get(self.pos).cloned().unwrap_or_default();
            match result {
                Ok(loaded) => {
                    let pending = loaded.pending;
                    self.install(loaded);
                    if !pending {
                        self.finish_job();
                    }
                }
                Err(msg) => {
                    self.finish_job();
                    self.show_error(&target, &msg);
                }
            }
            dirty = true;
        }
        if !self.script_queue.is_empty() && !self.awaiting_page {
            self.run_scripts_slice();
            // Repaint for script progress a few times a second, not every slice.
            if self.script_queue.is_empty() || now - self.slice_painted >= 250 {
                self.slice_painted = now;
                dirty = true;
            }
        }
        let scripts = self.job.as_ref().and_then(|(job, _)| job.scripts.lock().take());
        if let Some((scripts, arrived)) = scripts {
            self.finish_job();
            self.start_scripts(scripts, arrived);
            dirty = true;
        }
        // Finished images.
        let finished: Vec<(u64, String, Option<Image>)> = core::mem::take(&mut self.queue.lock().done);
        if !finished.is_empty() {
            let g = self.generation.load(Ordering::Relaxed);
            let mut any = false;
            let page_url = self.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
            let mut scripts_ran = false;
            for (gen, url, img) in finished {
                if gen == g {
                    any |= img.is_some();
                    // Scripts hear about it: load or error, and canvases may draw it
                    // (once the page's own scripts have all run).
                    if self.script_run.is_some() {
                        self.deferred_images.push((url.clone(), img.as_ref().map(|i| (i.width, i.height))));
                    } else if let (Some(page), Some(js)) = (self.page.as_mut(), self.js.as_mut()) {
                        let size = img.as_ref().map(|i| (i.width, i.height));
                        page.image_loaded(js, &page_url, &url, size);
                        if let (true, Some(i)) = (self.canvas_images, &img) {
                            js.set_image(&url, i.width, i.height, &i.pixels);
                        }
                        scripts_ran = true;
                    }
                    self.images.insert(url, img);
                }
            }
            if scripts_ran {
                self.after_scripts();
                dirty = true;
            }
            if any && self.script_run.is_some() {
                // One layout once the scripts are done instead.
                self.images_during_scripts = true;
            } else if any && self.relayout_at.is_none() {
                self.relayout_at = Some(now + 200);
            }
        }
        if let Some(since) = self.images_since {
            let idle = self.queue.lock().queue.is_empty() && self.workers.iter().all(|w| w.is_finished());
            if idle && self.requested.iter().all(|u| self.images.contains_key(u)) {
                self.images_since = None;
                let loaded = self.images.values().filter(|i| i.is_some()).count();
                corekit::io::log(&format!(
                    "[nebula] images {} done={} of {} total={}\n",
                    self.url.as_ref().map(|u| u.to_string()).unwrap_or_default(),
                    loaded,
                    self.requested.len(),
                    corekit::time::uptime_ms() - since
                ));
            }
        }
        if self.relayout_at.is_some_and(|t| now >= t) && !self.awaiting_page {
            self.relayout_at = None;
            self.relayout();
            dirty = true;
        }
        reap(&mut self.old_jobs);
        reap(&mut self.workers);
        dirty | self.loading()
    }

    fn tick_interval(&self) -> u64 {
        if !self.script_queue.is_empty() {
            return if self.background { 250 } else { 1 };
        }
        let busy = self.loading() || self.relayout_at.is_some() || self.scripts_loading > 0;
        if self.background {
            return if busy || !self.workers.is_empty() {
                250
            } else if self.js_wakeup.is_some() {
                1000
            } else {
                // Storage events from other tabs.
                2000
            };
        }
        let base = if self.media.active() {
            16
        } else if busy || !self.workers.is_empty() {
            50
        } else {
            1000
        };
        match self.js_wakeup {
            // Wake for the next timer (16 ms at the most often, for animations).
            Some(w) => ((w - corekit::time::unix_ms()).max(16.0) as u64).min(base),
            None => base,
        }
    }
}

/// KeyboardEvent `key` and `code` for a key press.
fn key_names(ev: &KeyEvent) -> (String, String) {
    let named = match ev.code {
        KeyCode::Enter => Some(("Enter", "Enter")),
        KeyCode::Escape => Some(("Escape", "Escape")),
        KeyCode::Backspace => Some(("Backspace", "Backspace")),
        KeyCode::Tab => Some(("Tab", "Tab")),
        KeyCode::Up => Some(("ArrowUp", "ArrowUp")),
        KeyCode::Down => Some(("ArrowDown", "ArrowDown")),
        KeyCode::Left => Some(("ArrowLeft", "ArrowLeft")),
        KeyCode::Right => Some(("ArrowRight", "ArrowRight")),
        KeyCode::Home => Some(("Home", "Home")),
        KeyCode::End => Some(("End", "End")),
        KeyCode::PageUp => Some(("PageUp", "PageUp")),
        KeyCode::PageDown => Some(("PageDown", "PageDown")),
        KeyCode::Delete => Some(("Delete", "Delete")),
        _ => None,
    };
    if let Some((k, c)) = named {
        return (String::from(k), String::from(c));
    }
    match ev.ch {
        Some(' ') => (String::from(" "), String::from("Space")),
        Some(ch) => {
            let code = if ch.is_ascii_alphabetic() {
                format!("Key{}", ch.to_ascii_uppercase())
            } else if ch.is_ascii_digit() {
                format!("Digit{ch}")
            } else {
                String::new()
            };
            (ch.to_string(), code)
        }
        None => (String::from("Unidentified"), String::new()),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// application/x-www-form-urlencoded.
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The start page, in the colours of the desktop.
fn home_page() -> String {
    let t = theme::current();
    let hex = |c: u32| format!("#{:06x}", c & 0xFF_FFFF);
    let (bg, card, line, hero) = if t.dark {
        ("#141724", "#1d2131", "#2a2f42", "#0c0f1a")
    } else {
        ("#fbfcff", "#ffffff", "#e3e7f2", "#eef1fb")
    };
    let vars = format!(
        ":root {{ --ink: {}; --muted: {}; --accent: {}; --bg: {bg}; --card: {card}; --line: {line}; --hero: {hero} }}",
        hex(t.text),
        hex(t.text_secondary),
        hex(theme::accent())
    );
    HOME_PAGE.replacen(":root {}", &vars, 1)
}

const HOME_PAGE: &str = r#"<!doctype html><html><head><title>Start Page</title><style>
:root {}
body { margin: 0; font-family: sans-serif; background: var(--bg); color: var(--ink) }
main { max-width: 1100px; margin: 0 auto; padding: 14px 22px 30px }
.hero { background: var(--hero); border-radius: 18px; padding: 52px 48px 56px; margin-bottom: 18px }
.eyebrow { color: var(--accent); font-size: 12px; font-weight: bold; margin: 0 0 16px }
h1 { font-size: 46px; margin: 0 0 12px; color: var(--ink) }
.tag { color: var(--ink); font-size: 16px; margin: 0 0 26px; line-height: 1.5 }
form { display: flex; gap: 10px; max-width: 640px }
input[type=search] { flex-grow: 1; width: 480px; max-width: 100%; box-sizing: border-box; font-size: 15px; padding: 11px 20px; border: 1px solid var(--line); border-radius: 22px; background: var(--card); color: var(--ink) }
button { font-size: 14px; font-weight: bold; padding: 11px 22px; border-radius: 22px; border: 0; background: var(--accent); color: #fff }
.grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px }
.card { display: block; padding: 18px 16px; background: var(--card); border-radius: 16px; border: 1px solid var(--line); text-decoration: none; color: var(--ink) }
.card b { display: block; font-size: 22px; margin-bottom: 8px; color: var(--accent) }
.card span { color: var(--muted); font-size: 13px }
.tools { display: flex; gap: 10px; margin-top: 18px }
.tools a { color: var(--ink); font-size: 13px; text-decoration: none; padding: 9px 16px; background: var(--card); border: 1px solid var(--line); border-radius: 18px }
.foot { color: var(--muted); font-size: 12px; margin: 26px 0 0; line-height: 1.8 }
@media (max-width: 760px) { .grid { grid-template-columns: repeat(2, 1fr) } .hero { padding: 32px 26px } h1 { font-size: 34px } }
</style></head><body><main>
<div class="hero"><p class="eyebrow">NEBULA · WAVEOS AURORA</p>
<h1>A little space to explore.</h1><p class="tag">Search, discover, and keep your favourite pages close.</p>
<form action="https://html.duckduckgo.com/html/" method="get"><input type="search" name="q" placeholder="Search the web or use the address bar"><button type="submit">Search</button></form></div>
<div class="grid">
<a class="card" href="https://en.wikipedia.org/wiki/Main_Page"><b>Wikipedia</b><span>A world of knowledge</span></a>
<a class="card" href="https://news.ycombinator.com/"><b>Hacker News</b><span>Ideas worth discussing</span></a>
<a class="card" href="https://text.npr.org/"><b>NPR</b><span>The news, simply</span></a>
<a class="card" href="https://lite.cnn.com/"><b>CNN Lite</b><span>Headlines at a glance</span></a>
<a class="card" href="https://www.rust-lang.org/"><b>Rust</b><span>Build something new</span></a>
<a class="card" href="https://github.com/Sw3bbl3/waveos-aurora-rs"><b>WaveOS Aurora</b><span>Explore your operating system</span></a>
</div><div class="tools"><a href="file:///System/Library/Nebula/pulsar-demo.html">Pulsar playground</a><a href="file:///System/Library/Nebula/css-demo.html">Layout gallery</a><a href="file:///System/Library/Nebula/canvas-demo.html">Canvas gallery</a><a href="file:///System/Library/Nebula/browser-apis.html">Browser checks</a></div>
<p class="foot">Ctrl+T New tab &nbsp; · &nbsp; Ctrl+Tab Switch tabs &nbsp; · &nbsp; Ctrl+Shift+T Reopen a closed page<br>Ctrl-click a link to open it behind. Background tabs keep loading, with their timers slowed to once a second.</p>
</main></body></html>"#;

fn main(args: corekit::Args) -> i32 {
    if args.get(1).is_some_and(|s| s == "--self-check") {
        return checks::self_check();
    }
    let start = match args.get(1) {
        Some(a) if a.starts_with('/') => format!("file://{a}"),
        Some(a) => a.clone(),
        None => String::from(HOME),
    };
    load_cookies();
    waveui::run(Browser::new(&start))
}
